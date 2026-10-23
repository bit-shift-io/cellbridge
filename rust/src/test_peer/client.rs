use crate::QueueDevice;
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    socket::tcp,
    time::Instant,
    wire::{HardwareAddress, IpAddress, IpCidr, IpEndpoint},
};
use std::net::SocketAddr;

pub(super) struct Client {
    device: QueueDevice,
    iface: Interface,
    sockets: SocketSet<'static>,
    handle: SocketHandle,
    request: Vec<u8>,
    sent: usize,
    response: Vec<u8>,
    closing: bool,
}

pub(super) struct ClientOutput {
    pub packets: Vec<Vec<u8>>,
    pub response: Option<Vec<u8>>,
}

impl Client {
    pub fn new(
        destination: SocketAddr,
        port: u16,
        request: Vec<u8>,
        mtu: usize,
    ) -> Result<Self, String> {
        let std::net::IpAddr::V4(ip) = destination.ip() else {
            return Err("IPv4 only".into());
        };
        let mut device = QueueDevice::new(mtu);
        let mut iface = Interface::new(
            Config::new(HardwareAddress::Ip),
            &mut device,
            Instant::now(),
        );
        iface.update_ip_addrs(|ips| {
            ips.push(IpCidr::new(IpAddress::v4(10, 10, 10, 1), 24))
                .unwrap();
        });
        iface
            .routes_mut()
            .add_default_ipv4_route(smoltcp::wire::Ipv4Address::new(10, 10, 10, 2))
            .unwrap();
        let mut sockets = SocketSet::new(vec![]);
        let mut socket = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; 128 * 1024]),
            tcp::SocketBuffer::new(vec![0; 128 * 1024]),
        );
        socket
            .connect(
                iface.context(),
                IpEndpoint::new(IpAddress::from(ip), destination.port()),
                port,
            )
            .map_err(|_| "cannot create TCP client".to_string())?;
        let handle = sockets.add(socket);
        Ok(Self {
            device,
            iface,
            sockets,
            handle,
            request,
            sent: 0,
            response: vec![],
            closing: false,
        })
    }

    pub fn receive(&mut self, packet: Vec<u8>) {
        self.device.push_rx(packet);
    }

    pub fn poll(&mut self) -> Result<ClientOutput, String> {
        let now = Instant::now();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        let socket = self.sockets.get_mut::<tcp::Socket>(self.handle);
        if socket.can_send() && self.sent < self.request.len() {
            self.sent += socket
                .send_slice(&self.request[self.sent..])
                .map_err(|_| "TCP send failed")?;
        }
        if self.sent == self.request.len() && socket.may_send() && !self.closing {
            socket.close();
            self.closing = true;
        }
        let mut buffer = [0; 8192];
        while socket.can_recv() {
            let n = socket
                .recv_slice(&mut buffer)
                .map_err(|_| "TCP receive failed")?;
            self.response.extend_from_slice(&buffer[..n]);
            if self.response.len() > 1024 * 1024 {
                return Err("response exceeds 1 MiB".into());
            }
        }
        let done = matches!(
            socket.state(),
            tcp::State::Closed | tcp::State::TimeWait | tcp::State::CloseWait | tcp::State::LastAck
        );
        let response = done.then(|| std::mem::take(&mut self.response));
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        Ok(ClientOutput {
            packets: self.device.drain_tx(),
            response,
        })
    }
}
