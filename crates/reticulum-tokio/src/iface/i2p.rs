//! I2PInterface — Reticulum over I2P using the SAM v3.1 bridge.
//!
//! ## Architecture
//!
//! ```text
//! I2PInterface::connect()
//!   SamConn<Init> → HelloDone → SessionReady → DataPipe
//!   Returns I2PInterface which holds the live DataPipe.
//! ```
//!
//! `I2PInterface` implements `reticulum_core::interface::Interface` directly.
//! `receive` reassembles HDLC frames from the SAM TCP stream.
//! `transmit` HDLC-encodes and writes the frame to the SAM stream.
//!
//! If the data pipe dies, `receive` returns `Err`, the generic driver exits, and
//! the interface is removed from the manager.  Reconnection can be implemented at
//! a higher level by spawning a new `I2PInterface` when needed.
//!
//! ## Type-driven patterns
//!
//! | Pattern | Where |
//! |---------|-------|
//! | Async Type-State | `SamConn<S>` |
//! | Session Types    | Protocol steps enforced by type |
//! | RAII / Drop      | `TcpStream`'s own `Drop` closes the connection |
//! | Sealed Traits    | `sam_state::Sealed` |

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use reticulum_core::buffer::OutputBuffer;

use crate::iface::TokioInterface;

use super::hdlc::Hdlc;

// ── Timing constants ──────────────────────────────────────────────────────────

pub const SAM_DEFAULT_HOST: &str = "127.0.0.1";
pub const SAM_DEFAULT_PORT: u16 = 7656;

/// MTU matching Python's `HW_MTU = 1064`.
pub const HW_MTU: usize = 1064;
const RX_WINDOW: usize = HW_MTU * 8;
const HDLC_FLAG: u8 = 0x7E;

// ── SAM error hierarchy ───────────────────────────────────────────────────────

#[derive(Debug)]
pub enum SamError {
    Io(std::io::Error),
    Protocol(String),
    Refused,
}

impl From<std::io::Error> for SamError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::ConnectionRefused {
            SamError::Refused
        } else {
            SamError::Io(e)
        }
    }
}

impl std::fmt::Display for SamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SamError::Io(e) => write!(f, "I/O: {e}"),
            SamError::Protocol(s) => write!(f, "SAM protocol: {s}"),
            SamError::Refused => write!(f, "SAM bridge refused connection"),
        }
    }
}

// ── SAM state markers — sealed ────────────────────────────────────────────────

mod sam_state {
    pub trait Sealed: Send + Unpin + 'static {}

    pub struct Init;
    pub struct HelloDone;
    pub struct SessionReady {
        pub session_id: String,
        #[allow(dead_code)]
        pub our_destination: String,
    }
    pub struct DataPipe;

    impl Sealed for Init {}
    impl Sealed for HelloDone {}
    impl Sealed for SessionReady {}
    impl Sealed for DataPipe {}
}

// ── SamConn<S> ────────────────────────────────────────────────────────────────

struct SamConn<S: sam_state::Sealed> {
    stream: TcpStream,
    sam_addr: String,
    state: S,
}

impl SamConn<sam_state::Init> {
    async fn connect(sam_addr: &str) -> Result<Self, SamError> {
        let stream = TcpStream::connect(sam_addr).await?;
        Ok(Self {
            stream,
            sam_addr: sam_addr.to_owned(),
            state: sam_state::Init,
        })
    }

    async fn hello(mut self) -> Result<SamConn<sam_state::HelloDone>, SamError> {
        self.stream
            .write_all(b"HELLO VERSION MIN=3.1 MAX=3.1\n")
            .await?;
        self.stream.flush().await?;
        let reply = read_line(&mut self.stream).await?;
        if !reply.contains("RESULT=OK") {
            return Err(SamError::Protocol(format!("HELLO failed: {reply}")));
        }
        Ok(SamConn {
            stream: self.stream,
            sam_addr: self.sam_addr,
            state: sam_state::HelloDone,
        })
    }
}

