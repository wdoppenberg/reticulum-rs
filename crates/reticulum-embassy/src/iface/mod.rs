//! Embassy interface driver and routing infrastructure.
//!
//! # Overview
//!
//! Each physical or virtual link is represented by an [`Interface`] impl from
//! `reticulum-core`.  This module provides:
//!
//! - [`RxMessage`] / [`TxMessage`] / [`TxMessageType`] — the packet-envelope
//!   types exchanged over Embassy channels.
//! - [`drive_interface`] — an async function that runs the RX/TX loop for one
//!   interface.  Spawn it inside a thin `#[embassy_executor::task]` wrapper.
//! - [`InterfaceRouter`] — holds up to `N` TX-channel senders and fans out
//!   outbound packets (broadcast or direct) to the correct interface(s).
//!
//! # Channel layout
//!
//! ```text
//!                   ┌─────────────┐
//!   iface0 ──rx──▶ │             │
//!   iface1 ──rx──▶ │  RX Channel │──▶  protocol task (reads RxMessage)
//!   iface2 ──rx──▶ │  (shared)   │
//!                   └─────────────┘
//!
//!   protocol task ──▶ InterfaceRouter ──tx0──▶ iface0
//!                                    ──tx1──▶ iface1
//!                                    ──tx2──▶ iface2
//! ```
//!
//! All channels are `static` and declared by the application; this crate only
//! touches them through `DynamicSender` / `DynamicReceiver` so that no
//! concrete capacity constant leaks into library code.
//!
//! # Memory budget (important for embedded targets)
//!
//! [`Packet`] internally holds a `StaticBuffer<PACKET_MDU>` (currently 512
//! bytes, sized to fit one [`RETICULUM_MTU`]-bounded wire frame), making each
//! [`RxMessage`] / [`TxMessage`] roughly **0.55 KB** on the stack or in a
//! channel slot.  On an nRF52840 (256 KB RAM) channel capacities up to 16
//! are comfortable:
//!
//! | Cap | Channel RAM |
//! |-----|-------------|
//! |   4 |   ~2.2 KB  |
//! |   8 |   ~4.4 KB  |
//! |  16 |   ~8.8 KB  |
//!
//! [`RETICULUM_MTU`]: reticulum_core::packet::RETICULUM_MTU
//!
//! For LoRa (MTU 255 B) the actual data is always far smaller, but the
//! `Packet` struct is fixed-size regardless.
//!
//! # Wiring example
//!
//! ```rust,ignore
//! use embassy_sync::{channel::Channel, blocking_mutex::raw::CriticalSectionRawMutex};
//! use reticulum_embassy::iface::{RxMessage, TxMessage, drive_interface};
//!
//! // Shared RX bus — all interfaces deposit received packets here.
//! static RX: Channel<CriticalSectionRawMutex, RxMessage, 4> = Channel::new();
//!
//! // Per-interface TX channels.
//! static TX0: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();
//!
//! #[embassy_executor::task]
//! async fn lora_task(iface: MyLoraIface, addr: AddressHash) {
//!     drive_interface::<_, 255>(iface, addr, RX.dyn_sender(), TX0.dyn_receiver()).await;
//! }
//! ```
//!
//! [`Interface`]: reticulum_core::interface::Interface
//! [`Packet`]: reticulum_core::packet::Packet

pub mod hdlc;

use embassy_futures::select::{select, Either};
use embassy_sync::channel::{DynamicReceiver, DynamicSender};
use heapless::Vec;

use reticulum_core::buffer::{InputBuffer, OutputBuffer};
use reticulum_core::hash::AddressHash;
use reticulum_core::interface::Interface;
use reticulum_core::packet::Packet;
use reticulum_core::serde::Serialize;

// ── Routing types (shared with reticulum-tokio via reticulum-core) ────────────

/// Routing selector — controls which interface(s) receive a [`TxMessage`].
///
/// Re-exported from [`reticulum_core::routing`].
pub use reticulum_core::routing::TxMessageType;

/// An outbound packet destined for one or more interfaces.
///
/// Re-exported from [`reticulum_core::routing`].
pub use reticulum_core::routing::TxMessage;

/// A packet received from an interface, tagged with the interface's address.
///
/// Re-exported from [`reticulum_core::routing`].
pub use reticulum_core::routing::RxMessage;

// ── drive_interface ───────────────────────────────────────────────────────────

