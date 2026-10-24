//! Bounded, handshake-only diagnostic. No gateway, egress sockets, or service state.
use crate::{BridgeConfig, tunnel::Tunnel};
use std::{
    net::{SocketAddr, UdpSocket},
    time::Duration,
};

pub async fn handshake(
    config: BridgeConfig,
    socket: UdpSocket,
    timeout: Duration,
) -> Result<(), String> {
    let endpoint: SocketAddr = config
        .endpoint
        .parse()
        .map_err(|_| "Invalid peer endpoint")?;
    let mut tunnel = Tunnel::new(&config).map_err(|_| "Invalid WireGuard keys")?;
    socket
        .set_nonblocking(true)
        .map_err(|_| "Could not prepare Wi-Fi socket")?;
    let socket =
        tokio::net::UdpSocket::from_std(socket).map_err(|_| "Could not prepare Wi-Fi socket")?;
    tokio::time::timeout(timeout, async {
        let initial = tunnel.encapsulate(&[]).map_err(|_| "Could not initiate handshake")?;
        for packet in initial.to_network {
            socket.send_to(&packet, endpoint).await.map_err(|_| "Could not send Wi-Fi handshake")?;
        }
        let mut ticker = tokio::time::interval(Duration::from_millis(100));
        let mut buffer = vec![0; 65535];
        loop {
            let output = tokio::select! {
                received = socket.recv_from(&mut buffer) => {
                    let (n, from) = received.map_err(|_| "Could not receive Wi-Fi handshake")?;
                    if from.ip().to_canonical() != endpoint.ip().to_canonical() || from.port() != endpoint.port() {
                        continue;
                    }
                    tunnel.handle_udp(&buffer[..n])
                }
                _ = ticker.tick() => tunnel.tick(),
            };
            if let Ok(output) = output {
                for packet in output.to_network {
                    socket.send_to(&packet, endpoint).await.map_err(|_| "Could not send Wi-Fi handshake")?;
                }
                // Decrypted payload is deliberately discarded, never forwarded.
                if tunnel.is_connected() { return Ok(()); }
            }
        }
    }).await.map_err(|_| "No authenticated response from peer (connection test timed out)".to_string())?
        .map_err(str::to_owned)
}
