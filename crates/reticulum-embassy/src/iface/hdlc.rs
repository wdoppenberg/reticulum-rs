//! HDLC-framed interface adapter.
//!
//! [`HdlcInterface`] wraps any `embedded-io-async` `Read + Write` transport
//! (UART, USB-CDC, …) and presents it as a Reticulum [`Interface`] by
//! accumulating raw bytes, extracting complete HDLC frames on receive, and
//! HDLC-encoding frames on transmit.
//!
//! This is the primary adapter for **RNode** serial connections — the LoRa
//! serial protocol used by Reticulum's reference hardware.
//!
//! # Memory layout
//!
//! `HdlcInterface<T, N>` stores an accumulation buffer of `N` bytes as part of
//! its own struct.  No heap allocation.  Choose `N` large enough to hold at
//! least one maximum-length HDLC-encoded frame:
//!
//! ```text
//! N_min = 2 + 2 * max_payload_bytes
//! ```
//!
//! For LoRa (SX1262, 255-byte max payload):
//!
//! ```text
//! N_min = 2 + 2 * 255 = 512
//! ```
//!
//! The MTU reported by [`Interface::mtu`] is derived as `(N - 2) / 2`.
//!
//! # Errors
//!
//! [`HdlcError`] wraps both I/O errors from the inner transport and HDLC
//! framing errors (malformed frames, buffer overflows).  Framing errors are
//! non-fatal: the accumulation buffer is cleared and reception continues.
//!
//! # Example
//!
//! ```rust,ignore
//! use reticulum_embassy::iface::hdlc::HdlcInterface;
//! use reticulum_embassy::iface::{RxMessage, TxMessage, drive_interface};
//! use embassy_sync::channel::Channel;
//! use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
//!
//! // 512-byte accumulation buffer → 255-byte decoded MTU (LoRa)
//! type LoraHdlc = HdlcInterface<UartType, 512>;
//!
//! static RX: Channel<CriticalSectionRawMutex, RxMessage, 4> = Channel::new();
//! static TX: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();
//!
//! #[embassy_executor::task]
//! async fn rnode_task(uart: UartType, addr: AddressHash) {
//!     let iface = HdlcInterface::<_, 512>::new(uart);
//!     drive_interface::<_, 512>(iface, addr, RX.sender().into(), TX.receiver().into()).await;
//! }
//! ```
//!
//! [`Interface`]: reticulum_core::interface::Interface
//! [`Interface::mtu`]: reticulum_core::interface::Interface::mtu

use embedded_io_async::{ErrorType, Read, Write};

use reticulum_core::buffer::OutputBuffer;
use reticulum_core::error::RnsError;
use reticulum_core::interface::Interface;

use reticulum_core::hdlc::Hdlc;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can arise from [`HdlcInterface`].
#[derive(Debug)]
pub enum HdlcError<E> {
    /// The underlying I/O transport returned an error.
    Io(E),
    /// HDLC framing error (malformed frame, decode failure, buffer overflow).
    Hdlc(RnsError),
}

impl<E: core::fmt::Debug> core::fmt::Display for HdlcError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HdlcError::Io(e) => write!(f, "I/O error: {e:?}"),
            HdlcError::Hdlc(e) => write!(f, "HDLC error: {e:?}"),
        }
    }
}

// ── HdlcInterface ─────────────────────────────────────────────────────────────

/// An [`Interface`] adapter that adds HDLC framing over any byte-stream
/// `embedded-io-async` transport.
///
/// `N` is the size of the internal accumulation buffer in bytes.  See the
/// [module documentation](self) for sizing guidance.
///
/// [`Interface`]: reticulum_core::interface::Interface
pub struct HdlcInterface<T, const N: usize> {
    inner: T,
    /// Raw-byte accumulation buffer — bytes are appended on each `read`, and
    /// consumed left-to-right as complete HDLC frames are decoded.
    accum: [u8; N],
    /// Number of valid bytes currently in `accum`.
    accum_len: usize,
}