/// Drive one [`Interface`] to completion.
///
/// Loops forever, racing between:
///
/// - `iface.receive()` → deserialises the raw frame into a [`Packet`] and
///   forwards it to `rx_send`, tagged with `address`.
/// - `tx_recv.receive()` → serialises the [`Packet`] and calls
///   `iface.transmit()`.
///
/// The function returns only when `tx_recv` is closed (all senders dropped),
/// which in practice happens only during a controlled shutdown.
///
/// ## Error handling
///
/// **Receive errors are non-fatal** — individual frame errors (CRC failures,
/// malformed HDLC, partial frames) are common on lossy links such as LoRa.
/// The driver logs the error and continues.  Only use a transport that returns
/// errors for true link-layer faults (e.g. `HdlcInterface` already translates
/// framing errors into skipped frames before reaching this layer).
///
/// Transmit errors are also non-fatal: the packet is dropped and the loop
/// continues.
///
/// ## Type parameters
///
/// - `I` — any [`Interface`] implementation; `Error` must be `Debug`.
/// - `MTU` — stack-allocated RX frame buffer size.  Must be ≥ `iface.mtu()`.
///   Frames larger than `MTU` bytes will be truncated and likely fail
///   deserialisation (logged and skipped).
///
/// [`Interface`]: reticulum_core::interface::Interface
pub async fn drive_interface<I, const MTU: usize>(
    mut iface: I,
    address: AddressHash,
    rx_send: DynamicSender<'_, RxMessage>,
    tx_recv: DynamicReceiver<'_, TxMessage>,
) where
    I: Interface,
    I::Error: core::fmt::Debug,
{
    let iface_mtu = iface.mtu();
    if MTU < iface_mtu {
        log::warn!(
            "drive_interface: MTU const ({MTU}) < iface.mtu() ({iface_mtu}); \
             large received frames will be truncated and dropped"
        );
    }

    let mut rx_buf = [0u8; MTU];

    loop {
        match select(iface.receive(&mut rx_buf), tx_recv.receive()).await {
            // ── Inbound frame ────────────────────────────────────────────────
            Either::First(Ok(n)) => {
                match Packet::deserialize(&mut InputBuffer::new(&rx_buf[..n])) {
                    Ok(packet) => {
                        rx_send.send(RxMessage { address, packet }).await;
                    }
                    Err(e) => {
                        log::warn!("drive_interface [{address}]: RX deserialise error: {e:?}");
                    }
                }
            }
            Either::First(Err(e)) => {
                // Non-fatal: log and continue.  Common on lossy RF links.
                log::warn!("drive_interface [{address}]: receive error: {e:?}");
            }

            // ── Outbound frame ───────────────────────────────────────────────
            Either::Second(msg) => {
                let mut tx_backing = [0u8; MTU];
                let mut out = OutputBuffer::new(&mut tx_backing);
                match msg.packet.serialize(&mut out) {
                    Ok(_) => {
                        if let Err(e) = iface.transmit(out.as_slice()).await {
                            log::warn!("drive_interface [{address}]: transmit error: {e:?}");
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "drive_interface [{address}]: TX serialise error \
                             (packet too large for MTU {MTU}?): {e:?}"
                        );
                    }
                }
            }
        }
    }
}

// ── InterfaceRouter ───────────────────────────────────────────────────────────

struct IfaceEntry<'a> {
    address: AddressHash,
    tx: DynamicSender<'a, TxMessage>,
}

/// Routes outbound [`TxMessage`]s to the correct interface TX channels.
///
/// `InterfaceRouter` holds up to `N` [`DynamicSender`] handles, one per
/// registered interface.  In the protocol task, call [`route`] or
/// [`try_route`] to dispatch packets.
///
/// ## Lifetime
///
/// `'a` is the lifetime of the static channels the senders originate from.
/// In practice `'a` is `'static`.
///
/// [`route`]: Self::route
/// [`try_route`]: Self::try_route
pub struct InterfaceRouter<'a, const N: usize> {
    ifaces: Vec<IfaceEntry<'a>, N>,
}

impl<'a, const N: usize> InterfaceRouter<'a, N> {
    /// Creates an empty router.
    pub const fn new() -> Self {
        Self { ifaces: Vec::new() }
    }

    /// Number of interfaces currently registered.
    pub fn len(&self) -> usize {
        self.ifaces.len()
    }

    /// Returns `true` if no interfaces are registered.
    pub fn is_empty(&self) -> bool {
        self.ifaces.is_empty()
    }

