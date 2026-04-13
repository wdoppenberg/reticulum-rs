//! I2PInterface — Reticulum over I2P using the SAM v3.1 bridge.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────────────┐
//! │  I2PInterface::spawn         │
//! │                              │
//! │  SamConn<Init>               │  TCP connect to SAM bridge
//! │    → SamConn<HelloDone>      │  HELLO VERSION exchange
//! │    → SamConn<SessionReady>   │  SESSION CREATE STYLE=STREAM
//! │                              │
//! │  loop:                       │
//! │    session.stream_connect()  │  new TCP conn → STREAM CONNECT
//! │    │ → SamConn<DataPipe>     │  HDLC-framed Reticulum packets
//! │    run_data_loop(pipe)       │
//! └──────────────────────────────┘
//! ```
//!
//! ## Type-driven patterns used
//!
//! | Pattern | Where |
//! |---------|-------|
//! | Async Type-State (#16) | `SamConn<S>` — transitions consume `self` |
//! | Session Types (#18) | Protocol steps enforced by type; wrong order is a compile error |
//! | RAII / Drop (#20) | `TcpStream`'s own `Drop` closes the connection |
//! | Error Type Hierarchy (#21) | `SamError` with distinct variants |
//! | Sealed Traits (#10) | `sam_state::Sealed` prevents external state impls |

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::iface::RxMessage;
use reticulum_core::buffer::{InputBuffer, OutputBuffer};
use reticulum_core::packet::Packet;
use reticulum_core::serde::Serialize;

use super::hdlc::Hdlc;
use super::{Interface, InterfaceContext};

// ── Timing constants (matching Python I2PInterface) ───────────────────────────

pub const SAM_DEFAULT_HOST: &str = "127.0.0.1";
pub const SAM_DEFAULT_PORT: u16 = 7656;

const PROBE_AFTER: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(100);
const RECONNECT_WAIT: Duration = Duration::from_secs(15);

const HDLC_FLAG: u8 = 0x7E;

/// MTU matching Python's `HW_MTU = 1064`.
pub const HW_MTU: usize = 1064;

// ── SAM error hierarchy (Pattern #21) ────────────────────────────────────────

#[derive(Debug)]
pub enum SamError {
    Io(std::io::Error),
    /// SAM bridge returned a non-OK result or unexpected response.
    Protocol(String),
    /// Connection to the SAM bridge was refused.
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

// ── SAM state markers — sealed so only this module can define states ──────────

mod sam_state {
    /// Sealed marker trait.  Prevents external code from inventing new states
    /// (Pattern #10 — Sealed Traits).
    pub trait Sealed: Send + Unpin + 'static {}

    /// TCP connection established; no protocol exchange yet.
    pub struct Init;
    /// `HELLO VERSION` exchange complete.
    pub struct HelloDone;
    /// `SESSION CREATE STYLE=STREAM` complete; session is live.
    pub struct SessionReady {
        pub session_id: String,
        pub our_destination: String,
    }
    /// `STREAM CONNECT` or `STREAM ACCEPT` acknowledged; raw bytes are
    /// Reticulum data (HDLC-framed).
    pub struct DataPipe;

    impl Sealed for Init {}
    impl Sealed for HelloDone {}
    impl Sealed for SessionReady {}
    impl Sealed for DataPipe {}
}

// ── SamConn<S> — the typed SAM connection ────────────────────────────────────

/// A connection to the SAM bridge in state `S`.
///
/// **Async Type-State (Pattern #16)**: each transition method consumes `self`
/// and returns `SamConn` in the *next* state.  Calling a method that belongs
/// to a different state is a compile error.
///
/// **RAII (Pattern #20)**: the inner `TcpStream` is closed when this value is
/// dropped (via `TcpStream`'s own `Drop`), whether normally or via a panic /
/// early return.  No explicit `Drop` impl is needed — and adding one would
/// prevent field-moves during type-state transitions.
struct SamConn<S: sam_state::Sealed> {
    stream: TcpStream,
    sam_addr: String,
    state: S,
}

// ── Init → HelloDone ──────────────────────────────────────────────────────────

impl SamConn<sam_state::Init> {
    /// Open a TCP connection to the SAM bridge.
    async fn connect(sam_addr: &str) -> Result<Self, SamError> {
        let stream = TcpStream::connect(sam_addr).await?;
        Ok(Self {
            stream,
            sam_addr: sam_addr.to_owned(),
            state: sam_state::Init,
        })
    }