impl<T, const N: usize> HdlcInterface<T, N> {
    /// Wrap `inner` in an HDLC framing adapter.
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            accum: [0u8; N],
            accum_len: 0,
        }
    }

    /// Returns a reference to the inner transport.
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Returns a mutable reference to the inner transport.
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Unwraps, returning the inner transport.
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Discard all buffered data (e.g. after a detected framing error).
    fn reset_accum(&mut self) {
        self.accum_len = 0;
    }

    /// Consume `n` bytes from the front of the accumulation buffer by shifting
    /// the remainder to the beginning.
    fn consume(&mut self, n: usize) {
        let n = n.min(self.accum_len);
        self.accum.copy_within(n..self.accum_len, 0);
        self.accum_len -= n;
    }

    /// Try to extract one complete HDLC frame from the accumulation buffer into
    /// `out`.
    ///
    /// Returns `Some(decoded_len)` on success, `None` if no complete frame is
    /// available yet.
    fn try_extract_frame(
        &mut self,
        out: &mut OutputBuffer<'_>,
    ) -> Option<Result<usize, HdlcError<T::Error>>>
    where
        T: ErrorType,
    {
        let (start, end) = Hdlc::find(&self.accum[..self.accum_len])?;

        let result = Hdlc::decode(&self.accum[start..=end], out).map_err(HdlcError::Hdlc);

        // Consume everything up to and including the closing flag byte.
        self.consume(end + 1);

        Some(result)
    }
}