    /// Returns the addresses of all registered interfaces.
    pub fn addresses(&self) -> impl Iterator<Item = AddressHash> + '_ {
        self.ifaces.iter().map(|e| e.address)
    }

    /// Register an interface with the router.
    ///
    /// Returns `Err(())` if the router is already full (`N` interfaces
    /// registered).  The sender is dropped in the error case.
    #[allow(clippy::result_unit_err)]
    pub fn register(
        &mut self,
        address: AddressHash,
        tx: DynamicSender<'a, TxMessage>,
    ) -> Result<(), ()> {
        self.ifaces.push(IfaceEntry { address, tx }).map_err(|_| ())
    }

    /// Send a [`TxMessage`] to the appropriate interface(s).
    ///
    /// - [`TxMessageType::Broadcast`] — sends to **all** interfaces, skipping
    ///   any whose address matches the optional exclusion address.
    /// - [`TxMessageType::Direct`] — sends only to the interface whose address
    ///   matches exactly.
    ///
    /// Each send is awaited; if a TX channel is full the router yields until
    /// there is space.  For best-effort, non-blocking behaviour use
    /// [`try_route`](Self::try_route).
    pub async fn route(&self, msg: TxMessage) {
        for entry in &self.ifaces {
            if self.should_send(msg.tx_type, entry.address) {
                entry.tx.send(msg).await;
            }
        }
    }

    /// Non-blocking variant of [`route`](Self::route).
    ///
    /// Attempts to enqueue `msg` on every targeted TX channel.  If **any**
    /// targeted channel is full, that send is skipped; others are still
    /// attempted.  Returns `Err(msg)` if at least one send was skipped.
    ///
    /// Callers that need guaranteed delivery to all targets should use the
    /// async [`route`](Self::route) instead.
    #[allow(clippy::result_large_err)]
    pub fn try_route(&self, msg: TxMessage) -> Result<(), TxMessage> {
        let mut all_ok = true;
        for entry in &self.ifaces {
            if self.should_send(msg.tx_type, entry.address) && entry.tx.try_send(msg).is_err() {
                all_ok = false;
            }
        }
        if all_ok {
            Ok(())
        } else {
            Err(msg)
        }
    }

    fn should_send(&self, tx_type: TxMessageType, addr: AddressHash) -> bool {
        match tx_type {
            TxMessageType::Broadcast(exclude) => exclude != Some(addr),
            TxMessageType::Direct(target) => target == addr,
        }
    }
}

impl<'a, const N: usize> Default for InterfaceRouter<'a, N> {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
    use reticulum_core::hash::AddressHash;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_addr(byte: u8) -> AddressHash {
        AddressHash::new([byte; 16])
    }

    fn make_packet() -> Packet {
        Packet::new_empty()
    }

    fn make_tx(tx_type: TxMessageType) -> TxMessage {
        TxMessage {
            tx_type,
            packet: make_packet(),
        }
    }

    // ── InterfaceRouter ───────────────────────────────────────────────────────

    #[test]
    fn router_starts_empty() {
        let router: InterfaceRouter<'static, 4> = InterfaceRouter::new();
        assert!(router.is_empty());
        assert_eq!(router.len(), 0);
    }

    #[test]
    fn router_register_and_len() {
        static CH0: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();
        static CH1: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();

        let mut router: InterfaceRouter<'static, 4> = InterfaceRouter::new();
        router.register(make_addr(0), CH0.dyn_sender()).unwrap();
        router.register(make_addr(1), CH1.dyn_sender()).unwrap();

        assert_eq!(router.len(), 2);
        let addrs: heapless::Vec<AddressHash, 4> = router.addresses().collect();
        assert!(addrs.contains(&make_addr(0)));
        assert!(addrs.contains(&make_addr(1)));
    }

    #[test]
    fn router_full_returns_err() {
        static CH0: Channel<CriticalSectionRawMutex, TxMessage, 2> = Channel::new();
        static CH1: Channel<CriticalSectionRawMutex, TxMessage, 2> = Channel::new();
        static CH2: Channel<CriticalSectionRawMutex, TxMessage, 2> = Channel::new();

        let mut router: InterfaceRouter<'static, 2> = InterfaceRouter::new();
        router.register(make_addr(0), CH0.dyn_sender()).unwrap();
        router.register(make_addr(1), CH1.dyn_sender()).unwrap();
        // Third registration must fail — router capacity is 2.
        assert_eq!(router.register(make_addr(2), CH2.dyn_sender()), Err(()));
    }

