use tokio::net::UdpSocket;

use crate::iface::TokioInterface;

const PACKET_TRACE: bool = false;

pub struct UdpInterface {
    socket: UdpSocket,
    forward_addr: Option<String>,
}

impl UdpInterface {
    /// Bind a UDP socket to `bind_addr`.
    ///
    /// `forward_addr` is the unicast/broadcast address to send outbound frames
    /// to.  If `None`, `transmit` is a no-op (receive-only interface).
    pub async fn bind(bind_addr: &str, forward_addr: Option<String>) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(bind_addr).await?;
        log::info!("udp_interface: bound to <{bind_addr}>");
        Ok(Self {
            socket,
            forward_addr,
        })
    }
}

impl TokioInterface for UdpInterface {
    type Error = std::io::Error;

    async fn transmit(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        if let Some(ref addr) = self.forward_addr {
            if PACKET_TRACE {
                log::trace!("udp_interface: tx >> {} bytes to {addr}", frame.len());
            }
            self.socket.send_to(frame, addr.as_str()).await?;
        }
        Ok(())
    }

    async fn receive(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let (n, src) = self.socket.recv_from(buf).await?;
        if PACKET_TRACE {
            log::trace!("udp_interface: rx << {n} bytes from {src}");
        }
        Ok(n)
    }

    fn mtu(&self) -> usize {
        2048
    }
}