impl<T, const N: usize> Interface for HdlcInterface<T, N>
where
    T: Read + Write,
    T::Error: core::fmt::Debug,
{
    type Error = HdlcError<T::Error>;

    /// Receive one complete HDLC-framed Reticulum packet into `buf`.
    ///
    /// Reads raw bytes from the inner transport, accumulates them, and returns
    /// as soon as a complete HDLC frame has been decoded.  Malformed frames are
    /// logged and skipped; reception continues without returning an error.
    async fn receive(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        loop {
            // ── Try to extract from what we already have ───────────────────
            let mut out = OutputBuffer::new(buf);
            if let Some(result) = self.try_extract_frame(&mut out) {
                match result {
                    Ok(n) => return Ok(n),
                    Err(HdlcError::Hdlc(e)) => {
                        log::warn!("HdlcInterface: bad frame, skipping: {e:?}");
                        // accum has already been advanced past the bad frame.
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            // ── Need more bytes ────────────────────────────────────────────
            if self.accum_len == N {
                // Buffer full but no complete frame found — likely a stream
                // that jumped in mid-frame.  Discard everything and re-sync.
                log::warn!(
                    "HdlcInterface: accumulation buffer full ({N} B) with no valid \
                     frame; discarding and re-syncing"
                );
                self.reset_accum();
                continue;
            }

            let free = N - self.accum_len;
            // Guard: read() must be called with a non-empty slice.
            if free == 0 {
                continue;
            }

            let n = self
                .inner
                .read(&mut self.accum[self.accum_len..self.accum_len + free])
                .await
                .map_err(HdlcError::Io)?;

            if n == 0 {
                // Transport returned 0 bytes — yield to the executor to avoid
                // a tight busy loop on a slow/empty stream.
                core::future::poll_fn(|cx| {
                    cx.waker().wake_by_ref();
                    core::task::Poll::<()>::Pending
                })
                .await;
                continue;
            }

            self.accum_len += n;
        }
    }

    /// Transmit a Reticulum frame by HDLC-encoding it and writing it to the
    /// inner transport.
    ///
    /// The encoded frame is built on the stack in a temporary buffer of `N`
    /// bytes (same size as the accumulation buffer).  If the encoded frame
    /// would exceed `N` bytes (i.e. the raw frame is larger than `mtu()`),
    /// the call fails with `HdlcError::Hdlc(OutOfMemory)`.
    async fn transmit(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        let mut backing = [0u8; N];
        let mut enc = OutputBuffer::new(&mut backing);
        Hdlc::encode(frame, &mut enc).map_err(HdlcError::Hdlc)?;

        self.inner
            .write_all(enc.as_slice())
            .await
            .map_err(HdlcError::Io)
    }

    /// Maximum unframed payload size in bytes.
    ///
    /// Derived as `(N - 2) / 2` — the worst-case HDLC overhead is 2 flag bytes
    /// plus one escape byte per payload byte.
    fn mtu(&self) -> usize {
        (N - 2) / 2
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_io_async::{Error, ErrorKind, ErrorType, Read, Write};
    use reticulum_core::hdlc::HDLC_FLAG;

    // ── Mock transport ────────────────────────────────────────────────────────

    #[derive(Debug)]
    struct MockError(ErrorKind);

    impl Error for MockError {
        fn kind(&self) -> ErrorKind {
            self.0
        }
    }

    /// A byte-pipe mock: `rx_data` is what `read()` returns byte-by-byte,
    /// `tx_data` accumulates everything written via `write()`.
    struct MockTransport {
        rx_data: heapless::Vec<u8, 512>,
        rx_pos: usize,
        pub tx_data: heapless::Vec<u8, 512>,
    }

    impl MockTransport {
        fn new(rx: &[u8]) -> Self {
            let mut rx_data = heapless::Vec::new();
            rx_data.extend_from_slice(rx).unwrap();
            Self {
                rx_data,
                rx_pos: 0,
                tx_data: heapless::Vec::new(),
            }
        }

        fn remaining(&self) -> usize {
            self.rx_data.len() - self.rx_pos
        }
    }

    impl ErrorType for MockTransport {
        type Error = MockError;
    }

    impl Read for MockTransport {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            let avail = self.remaining();
            if avail == 0 {
                // Signal EOF — returning 0 will trigger the yield loop in
                // HdlcInterface; tests must not call receive past the last frame.
                return Ok(0);
            }
            let n = buf.len().min(avail).min(32); // return ≤32 bytes at a time
            buf[..n].copy_from_slice(&self.rx_data[self.rx_pos..self.rx_pos + n]);
            self.rx_pos += n;
            Ok(n)
        }
    }

    impl Write for MockTransport {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            self.tx_data.extend_from_slice(buf).unwrap();
            Ok(buf.len())
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn hdlc_frame(payload: &[u8]) -> heapless::Vec<u8, 512> {
        let mut backing = [0u8; 512];
        let mut out = reticulum_core::buffer::OutputBuffer::new(&mut backing);
        Hdlc::encode(payload, &mut out).unwrap();
        let mut v = heapless::Vec::new();
        v.extend_from_slice(out.as_slice()).unwrap();
        v
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn receive_single_frame() {
        let frame = hdlc_frame(b"hello");
        let transport = MockTransport::new(&frame);
        let mut iface = HdlcInterface::<_, 512>::new(transport);

        let mut buf = [0u8; 64];
        let n = pollster::block_on(iface.receive(&mut buf)).unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    #[test]
    fn receive_two_consecutive_frames() {
        let mut stream: heapless::Vec<u8, 512> = heapless::Vec::new();
        stream.extend_from_slice(&hdlc_frame(b"first")).unwrap();
        stream.extend_from_slice(&hdlc_frame(b"second")).unwrap();

        let transport = MockTransport::new(&stream);
        let mut iface = HdlcInterface::<_, 512>::new(transport);
        let mut buf = [0u8; 64];

        let n1 = pollster::block_on(iface.receive(&mut buf)).unwrap();
        assert_eq!(&buf[..n1], b"first");

        let n2 = pollster::block_on(iface.receive(&mut buf)).unwrap();
        assert_eq!(&buf[..n2], b"second");
    }

    #[test]
    fn receive_frame_with_leading_garbage() {
        // Bytes before the first 0x7E should be skipped.
        let mut stream: heapless::Vec<u8, 512> = heapless::Vec::new();
        stream.extend_from_slice(b"junk junk").unwrap(); // no flags
        stream.extend_from_slice(&hdlc_frame(b"payload")).unwrap();

        let transport = MockTransport::new(&stream);
        let mut iface = HdlcInterface::<_, 512>::new(transport);
        let mut buf = [0u8; 64];

        let n = pollster::block_on(iface.receive(&mut buf)).unwrap();
        assert_eq!(&buf[..n], b"payload");
    }

    #[test]
    fn receive_frame_with_escaped_bytes() {
        let payload = [0x00, HDLC_FLAG, 0x7D, 0xFF];
        let frame = hdlc_frame(&payload);
        let transport = MockTransport::new(&frame);
        let mut iface = HdlcInterface::<_, 512>::new(transport);
        let mut buf = [0u8; 64];

        let n = pollster::block_on(iface.receive(&mut buf)).unwrap();
        assert_eq!(&buf[..n], &payload);
    }

    #[test]
    fn transmit_encodes_as_hdlc() {
        let transport = MockTransport::new(&[]);
        let mut iface = HdlcInterface::<_, 512>::new(transport);

        pollster::block_on(iface.transmit(b"world")).unwrap();

        let written = &iface.inner().tx_data;
        // Must start and end with flag bytes.
        assert_eq!(written[0], HDLC_FLAG);
        assert_eq!(*written.last().unwrap(), HDLC_FLAG);

        // Decode what was written and check it matches the original payload.
        let mut backing = [0u8; 64];
        let mut out = OutputBuffer::new(&mut backing);
        Hdlc::decode(written, &mut out).unwrap();
        assert_eq!(out.as_slice(), b"world");
    }

    #[test]
    fn mtu_matches_buffer_size() {
        let transport = MockTransport::new(&[]);
        let iface = HdlcInterface::<_, 512>::new(transport);
        // (512 - 2) / 2 = 255
        assert_eq!(iface.mtu(), 255);
    }
}
