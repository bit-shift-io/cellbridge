//! Maintained, unprivileged router-side verification peer.
mod client;
mod scenario;
use crate::{BridgeConfig, Tunnel, TunnelOutput, build_udp_reply, parse_udp};
use client::{Client, ClientOutput};
pub use scenario::{Scenario, ScenarioResult};
use std::{
    io,
    net::SocketAddr,
    time::{Duration, Instant},
};
use tokio::{net::UdpSocket, sync::mpsc};

pub struct PeerConfig {
    pub private_key: String,
    pub peer_public_key: String,
    pub listen: SocketAddr,
    pub mtu: u16,
}
pub struct TestPeer {
    socket: UdpSocket,
    tunnel: Tunnel,
    endpoint: Option<SocketAddr>,
    timeout: Duration,
    mtu: usize,
    next_port: u16,
}
enum Flow {
    Handshake,
    Tcp(Box<Client>, Vec<u8>, bool),
    Udp {
        destination: SocketAddr,
        port: u16,
        payload: Vec<u8>,
        expected: Vec<u8>,
        dns: bool,
        sent: bool,
    },
}
struct Pending {
    started: Instant,
    flow: Flow,
}

impl TestPeer {
    pub async fn bind(config: PeerConfig) -> Result<Self, String> {
        if !(1280..=1500).contains(&config.mtu) {
            return Err("invalid MTU".into());
        }
        let tunnel = Tunnel::new(&BridgeConfig {
            private_key: config.private_key,
            peer_public_key: config.peer_public_key,
            address: "10.10.10.1/24".into(),
            endpoint: config.listen.to_string(),
            preshared_key: None,
            mtu: config.mtu,
        })
        .map_err(|_| "invalid peer keys".to_string())?;
        let socket = UdpSocket::bind(config.listen)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self {
            socket,
            tunnel,
            endpoint: None,
            timeout: Duration::from_secs(15),
            mtu: config.mtu as usize,
            next_port: 41000,
        })
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn begin(&mut self, scenario: Scenario) -> Result<Pending, String> {
        self.next_port = if self.next_port == 60000 {
            41000
        } else {
            self.next_port + 1
        };
        let port = self.next_port;
        let flow = match scenario {
            Scenario::Fetch {
                destination,
                host,
                path,
            } => {
                if !path.starts_with('/')
                    || host.contains(['\r', '\n'])
                    || path.contains(['\r', '\n'])
                {
                    return Err("invalid HTTP host/path".into());
                }
                let request =
                    format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                        .into_bytes();
                Flow::Tcp(
                    Box::new(Client::new(destination, port, request, self.mtu)?),
                    b"HTTP/1.".to_vec(),
                    true,
                )
            }
            Scenario::Handshake => Flow::Handshake,
            Scenario::Tcp {
                destination,
                request,
                expected,
            } => {
                if request.len() > 1024 * 1024 {
                    return Err("request exceeds 1 MiB".into());
                }
                Flow::Tcp(
                    Box::new(Client::new(destination, port, request, self.mtu)?),
                    expected,
                    false,
                )
            }
            Scenario::Udp {
                destination,
                payload,
                expected,
            } => Flow::Udp {
                destination,
                port,
                payload,
                expected,
                dns: false,
                sent: false,
            },
            Scenario::Dns { resolver, name } => Flow::Udp {
                destination: resolver,
                port,
                payload: scenario::dns_query(&name, port)?,
                expected: vec![],
                dns: true,
                sent: false,
            },
        };
        if let Flow::Udp {
            payload,
            destination,
            ..
        } = &flow
            && (!destination.is_ipv4() || payload.len() + 28 > self.mtu)
        {
            return Err("UDP request exceeds MTU or is not IPv4".into());
        }
        Ok(Pending {
            started: Instant::now(),
            flow,
        })
    }

    async fn send(&self, output: TunnelOutput) {
        if let Some(endpoint) = self.endpoint {
            for datagram in output.to_network {
                let _ = self.socket.send_to(&datagram, endpoint).await;
            }
        }
    }
    async fn encapsulate(&mut self, packet: &[u8]) {
        if let Ok(output) = self.tunnel.encapsulate(packet) {
            self.send(output).await;
        }
    }