    #[test]
    fn try_route_broadcast_reaches_all() {
        static CH0: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();
        static CH1: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();

        let mut router: InterfaceRouter<'static, 4> = InterfaceRouter::new();
        router.register(make_addr(0), CH0.dyn_sender()).unwrap();
        router.register(make_addr(1), CH1.dyn_sender()).unwrap();

        let msg = make_tx(TxMessageType::Broadcast(None));
        router.try_route(msg).unwrap();

        assert_eq!(CH0.try_receive().unwrap(), msg);
        assert_eq!(CH1.try_receive().unwrap(), msg);
    }

    #[test]
    fn try_route_broadcast_excludes_source() {
        static CH0: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();
        static CH1: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();

        let mut router: InterfaceRouter<'static, 4> = InterfaceRouter::new();
        router.register(make_addr(0), CH0.dyn_sender()).unwrap();
        router.register(make_addr(1), CH1.dyn_sender()).unwrap();

        // Exclude addr(0) — simulates re-broadcasting a packet received on iface0.
        let msg = make_tx(TxMessageType::Broadcast(Some(make_addr(0))));
        router.try_route(msg).unwrap();

        // iface0 must NOT receive the packet.
        assert!(CH0.try_receive().is_err());
        // iface1 MUST receive it.
        assert_eq!(CH1.try_receive().unwrap(), msg);
    }

    #[test]
    fn try_route_direct_only_reaches_target() {
        static CH0: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();
        static CH1: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();

        let mut router: InterfaceRouter<'static, 4> = InterfaceRouter::new();
        router.register(make_addr(0), CH0.dyn_sender()).unwrap();
        router.register(make_addr(1), CH1.dyn_sender()).unwrap();

        let msg = make_tx(TxMessageType::Direct(make_addr(1)));
        router.try_route(msg).unwrap();

        assert!(CH0.try_receive().is_err()); // iface0 not targeted
        assert_eq!(CH1.try_receive().unwrap(), msg);
    }

    #[test]
    fn try_route_direct_unknown_addr_sends_nowhere() {
        static CH0: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();

        let mut router: InterfaceRouter<'static, 4> = InterfaceRouter::new();
        router.register(make_addr(0), CH0.dyn_sender()).unwrap();

        // Address 99 is not registered.
        let msg = make_tx(TxMessageType::Direct(make_addr(99)));
        router.try_route(msg).unwrap(); // ok — just sent nowhere

        assert!(CH0.try_receive().is_err());
    }

    #[test]
    fn try_route_returns_err_when_channel_full() {
        // Channel capacity = 1; fill it first.
        static CH0: Channel<CriticalSectionRawMutex, TxMessage, 1> = Channel::new();

        let mut router: InterfaceRouter<'static, 4> = InterfaceRouter::new();
        router.register(make_addr(0), CH0.dyn_sender()).unwrap();

        let msg = make_tx(TxMessageType::Broadcast(None));
        router.try_route(msg).unwrap(); // fills the slot

        // Channel is now full — second try_route must return Err.
        assert!(router.try_route(msg).is_err());
    }

    // ── route (async) ─────────────────────────────────────────────────────────

    #[test]
    fn route_async_broadcast() {
        static CH0: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();
        static CH1: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();

        let mut router: InterfaceRouter<'static, 4> = InterfaceRouter::new();
        router.register(make_addr(0), CH0.dyn_sender()).unwrap();
        router.register(make_addr(1), CH1.dyn_sender()).unwrap();

        let msg = make_tx(TxMessageType::Broadcast(None));
        // route() is async but channels have space, so it completes immediately.
        pollster::block_on(router.route(msg));

        assert_eq!(CH0.try_receive().unwrap(), msg);
        assert_eq!(CH1.try_receive().unwrap(), msg);
    }

    #[test]
    fn route_async_direct() {
        static CH0: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();
        static CH1: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();

        let mut router: InterfaceRouter<'static, 4> = InterfaceRouter::new();
        router.register(make_addr(0), CH0.dyn_sender()).unwrap();
        router.register(make_addr(1), CH1.dyn_sender()).unwrap();

        let msg = make_tx(TxMessageType::Direct(make_addr(0)));
        pollster::block_on(router.route(msg));

        assert_eq!(CH0.try_receive().unwrap(), msg);
        assert!(CH1.try_receive().is_err());
    }
}