    /// Send `HELLO VERSION MIN=3.1 MAX=3.1` and wait for `RESULT=OK`.
    ///
    /// Consumes `self`; returns `SamConn<HelloDone>` on success.
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

// ── HelloDone → SessionReady ──────────────────────────────────────────────────

impl SamConn<sam_state::HelloDone> {
    /// Send `SESSION CREATE STYLE=STREAM` and parse the assigned destination.
    ///
    /// `destination = None` requests a transient (ephemeral) identity from SAM.
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

// ── SessionReady: open data pipes ─────────────────────────────────────────────

impl SamConn<sam_state::SessionReady> {
    /// Open a **new** SAM connection and issue `STREAM CONNECT` to the given
    /// remote I2P destination.
    ///
    /// Borrows `self` so the session control connection remains alive while
    /// data flows on the returned `SamConn<DataPipe>`.
    async fn stream_connect(
        &self,
        remote_dest: &str,
    ) -> Result<SamConn<sam_state::DataPipe>, SamError> {
        // SAM STREAM protocol requires a fresh TCP connection for each stream.
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

    /// Open a **new** SAM connection and issue `STREAM ACCEPT` to receive the
    /// next incoming stream on this session.
    async fn stream_accept(&self) -> Result<SamConn<sam_state::DataPipe>, SamError> {
        let mut pipe = SamConn::connect(&self.sam_addr).await?.hello().await?;

        let cmd = format!("STREAM ACCEPT ID={} SILENT=false\n", self.state.session_id);
        pipe.stream.write_all(cmd.as_bytes()).await?;
        pipe.stream.flush().await?;

        let reply = read_line(&mut pipe.stream).await?;
        if !reply.contains("RESULT=OK") {
            return Err(SamError::Protocol(format!("STREAM ACCEPT failed: {reply}")));
        }

        // After RESULT=OK the next line from the SAM bridge is the remote
        // destination address; read and discard it (logged for diagnostics).
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

    pub fn our_destination(&self) -> &str {
        &self.state.our_destination
    }
}

// ── DataPipe: HDLC-framed I/O + keepalive ─────────────────────────────────────

impl SamConn<sam_state::DataPipe> {
    /// Send an HDLC-encoded Reticulum packet over the data pipe.
    async fn send_packet(&mut self, data: &[u8]) -> Result<(), SamError> {
        let mut enc = [0u8; HW_MTU * 2 + 4];
        let mut out = OutputBuffer::new(&mut enc);
        Hdlc::encode(data, &mut out)
            .map_err(|_| SamError::Protocol("HDLC encode failed".into()))?;
        self.stream.write_all(out.as_slice()).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Send two consecutive HDLC FLAG bytes as a keepalive probe.
    async fn send_keepalive(&mut self) -> Result<(), SamError> {
        self.stream.write_all(&[HDLC_FLAG, HDLC_FLAG]).await?;
        self.stream.flush().await?;
        Ok(())
    }
}

// ── Tunnel state (runtime — changes based on I/O timing) ─────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum TunnelState {
    Init,   // pipeline set up; no data yet
    Active, // data has flowed recently
    Stale,  // nothing received for > PROBE_AFTER * 2
}

// ── Data I/O loop ─────────────────────────────────────────────────────────────

/// Drive send/receive on a `SamConn<DataPipe>` until the connection drops,
/// READ_TIMEOUT fires, or the cancellation token is triggered.
async fn run_data_loop(
    mut pipe: SamConn<sam_state::DataPipe>,
    iface_addr: reticulum_core::hash::AddressHash,
    rx_send: crate::iface::InterfaceRxSender,
    tx_recv: Arc<Mutex<crate::iface::InterfaceTxReceiver>>,
    cancel: CancellationToken,
) {
    let mut state = TunnelState::Init;
    let mut last_read = Instant::now();
    let mut last_write = Instant::now();

    // Rolling receive buffer for HDLC frame reassembly.
    const BUF: usize = HW_MTU * 2 + 4;
    let mut rx_buf = [0u8; BUF];
    let mut hdlc_buf = [0u8; HW_MTU + 4];
    let mut rx_window = [0u8; BUF + BUF / 2];
    let mut rx_pos: usize = 0;

    let mut watchdog = tokio::time::interval(Duration::from_secs(1));
    let mut tx_lock = tx_recv.lock().await;

    loop {
        tokio::select! {
            biased;

            _ = cancel.cancelled() => break,

            // ── Watchdog tick ──────────────────────────────────────────────
            _ = watchdog.tick() => {
                let since_read  = last_read.elapsed();
                let since_write = last_write.elapsed();

                state = if since_read > PROBE_AFTER * 2 {
                    TunnelState::Stale
                } else {
                    TunnelState::Active
                };

                if since_read > READ_TIMEOUT {
                    log::warn!("i2p: read timeout; closing data pipe");
                    break;
                }
                if since_write > PROBE_AFTER {
                    if let Err(e) = pipe.send_keepalive().await {
                        log::warn!("i2p: keepalive failed: {e}");
                        break;
                    }
                    last_write = Instant::now();
                    log::debug!("i2p: keepalive sent (state={state:?})");
                }
            }

            // ── Receive path ───────────────────────────────────────────────
            result = pipe.stream.readable() => {
                if result.is_err() { break; }

                match pipe.stream.try_read(&mut rx_buf) {
                    Ok(0) => {
                        log::info!("i2p: remote closed data pipe");
                        break;
                    }
                    Ok(n) => {
                        last_read = Instant::now();

                        // Append to the sliding window buffer.
                        let avail = rx_window.len() - rx_pos;
                        let copy = n.min(avail);
                        rx_window[rx_pos..rx_pos + copy].copy_from_slice(&rx_buf[..copy]);
                        rx_pos += copy;

                        // Extract complete HDLC frames.
                        let mut consumed = 0usize;
                        loop {
                            if let Some((start, end)) = Hdlc::find(&rx_window[consumed..rx_pos]) {
                                let frame_slice = &mut rx_window[consumed + start..consumed + end + 1].to_vec();
                                let mut out = OutputBuffer::new(&mut hdlc_buf);
                                if Hdlc::decode(frame_slice, &mut out).is_ok() {
                                    let payload = out.as_slice();
                                    // Empty frames are keepalives — ignore.
                                    if !payload.is_empty() {
                                        if let Ok(pkt) = Packet::deserialize(
                                            &mut InputBuffer::new(payload)
                                        ) {
                                            let _ = rx_send
                                                .send(RxMessage {
                                                    address: iface_addr,
                                                    packet: pkt,
                                                })
                                                .await;
                                        }
                                    }
                                }
                                consumed += end + 1;
                            } else {
                                break;
                            }
                        }
                        // Slide the window: move unprocessed bytes to front.
                        rx_window.copy_within(consumed..rx_pos, 0);
                        rx_pos -= consumed;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => {
                        log::warn!("i2p: data rx error: {e}");
                        break;
                    }
                }
            }

            // ── Transmit path ──────────────────────────────────────────────
            Some(msg) = tx_lock.recv() => {
                let mut raw = [0u8; HW_MTU];
                let mut out = OutputBuffer::new(&mut raw);
                if msg.packet.serialize(&mut out).is_ok() {
                    match pipe.send_packet(out.as_slice()).await {
                        Ok(()) => { last_write = Instant::now(); }
                        Err(e) => {
                            log::warn!("i2p: data tx error: {e}");
                            break;
                        }
                    }
                }
            }
        }
    }

    log::debug!("i2p: data loop exited (tunnel_state={state:?})");
}

// ── I2PInterface ──────────────────────────────────────────────────────────────

pub struct I2PInterface {
    sam_host: String,
    sam_port: u16,
    /// `None` = server mode (accept incoming); `Some(dest)` = client mode.
    peer_destination: Option<String>,
}

impl I2PInterface {
    pub fn new(sam_host: String, sam_port: u16, destination: Option<String>) -> Self {
        Self {
            sam_host,
            sam_port,
            peer_destination: destination,
        }
    }

    pub async fn spawn(context: InterfaceContext<Self>) {
        let (sam_host, sam_port, peer_dest) = {
            let g = context.inner.lock().unwrap();
            (g.sam_host.clone(), g.sam_port, g.peer_destination.clone())
        };
        let sam_addr = format!("{sam_host}:{sam_port}");
        let iface_addr = context.channel.address;
        let (rx_send, tx_recv) = context.channel.split();
        let tx_recv = Arc::new(Mutex::new(tx_recv));
        let cancel = context.cancel.clone();

        log::info!("i2p: connecting to SAM bridge at {sam_addr}");

        loop {
            if cancel.is_cancelled() {
                break;
            }

            // ── Establish SAM session ─────────────────────────────────────
            let session = match Self::establish_session(&sam_addr, peer_dest.as_deref()).await {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("i2p: session setup failed: {e}; retrying in {RECONNECT_WAIT:?}");
                    tokio::time::sleep(RECONNECT_WAIT).await;
                    continue;
                }
            };

            log::info!("i2p: session ready (our_dest={}…)", {
                let d = session.our_destination();
                &d[..d.len().min(16)]
            });

            // ── Open data pipe based on mode ──────────────────────────────
            match &peer_dest {
                Some(remote) => {
                    // Client mode: connect to the remote I2P destination.
                    match session.stream_connect(remote).await {
                        Ok(pipe) => {
                            run_data_loop(
                                pipe,
                                iface_addr,
                                rx_send.clone(),
                                tx_recv.clone(),
                                cancel.clone(),
                            )
                            .await;
                        }
                        Err(e) => {
                            log::warn!("i2p: STREAM CONNECT failed: {e}");
                        }
                    }
                }
                None => {
                    // Server mode: accept one incoming stream at a time.
                    loop {
                        if cancel.is_cancelled() {
                            return;
                        }
                        match session.stream_accept().await {
                            Ok(pipe) => {
                                run_data_loop(
                                    pipe,
                                    iface_addr,
                                    rx_send.clone(),
                                    tx_recv.clone(),
                                    cancel.clone(),
                                )
                                .await;
                            }
                            Err(e) => {
                                log::warn!("i2p: STREAM ACCEPT failed: {e}; reconnecting");
                                break; // re-establish session
                            }
                        }
                    }
                }
            }

            if cancel.is_cancelled() {
                break;
            }
            log::info!("i2p: reconnecting in {RECONNECT_WAIT:?}");
            tokio::time::sleep(RECONNECT_WAIT).await;
        }

        log::info!("i2p: interface stopped");
    }

    /// Build a `SamConn<SessionReady>` from scratch: connect → hello →
    /// create_session.
    async fn establish_session(
        sam_addr: &str,
        destination: Option<&str>,
    ) -> Result<SamConn<sam_state::SessionReady>, SamError> {
        let session_id = format!("rns_{:08x}", rand::random::<u32>());
        SamConn::connect(sam_addr)
            .await?
            .hello()
            .await?
            .create_session(&session_id, destination)
            .await
    }
}

impl Interface for I2PInterface {
    fn mtu() -> usize {
        HW_MTU
    }
}

// ── SAM reply helpers ─────────────────────────────────────────────────────────

/// Read a `\n`-terminated line from `stream`.
async fn read_line(stream: &mut TcpStream) -> Result<String, SamError> {
    // Wrap in a BufReader for line reading; we rebuild it each call to avoid
    // consuming the stream.
    let (read_half, _) = stream.split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    Ok(line.trim_end().to_owned())
}

/// Extract `KEY=VALUE` from a SAM reply string.
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
    fn extract_field_last_in_line() {
        let reply = "HELLO REPLY RESULT=OK VERSION=3.1";
        assert_eq!(extract_field(reply, "VERSION"), Some("3.1"));
    }

    #[test]
    fn sam_error_display() {
        let e = SamError::Protocol("bad result".into());
        assert!(e.to_string().contains("bad result"));
        let e2 = SamError::Refused;
        assert!(e2.to_string().contains("refused"));
    }
}