impl SamConn<sam_state::HelloDone> {
    async fn create_session(
        mut self,
        session_id: &str,
        destination: Option<&str>,
    ) -> Result<SamConn<sam_state::SessionReady>, SamError> {
        let dest_field = destination.unwrap_or("TRANSIENT");
        let cmd = format!("SESSION CREATE STYLE=STREAM ID={session_id} DESTINATION={dest_field}\n");
        self.stream.write_all(cmd.as_bytes()).await?;
        self.stream.flush().await?;
        let reply = read_line(&mut self.stream).await?;
        if !reply.contains("RESULT=OK") {
            return Err(SamError::Protocol(format!(
                "SESSION CREATE failed: {reply}"
            )));
        }
        let our_destination = extract_field(&reply, "DESTINATION")
            .ok_or_else(|| {
                SamError::Protocol(format!("SESSION CREATE reply missing DESTINATION: {reply}"))
            })?
            .to_owned();
        log::info!(
            "i2p: session '{session_id}' created; destination={}…",
            &our_destination[..our_destination.len().min(16)]
        );
        Ok(SamConn {
            stream: self.stream,
            sam_addr: self.sam_addr,
            state: sam_state::SessionReady {
                session_id: session_id.to_owned(),
                our_destination,
            },
        })
    }
}

impl SamConn<sam_state::SessionReady> {
    async fn stream_connect(
        &self,
        remote_dest: &str,
    ) -> Result<SamConn<sam_state::DataPipe>, SamError> {
        let mut pipe = SamConn::connect(&self.sam_addr).await?.hello().await?;
        let cmd = format!(
            "STREAM CONNECT ID={} DESTINATION={remote_dest} SILENT=false\n",
            self.state.session_id
        );
        pipe.stream.write_all(cmd.as_bytes()).await?;
        pipe.stream.flush().await?;
        let reply = read_line(&mut pipe.stream).await?;
        if !reply.contains("RESULT=OK") {
            return Err(SamError::Protocol(format!(
                "STREAM CONNECT failed: {reply}"
            )));
        }
        log::info!(
            "i2p: stream connected to {}…",
            &remote_dest[..remote_dest.len().min(16)]
        );
        Ok(SamConn {
            stream: pipe.stream,
            sam_addr: self.sam_addr.clone(),
            state: sam_state::DataPipe,
        })
    }

    async fn stream_accept(&self) -> Result<SamConn<sam_state::DataPipe>, SamError> {
        let mut pipe = SamConn::connect(&self.sam_addr).await?.hello().await?;
        let cmd = format!("STREAM ACCEPT ID={} SILENT=false\n", self.state.session_id);
        pipe.stream.write_all(cmd.as_bytes()).await?;
        pipe.stream.flush().await?;
        let reply = read_line(&mut pipe.stream).await?;
        if !reply.contains("RESULT=OK") {
            return Err(SamError::Protocol(format!("STREAM ACCEPT failed: {reply}")));
        }
        let remote_dest = read_line(&mut pipe.stream).await?;
        log::info!(
            "i2p: accepted stream from {}…",
            &remote_dest[..remote_dest.len().min(16)]
        );
        Ok(SamConn {
            stream: pipe.stream,
            sam_addr: self.sam_addr.clone(),
            state: sam_state::DataPipe,
        })
    }
}

impl SamConn<sam_state::DataPipe> {
    async fn send_packet(&mut self, data: &[u8]) -> Result<(), SamError> {
        let mut enc = vec![0u8; data.len() * 2 + 4];
        let mut out = OutputBuffer::new(&mut enc);
        Hdlc::encode(data, &mut out)
            .map_err(|_| SamError::Protocol("HDLC encode failed".into()))?;
        self.stream.write_all(out.as_slice()).await?;
        self.stream.flush().await?;
        Ok(())
    }
}

// ── I2PInterface ──────────────────────────────────────────────────────────────

/// A live I2P data connection that implements `reticulum_core::interface::Interface`.
///
/// Constructed via [`I2PInterface::connect`], which performs the full SAM
/// handshake and returns once the data pipe is ready.
pub struct I2PInterface {
    pipe: SamConn<sam_state::DataPipe>,
    rx_window: Vec<u8>,
    rx_pos: usize,
}

