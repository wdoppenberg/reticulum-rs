//! Transport interface abstraction
//!
//! Implement [`Interface`] for your physical or virtual transport layer — a
//! serial port, LoRa radio, UDP socket, etc. — so that a Reticulum runtime
//! (e.g. `reticulum-embassy` or `reticulum-tokio`) can drive it without
//! depending on any specific I/O library.
//!
//! # Two flavours: [`Interface`] and [`SendInterface`]
//!
//! The crate exposes two async traits that describe the same three methods:
//!
//! | Trait | Async flavour | Suitable for |
//! |-------|---------------|--------------|
//! | [`Interface`] | AFIT — futures are opaque, no `Send` bound | Embassy (single-core, `!Send` tasks) |
//! | [`SendInterface`] | RPITIT — futures explicitly `+ Send + 'static` | Tokio multi-threaded scheduler |
//!
//! They are generated from a single source with
//! [`trait_variant::make`](trait_variant).  Every type that implements
//! [`SendInterface`] automatically receives a blanket [`Interface`] impl, so
//! tokio adapters "just work" anywhere [`Interface`] is expected.
//!
//! ## Why the split?
//!
//! Rust's AFIT (`async fn` in traits) generates opaque future types whose
//! `Send`-ness the compiler infers per implementation.  Tokio's
//! multi-threaded scheduler requires futures to be `Send + 'static` before
//! they can be spawned.  Expressing this with AFIT alone is not possible on
//! stable Rust — the compiler cannot verify the bound on an opaque type.
//!
//! RPITIT (`impl Trait` in trait return position) *can* carry explicit
//! `+ Send` bounds, which is what [`SendInterface`] uses.  `trait_variant`
//! generates both traits from one definition so neither runtime needs to
//! copy-paste the method signatures.
//!
//! ## Implementing for Embassy
//!
//! ```rust,ignore
//! use reticulum_core::interface::Interface;
//!
//! pub struct LoraIface<SPI> { radio: SPI }
//!
//! impl<SPI: embedded_hal_async::spi::SpiDevice> Interface for LoraIface<SPI> {
//!     type Error = SPI::Error;
//!
//!     async fn transmit(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
//!         self.radio.write(frame).await
//!     }
//!
//!     async fn receive(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
//!         self.radio.read(buf).await
//!     }
//!
//!     fn mtu(&self) -> usize { 255 }
//! }
//! ```
//!
//! ## Implementing for Tokio
//!
//! ```rust,ignore
//! use reticulum_core::interface::SendInterface;
//!
//! pub struct TcpIface { stream: tokio::net::TcpStream }
//!
//! impl SendInterface for TcpIface {
//!     type Error = std::io::Error;
//!
//!     fn transmit<'a>(&'a mut self, frame: &'a [u8])
//!         -> impl Future<Output = Result<(), Self::Error>> + Send + 'a
//!     {
//!         async move { self.stream.write_all(frame).await }
//!     }
//!
//!     fn receive<'a>(&'a mut self, buf: &'a mut [u8])
//!         -> impl Future<Output = Result<usize, Self::Error>> + Send + 'a
//!     {
//!         async move { self.stream.read(buf).await }
//!     }
//!
//!     fn mtu(&self) -> usize { 500 }
//! }
//! ```
//!
//! ## Sync adapters
//!
//! For drivers that only expose blocking or poll-based I/O, see
//! [`SyncInterface`] and [`SyncAdapter`].

// ── Interface + SendInterface ─────────────────────────────────────────────────

/// An async transport interface.
///
/// Implement this for any physical or virtual link over which raw Reticulum
/// frames are exchanged.  Both methods are `async fn` so that the runtime can
/// suspend the calling task while waiting for I/O.
///
/// # Embassy vs Tokio
///
/// This trait uses AFIT (async fn in trait).  Embassy tasks are `!Send` by
/// design, so no `Send` bound is added here.  Tokio's multi-threaded
/// scheduler needs `Send` futures — use [`SendInterface`] for that.
///
/// See the [module documentation](self) for a detailed explanation and
/// code examples.
#[trait_variant::make(SendInterface: Send)]
#[allow(async_fn_in_trait)]
pub trait Interface {
    /// Error type returned by I/O operations.
    type Error;

    /// Transmit `frame` over the interface.
    ///
    /// Implementations may buffer the write and return before the frame has
    /// been fully transmitted, but must not silently drop it.
    async fn transmit(&mut self, frame: &[u8]) -> Result<(), Self::Error>;

    /// Receive one frame into `buf`.
    ///
    /// Suspends until a complete frame is available.  Returns the number of
    /// bytes written into `buf`.
    async fn receive(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error>;

    /// Maximum transmission unit (bytes) for this interface.
    ///
    /// The runtime will never call [`transmit`](Self::transmit) with a frame
    /// larger than this value.
    fn mtu(&self) -> usize;
}

// ── SyncInterface ─────────────────────────────────────────────────────────────

/// A synchronous, poll-based transport interface.
///
/// Kept for drivers that only expose blocking or poll-based I/O.  Wrap with
/// [`SyncAdapter`] to obtain an async [`Interface`] impl.
pub trait SyncInterface {
    /// Error type returned by I/O operations.
    type Error;

    /// Transmit `frame` over the interface.  Must not block indefinitely.
    fn transmit(&mut self, frame: &[u8]) -> Result<(), Self::Error>;

    /// Attempt to receive one frame into `buf` without blocking.
    ///
    /// Returns:
    /// - `Ok(Some(n))` — `n` bytes were written into `buf`.
    /// - `Ok(None)`    — no frame available right now.
    /// - `Err(e)`      — a transport-level error occurred.
    fn poll_receive(&mut self, buf: &mut [u8]) -> Result<Option<usize>, Self::Error>;

    /// Maximum transmission unit (bytes) for this interface.
    fn mtu(&self) -> usize;
}

// ── SyncAdapter ───────────────────────────────────────────────────────────────

/// Wraps a [`SyncInterface`] and provides an async [`Interface`] impl.
///
/// `transmit` delegates directly to the inner synchronous call.  `receive`
/// loops on [`SyncInterface::poll_receive`], yielding to the executor each
/// time no data is available.
///
/// # Caveats
///
/// The yield strategy used by `receive` is `wake_by_ref() + Poll::Pending`,
/// which reschedules the future immediately — a **cooperative spin loop**.
/// Acceptable for test stubs and loopback adapters.  For production embedded
/// targets, implement [`Interface`] directly on your driver instead.
pub struct SyncAdapter<T: SyncInterface> {
    inner: T,
}

impl<T: SyncInterface> SyncAdapter<T> {
    /// Wraps `inner` in a `SyncAdapter`.
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    /// Returns a reference to the inner driver.
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Returns a mutable reference to the inner driver.
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Unwraps, returning the inner driver.
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T: SyncInterface> Interface for SyncAdapter<T> {
    type Error = T::Error;

    async fn transmit(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        self.inner.transmit(frame)
    }

    async fn receive(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        loop {
            match self.inner.poll_receive(buf)? {
                Some(n) => return Ok(n),
                None => {
                    core::future::poll_fn(|cx| {
                        cx.waker().wake_by_ref();
                        core::task::Poll::<()>::Pending
                    })
                    .await;
                }
            }
        }
    }

    fn mtu(&self) -> usize {
        self.inner.mtu()
    }
}
