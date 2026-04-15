//! Runtime-agnostic routing message types.
//!
//! [`TxMessage`] and [`RxMessage`] are the envelopes that flow between the
//! protocol layer and interface drivers in both the `reticulum-tokio` and
//! `reticulum-embassy` runtime crates.  Centralising them in `reticulum-core`
//! ensures both runtimes use the same wire-compatible types.

use crate::hash::AddressHash;
use crate::packet::Packet;

/// Controls which interface(s) a [`TxMessage`] is delivered to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxMessageType {
    /// Deliver to **every** registered interface.
    ///
    /// If the inner `Option<AddressHash>` is `Some(src)`, skip the interface
    /// whose address matches `src` — used to avoid re-echoing a received
    /// packet back out the same link.
    Broadcast(Option<AddressHash>),

    /// Deliver only to the interface whose address matches exactly.
    Direct(AddressHash),
}

/// An outbound packet destined for one or more interfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxMessage {
    /// Routing selector.
    pub tx_type: TxMessageType,
    /// The Reticulum packet to transmit.
    pub packet: Packet,
}

/// A packet received from an interface, tagged with the interface's address.
///
/// The `address` field lets the protocol layer identify which link the packet
/// arrived on — typically used to avoid echoing announces back to their source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxMessage {
    /// Address of the interface that received this packet.
    pub address: AddressHash,
    /// The deserialised Reticulum packet.
    pub packet: Packet,
}