impl I2PInterface {
    /// Establish a SAM session and open a data pipe.
    ///
    /// - `peer_destination = None` → server mode (accept the next incoming stream).
    /// - `peer_destination = Some(dest)` → client mode (connect to `dest`).
    pub async fn connect(
        sam_host: &str,
        sam_port: u16,
        peer_destination: Option<&str>,
    ) -> Result<Self, SamError> {
        let sam_addr = format!("{sam_host}:{sam_port}");
        let session_id = format!("rns_{:08x}", rand::random::<u32>());

        let session = SamConn::connect(&sam_addr)
            .await?
            .hello()
            .await?
            .create_session(&session_id, peer_destination)
            .await?;

        let pipe = match peer_destination {
            Some(dest) => session.stream_connect(dest).await?,
            None => session.stream_accept().await?,
        };

        log::info!("i2p: data pipe established");

        Ok(Self {
            pipe,
            rx_window: vec![0u8; RX_WINDOW],
            rx_pos: 0,
        })
    }
}

impl TokioInterface for I2PInterface {
    type Error = SamError;

    async fn transmit(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        self.pipe.send_packet(frame).await
    }

    async fn receive(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let window_cap = self.rx_window.len();
        loop {
            // ── HDLC frame detection ──────────────────────────────────────────
            if let Some((start, end)) = Hdlc::find(&self.rx_window[..self.rx_pos]) {
                let frame_end = end + 1;
                let frame_bytes: Vec<u8> = self.rx_window[start..frame_end].to_vec();

                self.rx_window.copy_within(frame_end..self.rx_pos, 0);
                self.rx_pos -= frame_end;

                let mut decode_buf = vec![0u8; RX_WINDOW];
                let mut out = OutputBuffer::new(&mut decode_buf);
                if Hdlc::decode(&frame_bytes, &mut out).is_ok() {
                    let decoded = out.as_slice();
                    // Two consecutive HDLC flags = keepalive; skip it.
                    if decoded.is_empty() {
                        continue;
                    }
                    let n = decoded.len().min(buf.len());
                    buf[..n].copy_from_slice(&decoded[..n]);
                    return Ok(n);
                }
                continue; // bad frame
            }

            // ── Slide window if full ──────────────────────────────────────────
            if self.rx_pos >= window_cap {
                let keep_from = window_cap / 2;
                self.rx_window.copy_within(keep_from..self.rx_pos, 0);
                self.rx_pos -= keep_from;
            }

            // ── Read from SAM data pipe ───────────────────────────────────────
            let n = self
                .pipe
                .stream
                .read(&mut self.rx_window[self.rx_pos..])
                .await
                .map_err(SamError::Io)?;
            if n == 0 {
                return Err(SamError::Protocol("i2p data pipe closed".into()));
            }
            self.rx_pos += n;

            // Detect keepalive: two consecutive 0x7E bytes not forming a frame.
            if n == 2
                && self.rx_window[self.rx_pos - 2] == HDLC_FLAG
                && self.rx_window[self.rx_pos - 1] == HDLC_FLAG
            {
                self.rx_pos -= 2; // discard keepalive bytes
            }
        }
    }

    fn mtu(&self) -> usize {
        HW_MTU
    }
}

// ── SAM reply helpers ─────────────────────────────────────────────────────────

async fn read_line(stream: &mut TcpStream) -> Result<String, SamError> {
    let (read_half, _) = stream.split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    Ok(line.trim_end().to_owned())
}

fn extract_field<'a>(reply: &'a str, key: &str) -> Option<&'a str> {
    let search = format!("{key}=");
    let start = reply.find(search.as_str())? + search.len();
    let rest = &reply[start..];
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    Some(&rest[..end])
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_field_from_reply() {
        let reply = "SESSION STATUS RESULT=OK DESTINATION=abc123def456==";
        assert_eq!(extract_field(reply, "RESULT"), Some("OK"));
        assert_eq!(extract_field(reply, "DESTINATION"), Some("abc123def456=="));
        assert_eq!(extract_field(reply, "MISSING"), None);
    }

    #[test]
    fn sam_error_display() {
        let e = SamError::Protocol("bad result".into());
        assert!(e.to_string().contains("bad result"));
        let e2 = SamError::Refused;
        assert!(e2.to_string().contains("refused"));
    }
}
