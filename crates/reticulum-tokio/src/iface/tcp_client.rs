use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use reticulum_core::buffer::OutputBuffer;

use super::hdlc::Hdlc;
use crate::iface::TokioInterface;

const PACKET_TRACE: bool = false;
/// Receive window capacity.  Large enough to hold several packets worth of
/// HDLC-framed data before a complete frame is found.
const RX_WINDOW: usize = 8192;

pub struct TcpClient {
    addr: String,
    stream: TcpStream,
    /// Sliding byte-accumulation window for HDLC frame detection.
    rx_window: Vec<u8>,
    /// Number of valid bytes at the start of `rx_window`.
    rx_pos: usize,
}

impl TcpClient {
    /// Connect to `addr` and return a ready interface.
    pub async fn connect(addr: &str) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        log::info!("tcp_client: connected to <{addr}>");
        Ok(Self::from_stream(addr.to_string(), stream))
    }

    /// Construct from an already-established `TcpStream` (used by `TcpServer`
    /// when it accepts an inbound connection).
    pub fn from_stream(addr: String, stream: TcpStream) -> Self {
        Self {
            addr,
            stream,
            rx_window: vec![0u8; RX_WINDOW],
            rx_pos: 0,
        }
    }
}

impl TokioInterface for TcpClient {
    type Error = std::io::Error;

    async fn transmit(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        let mut hdlc_buf = vec![0u8; frame.len() * 2 + 4];
        let mut output = OutputBuffer::new(&mut hdlc_buf);
        Hdlc::encode(frame, &mut output).map_err(|_| std::io::Error::other("HDLC encode error"))?;
        if PACKET_TRACE {
            log::trace!("tcp_client {}: tx >> {} bytes", self.addr, output.offset());
        }
        self.stream.write_all(output.as_slice()).await?;
        self.stream.flush().await
    }

    async fn receive(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let window_cap = self.rx_window.len();
        loop {
            // ── Check for a complete HDLC frame ───────────────────────────────
            if let Some((start, end)) = Hdlc::find(&self.rx_window[..self.rx_pos]) {
                let frame_end = end + 1;
                let frame_bytes: Vec<u8> = self.rx_window[start..frame_end].to_vec();

                // Consume the frame from the window regardless of decode outcome.
                self.rx_window.copy_within(frame_end..self.rx_pos, 0);
                self.rx_pos -= frame_end;

                let mut decode_buf = vec![0u8; RX_WINDOW];
                let mut out = OutputBuffer::new(&mut decode_buf);
                if Hdlc::decode(&frame_bytes, &mut out).is_ok() {
                    let decoded = out.as_slice();
                    if PACKET_TRACE {
                        log::trace!("tcp_client {}: rx << {} bytes", self.addr, decoded.len());
                    }
                    let n = decoded.len().min(buf.len());
                    buf[..n].copy_from_slice(&decoded[..n]);
                    return Ok(n);
                }
                // Bad frame — discard and look for the next one.
                log::debug!("tcp_client {}: discarded bad HDLC frame", self.addr);
                continue;
            }

            // ── Window full: slide out the oldest half ────────────────────────
            if self.rx_pos >= window_cap {
                let keep_from = window_cap / 2;
                self.rx_window.copy_within(keep_from..self.rx_pos, 0);
                self.rx_pos -= keep_from;
            }

            // ── Read more bytes from the TCP stream ───────────────────────────
            let n = self.stream.read(&mut self.rx_window[self.rx_pos..]).await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("tcp_client {}: connection closed", self.addr),
                ));
            }
            self.rx_pos += n;
        }
    }

    fn mtu(&self) -> usize {
        2048
    }
}