    pub async fn run(
        mut self,
        mut commands: mpsc::Receiver<Scenario>,
        results: mpsc::Sender<ScenarioResult>,
    ) {
        let mut ticker = tokio::time::interval(Duration::from_millis(10));
        let mut buffer = vec![0; 65535];
        let mut pending: Option<Pending> = None;
        loop {
            let mut received = vec![];
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    if pending.is_some() {
                        let _ = results.send(ScenarioResult { name: "command".into(), passed: false, elapsed_ms: 0, received_bytes: 0, message: "another check is running".into(), response_text: None }).await;
                    } else { match self.begin(command) {
                        Ok(flow) => pending = Some(flow),
                        Err(message) => { let _ = results.send(ScenarioResult { name: "command".into(), passed: false, elapsed_ms: 0, received_bytes: 0, message, response_text: None }).await; }
                    }}
                }
                packet = self.socket.recv_from(&mut buffer) => {
                    if let Ok((n, from)) = packet
                        && let Ok(output) = self.tunnel.handle_udp(&buffer[..n]) {
                                // A type-2 response is only emitted after authenticating a
                                // type-1 initiation. Done/no-op datagrams prove nothing.
                                let authenticated = !output.to_tunnel.is_empty() || output.to_network.iter().any(|p| p.len() >= 4 && p[..4] == [2,0,0,0]);
                                if authenticated { self.endpoint = Some(from); }
                                if self.endpoint != Some(from) { continue }
                                received = output.to_tunnel.clone();
                                self.send(output).await;
                    }
                }
                _ = ticker.tick() => { if let Ok(output) = self.tunnel.tick() { self.send(output).await; } }
            }
            let Some(active) = pending.as_mut() else {
                continue;
            };
            let name = match active.flow {
                Flow::Handshake => "handshake",
                Flow::Tcp(..) => "tcp",
                Flow::Udp { dns: true, .. } => "dns",
                _ => "udp",
            };
            let mut outcome: Option<(bool, Vec<u8>, String)> = None;
            if active.started.elapsed() >= self.timeout {
                outcome = Some((false, vec![], "check timed out".into()));
            } else if self.tunnel.is_connected() {
                match &mut active.flow {
                    Flow::Handshake => {
                        outcome = Some((true, vec![], "authenticated tunnel established".into()))
                    }
                    Flow::Tcp(client, expected, _) => {
                        for packet in received {
                            client.receive(packet);
                        }
                        match client.poll() {
                            Ok(ClientOutput { packets, response }) => {
                                for packet in packets {
                                    self.encapsulate(&packet).await;
                                }
                                if let Some(response) = response {
                                    let pass = !response.is_empty()
                                        && (expected.is_empty()
                                            || response
                                                .windows(expected.len())
                                                .any(|w| w == expected));
                                    outcome = Some((
                                        pass,
                                        response,
                                        if pass {
                                            "response matched"
                                        } else {
                                            "TCP closed without expected response"
                                        }
                                        .into(),
                                    ));
                                }
                            }
                            Err(error) => outcome = Some((false, vec![], error)),
                        }
                    }
                    Flow::Udp {
                        destination,
                        port,
                        payload,
                        expected,
                        dns,
                        sent,
                    } => {
                        let source: SocketAddr = format!("10.10.10.1:{port}").parse().unwrap();
                        if !*sent {
                            self.encapsulate(&build_udp_reply(source, *destination, payload))
                                .await;
                            *sent = true;
                        }
                        for packet in received {
                            if let Some(reply) = parse_udp(&packet)
                                && reply.src == *destination
                                && reply.dst == source
                            {
                                let pass = if *dns {
                                    scenario::valid_dns(&reply.payload, payload)
                                } else {
                                    reply.payload == *expected
                                };
                                if !*dns || pass {
                                    outcome = Some((
                                        pass,
                                        reply.payload,
                                        if pass {
                                            "response matched"
                                        } else {
                                            "unexpected UDP response"
                                        }
                                        .into(),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            if let Some((passed, response, message)) = outcome {
                let result = ScenarioResult {
                    name: name.into(),
                    passed,
                    elapsed_ms: active.started.elapsed().as_millis() as u64,
                    received_bytes: response.len(),
                    message,
                    response_text: if matches!(active.flow, Flow::Tcp(_, _, true)) {
                        Some(String::from_utf8_lossy(&response).into_owned())
                    } else {
                        None
                    },
                };
                pending = None;
                if results.send(result).await.is_err() {
                    break;
                }
            }
        }
    }
}
