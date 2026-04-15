//! Embassy runtime support for the Reticulum Network Stack.
//!
//! This crate wires [`reticulum-core`] protocol types into the Embassy async
//! executor so that Reticulum can run on bare-metal and RTOS targets without
//! `std` or `alloc`.
//!
//! # What this crate provides
//!
//! | Item | Purpose |
//! |------|---------|
//! | [`clock`] *(feature: `embassy-time`)* | [`EmbassyClock`] — implements [`Clock`] and [`Delay`] via `embassy_time` |
//! | [`hdlc`] | Re-export of `reticulum_core::hdlc` — shared HDLC codec |
//! | [`iface`] | [`drive_interface`] driver loop, [`InterfaceRouter`], and [`HdlcInterface`] |
//!
//! # Target agnosticism
//!
//! This crate does **not** pull in any target-specific HAL.  The two runtime
//! dependencies that touch hardware-level concerns are:
//!
//! - **`embassy-time`** — guarded behind the `embassy-time` Cargo feature
//!   (enabled by default).  Disable it if your target uses a custom time
//!   source; all interface infrastructure remains available.
//! - **`embassy-sync`** critical section — provided automatically by the
//!   target HAL (e.g. `embassy-nrf`).  For host-side tests add
//!   `critical-section = { version = "1", features = ["std"] }` to your
//!   `[dev-dependencies]`.
//!
//! # Architectural notes
//!
//! Embassy tasks must be `'static` and monomorphised at compile time — there
//! is no `tokio::task::spawn` equivalent for generic futures at runtime.
//! This crate provides the building blocks instead of a dynamic
//! `InterfaceManager`:
//!
//! 1. Declare `static` Embassy channels (one shared RX bus, one TX channel per
//!    interface).
//! 2. Write thin `#[embassy_executor::task]` wrappers that call
//!    [`iface::drive_interface`].
//! 3. Use [`iface::InterfaceRouter`] to dispatch outbound packets.
//!
//! # Memory budget
//!
//! [`reticulum_core::packet::Packet`] contains a `StaticBuffer<2048>`, making
//! each channel message roughly **2.1 KB**.  On an nRF52840 (256 KB RAM)
//! prefer channel capacities of 2–4.  See [`iface`] for the full table.
//!
//! # Heltec T114 / nRF52840 checklist
//!
//! 1. **Time driver** — enable the correct `time-driver-*` feature in
//!    `embassy-nrf` for your chip variant.
//! 2. **RNG** — use `embassy_nrf::rng::Rng` (implements `rand_core::RngCore`)
//!    and pass it to [`PrivateIdentity::new_from_rand`].
//! 3. **Identity storage** — use `embedded-storage` (or `nrf-softdevice`'s
//!    flash) to persist the 64-byte raw key across power cycles.
//! 4. **LoRa interface** — wrap your UART-to-RNode connection with
//!    [`iface::hdlc::HdlcInterface`]`<_, 512>` (512-byte accumulation buffer
//!    → 255-byte MTU).
//!
//! [`Clock`]: reticulum_core::clock::Clock
//! [`Delay`]: reticulum_core::clock::Delay
//! [`EmbassyClock`]: clock::EmbassyClock
//! [`drive_interface`]: iface::drive_interface
//! [`HdlcInterface`]: iface::hdlc::HdlcInterface
//! [`InterfaceRouter`]: iface::InterfaceRouter
//! [`PrivateIdentity::new_from_rand`]: reticulum_core::identity::PrivateIdentity::new_from_rand

#![no_std]
#![deny(missing_docs)]

#[cfg(feature = "embassy-time")]
pub mod clock;

/// HDLC frame codec — re-exported from [`reticulum_core::hdlc`].
///
/// The canonical implementation lives in `reticulum-core` and is shared by
/// both `reticulum-embassy` and `reticulum-tokio`.
pub use reticulum_core::hdlc;

pub mod iface;
