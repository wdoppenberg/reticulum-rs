//! Pure `no_std` packet router for `reticulum-node`.
//!
//! [`Router`] makes forwarding decisions for received packets:
//!
//! - **Deduplication**: a ring buffer of recently-seen packet hashes prevents
//!   the same packet from being forwarded more than once.
//! - **Hop limit**: packets with `hops >= MAX_HOPS` are dropped.
//! - **Hop increment**: the forwarded packet's `hops` field is incremented by
//!   one before transmission, as required by the Reticulum protocol.
//! - **Path table**: maps destination [`AddressHash`]es to the
//!   [`AddressHash`] of the interface last known to route toward them.
//! - **Transport propagation**: when `propagation_type == Transport`, the
//!   packet's `transport` field carries the address of the intended relay node.
//!   The router checks whether the local node is the intended relay before
//!   forwarding.
//!
//! # Size parameters
//!
//! | Parameter  | Type             | Role                              |
//! |------------|------------------|-----------------------------------|
//! | `N_SEEN`   | `const usize`    | Dedup ring-buffer capacity        |
//! | `N_PATHS`  | `const usize`    | Path-table capacity               |

use heapless::Deque;
use heapless::LinearMap;

use reticulum_core::hash::AddressHash;
use reticulum_core::packet::{DestinationType, Packet, PacketType, PropagationType};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum hop count before a packet is dropped.  Matches Reticulum's
/// `PATHFINDER_M = 128`.
pub const MAX_HOPS: u8 = 128;

// ── RouteDecision ─────────────────────────────────────────────────────────────

/// What the router decided to do with an incoming packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDecision {
    /// Forward the packet (with incremented hop count) to all interfaces except
    /// the one it arrived on.
    Broadcast,
    /// Forward the packet (with incremented hop count) directly to the interface
    /// with this address.
    Direct(AddressHash),
    /// Deliver the packet locally — it is addressed to this node.
    Local,
    /// Drop the packet (duplicate, hop-limit exceeded, or unroutable).
    Drop,
}

// ── Router ────────────────────────────────────────────────────────────────────

/// Stateful forwarding table.
///
/// - `N_SEEN`: dedup ring-buffer size (power-of-two recommended).
/// - `N_PATHS`: maximum number of destination→interface mappings.
pub struct Router<const N_SEEN: usize, const N_PATHS: usize> {
    /// Ring buffer of recently-seen packet hashes (16 bytes each).
    seen: Deque<[u8; 16], N_SEEN>,
    /// destination_hash → source_interface_hash
    paths: LinearMap<AddressHash, AddressHash, N_PATHS>,
    /// The local node's address — used to detect transport-propagated packets
    /// that this node should relay.
    node_addr: AddressHash,
    /// Whether to forward announces.
    forward_announces: bool,
    /// Whether transport (data forwarding) is enabled.
    transport_enabled: bool,
}

impl<const N_SEEN: usize, const N_PATHS: usize> Router<N_SEEN, N_PATHS> {
    /// Create a new router.
    pub const fn new(
        node_addr: AddressHash,
        forward_announces: bool,
        transport_enabled: bool,
    ) -> Self {
        Self {
            seen: Deque::new(),
            paths: LinearMap::new(),
            node_addr,
            forward_announces,
            transport_enabled,
        }
    }

    /// Record a path: packets destined for `destination` can reach it via
    /// `via_interface`.
    ///
    /// Returns `false` if the path table is full and the entry could not be
    /// inserted.
    pub fn learn_path(&mut self, destination: AddressHash, via_interface: AddressHash) -> bool {
        if let Some(entry) = self.paths.get_mut(&destination) {
            *entry = via_interface;
            return true;
        }
        self.paths.insert(destination, via_interface).is_ok()
    }

    /// Look up the outbound interface for `destination`, if known.
    pub fn lookup(&self, destination: &AddressHash) -> Option<AddressHash> {
        self.paths.get(destination).copied()
    }

    /// Remove a path from the table.
    pub fn forget_path(&mut self, destination: &AddressHash) {
        self.paths.remove(destination);
    }

    /// Returns `true` if this packet hash has been seen recently.
    pub fn is_duplicate(&self, hash: &[u8; 16]) -> bool {
        self.seen.iter().any(|h| h == hash)
    }

    /// Mark the packet hash as seen.  Evicts the oldest entry if the ring is
    /// full.
    pub fn mark_seen(&mut self, hash: [u8; 16]) {
        if self.seen.is_full() {
            self.seen.pop_front();
        }
        let _ = self.seen.push_back(hash);
    }

    /// Return the first 16 bytes of the packet's SHA-256 hash — used as the
    /// dedup key.
    fn dedup_key(packet: &Packet) -> [u8; 16] {
        let full = packet.hash();
        let b = full.as_bytes();
        let mut key = [0u8; 16];
        key.copy_from_slice(&b[..16]);
        key
    }

    /// Decide what to do with a received packet.
    ///
    /// `source_iface` is the [`AddressHash`] of the interface that delivered
    /// the packet — used to exclude it from broadcasts and to update the path
    /// table.
    ///
    /// When the decision is [`Broadcast`](RouteDecision::Broadcast) or
    /// [`Direct`](RouteDecision::Direct), the caller must increment
    /// `packet.header.hops` before transmitting.  Use
    /// [`increment_hops`](Self::increment_hops) for this.
    pub fn route(&mut self, packet: &Packet, source_iface: AddressHash) -> RouteDecision {
        // ── Hop limit ──────────────────────────────────────────────────────────
        if packet.header.hops >= MAX_HOPS {
            log::debug!(
                "router: dropping packet (hops={} >= MAX_HOPS={})",
                packet.header.hops,
                MAX_HOPS,
            );
            return RouteDecision::Drop;
        }

        // ── Deduplication ──────────────────────────────────────────────────────
        let key = Self::dedup_key(packet);
        if self.is_duplicate(&key) {
            return RouteDecision::Drop;
        }
        self.mark_seen(key);

        // ── Transport propagation ──────────────────────────────────────────────
        //
        // When `propagation_type == Transport`, the `transport` field holds the
        // address of the node that should relay this packet onward.  If that
        // address is us, we accept the relay role.  If it is not us, but we know
        // the way to the transport node, forward there; otherwise drop (we are
        // not the intended relay).
        if packet.header.propagation_type == PropagationType::Transport {
            if let Some(transport_addr) = packet.transport {
                if transport_addr == self.node_addr {
                    // We are the intended transport relay — proceed with normal
                    // destination-based forwarding below, but without the
                    // transport guard: fall through to the packet_type match.
                } else {
                    // Not our relay role.  Forward toward the transport node if
                    // we know the route; otherwise drop.
                    if let Some(via) = self.lookup(&transport_addr) {
                        return RouteDecision::Direct(via);
                    } else {
                        return RouteDecision::Drop;
                    }
                }
            }
        }

        match packet.header.packet_type {
            // ── Announce ───────────────────────────────────────────────────────
            PacketType::Announce => {
                if !self.forward_announces {
                    return RouteDecision::Drop;
                }
                // Learn: the source interface can reach the announced destination.
                self.learn_path(packet.destination, source_iface);
                RouteDecision::Broadcast
            }

            // ── Data packets ───────────────────────────────────────────────────
            PacketType::Data => {
                if !self.transport_enabled {
                    return RouteDecision::Drop;
                }
                match packet.header.destination_type {
                    DestinationType::Single | DestinationType::Group => {
                        // Addressed to us?
                        if packet.destination == self.node_addr {
                            return RouteDecision::Local;
                        }
                        if let Some(via) = self.lookup(&packet.destination) {
                            RouteDecision::Direct(via)
                        } else {
                            RouteDecision::Broadcast
                        }
                    }
                    DestinationType::Plain | DestinationType::Link => RouteDecision::Broadcast,
                }
            }

            // ── Link request / proof ───────────────────────────────────────────
            PacketType::LinkRequest | PacketType::Proof => {
                if !self.transport_enabled {
                    return RouteDecision::Drop;
                }
                if packet.destination == self.node_addr {
                    return RouteDecision::Local;
                }
                if let Some(via) = self.lookup(&packet.destination) {
                    RouteDecision::Direct(via)
                } else {
                    RouteDecision::Broadcast
                }
            }
        }
    }

    /// Increment the hop counter in a packet that is about to be forwarded.
    ///
    /// Call this before passing the packet to [`InterfaceRouter::route`].
    /// Saturates at [`u8::MAX`] (which is well above [`MAX_HOPS`]).
    pub fn increment_hops(packet: &mut Packet) {
        packet.header.hops = packet.header.hops.saturating_add(1);
    }

    /// Number of known paths.
    pub fn path_count(&self) -> usize {
        self.paths.len()
    }

    /// Number of recently-seen packet hashes held.
    pub fn seen_count(&self) -> usize {
        self.seen.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_core::hash::Hash;
    use reticulum_core::packet::{
        Header, HeaderType, IfacFlag, PacketContext, PacketDataBuffer, PropagationType,
    };

    fn make_addr(seed: u8) -> AddressHash {
        let mut b = [0u8; 32];
        b[0] = seed;
        AddressHash::new_from_hash(&Hash::new_from_slice(&b))
    }

    fn node_addr() -> AddressHash {
        make_addr(0xFF)
    }

    fn make_router() -> Router<16, 16> {
        Router::new(node_addr(), true, true)
    }

    fn make_packet(dest: AddressHash, ptype: PacketType, dtype: DestinationType) -> Packet {
        make_packet_with_hops(dest, ptype, dtype, PropagationType::Broadcast, None, 0)
    }

    fn make_packet_with_hops(
        dest: AddressHash,
        ptype: PacketType,
        dtype: DestinationType,
        prop: PropagationType,
        transport: Option<AddressHash>,
        hops: u8,
    ) -> Packet {
        Packet {
            header: Header {
                ifac_flag: IfacFlag::Open,
                header_type: HeaderType::Type1,
                propagation_type: prop,
                destination_type: dtype,
                packet_type: ptype,
                hops,
            },
            destination: dest,
            transport,
            context: PacketContext::None,
            data: PacketDataBuffer::new(),
            ifac: None,
        }
    }

    #[test]
    fn announce_is_broadcast_and_learns_path() {
        let mut router = make_router();
        let dest = make_addr(1);
        let iface = make_addr(10);
        let pkt = make_packet(dest, PacketType::Announce, DestinationType::Single);

        assert_eq!(router.route(&pkt, iface), RouteDecision::Broadcast);
        assert_eq!(router.lookup(&dest), Some(iface));
    }

    #[test]
    fn duplicate_is_dropped() {
        let mut router = make_router();
        let dest = make_addr(2);
        let iface = make_addr(10);
        let pkt = make_packet(dest, PacketType::Announce, DestinationType::Single);

        router.route(&pkt, iface);
        assert_eq!(router.route(&pkt, iface), RouteDecision::Drop);
    }

    #[test]
    fn data_packet_routed_to_known_path() {
        let mut router = make_router();
        let dest = make_addr(3);
        let iface_a = make_addr(10);
        let iface_b = make_addr(11);

        router.learn_path(dest, iface_b);
        let pkt = make_packet(dest, PacketType::Data, DestinationType::Single);
        assert_eq!(router.route(&pkt, iface_a), RouteDecision::Direct(iface_b));
    }

    #[test]
    fn data_packet_broadcast_when_no_path() {
        let mut router = make_router();
        let dest = make_addr(4);
        let iface = make_addr(10);
        let pkt = make_packet(dest, PacketType::Data, DestinationType::Single);

        assert_eq!(router.route(&pkt, iface), RouteDecision::Broadcast);
    }

    #[test]
    fn announce_dropped_when_disabled() {
        let mut router = Router::<16, 16>::new(node_addr(), false, true);
        let dest = make_addr(5);
        let iface = make_addr(10);
        let pkt = make_packet(dest, PacketType::Announce, DestinationType::Single);

        assert_eq!(router.route(&pkt, iface), RouteDecision::Drop);
    }

    #[test]
    fn packet_at_hop_limit_is_dropped() {
        let mut router = make_router();
        let dest = make_addr(6);
        let iface = make_addr(10);
        let pkt = make_packet_with_hops(
            dest,
            PacketType::Data,
            DestinationType::Single,
            PropagationType::Broadcast,
            None,
            MAX_HOPS,
        );
        assert_eq!(router.route(&pkt, iface), RouteDecision::Drop);
    }

    #[test]
    fn packet_just_below_hop_limit_is_forwarded() {
        let mut router = make_router();
        let dest = make_addr(7);
        let iface = make_addr(10);
        let pkt = make_packet_with_hops(
            dest,
            PacketType::Data,
            DestinationType::Single,
            PropagationType::Broadcast,
            None,
            MAX_HOPS - 1,
        );
        // No path known → broadcast.
        assert_eq!(router.route(&pkt, iface), RouteDecision::Broadcast);
    }

    #[test]
    fn increment_hops_adds_one() {
        let mut pkt = make_packet(make_addr(1), PacketType::Data, DestinationType::Single);
        pkt.header.hops = 5;
        Router::<16, 16>::increment_hops(&mut pkt);
        assert_eq!(pkt.header.hops, 6);
    }

    #[test]
    fn increment_hops_saturates() {
        let mut pkt = make_packet(make_addr(1), PacketType::Data, DestinationType::Single);
        pkt.header.hops = u8::MAX;
        Router::<16, 16>::increment_hops(&mut pkt);
        assert_eq!(pkt.header.hops, u8::MAX);
    }

    #[test]
    fn local_delivery_for_our_address() {
        let mut router = make_router();
        let iface = make_addr(10);
        let pkt = make_packet(node_addr(), PacketType::Data, DestinationType::Single);

        assert_eq!(router.route(&pkt, iface), RouteDecision::Local);
    }

    #[test]
    fn transport_packet_forwarded_to_transport_node() {
        let mut router = make_router();
        let transport_node = make_addr(20);
        let dest = make_addr(3);
        let iface_a = make_addr(10);
        let iface_b = make_addr(11);

        // We know the way to the transport node.
        router.learn_path(transport_node, iface_b);

        let pkt = make_packet_with_hops(
            dest,
            PacketType::Data,
            DestinationType::Single,
            PropagationType::Transport,
            Some(transport_node),
            0,
        );

        assert_eq!(router.route(&pkt, iface_a), RouteDecision::Direct(iface_b));
    }

    #[test]
    fn transport_packet_dropped_when_no_path_to_relay() {
        let mut router = make_router();
        let transport_node = make_addr(20); // no path registered
        let dest = make_addr(3);
        let iface = make_addr(10);

        let pkt = make_packet_with_hops(
            dest,
            PacketType::Data,
            DestinationType::Single,
            PropagationType::Transport,
            Some(transport_node),
            0,
        );

        assert_eq!(router.route(&pkt, iface), RouteDecision::Drop);
    }

    #[test]
    fn transport_packet_relayed_when_we_are_transport_node() {
        let mut router = make_router();
        let dest = make_addr(3);
        let iface_a = make_addr(10);
        let iface_b = make_addr(11);

        router.learn_path(dest, iface_b);

        let pkt = make_packet_with_hops(
            dest,
            PacketType::Data,
            DestinationType::Single,
            PropagationType::Transport,
            Some(node_addr()), // we ARE the transport node
            0,
        );

        assert_eq!(router.route(&pkt, iface_a), RouteDecision::Direct(iface_b));
    }

    #[test]
    fn dedup_ring_evicts_oldest() {
        let mut router = Router::<4, 16>::new(node_addr(), true, true);
        let iface = make_addr(0);

        for i in 0..4u8 {
            let pkt = make_packet(make_addr(i), PacketType::Announce, DestinationType::Single);
            router.route(&pkt, iface);
        }
        for i in 0..4u8 {
            let pkt = make_packet(make_addr(i), PacketType::Announce, DestinationType::Single);
            assert_eq!(router.route(&pkt, iface), RouteDecision::Drop);
        }

        // Fifth entry evicts oldest (addr 0).
        let pkt5 = make_packet(make_addr(99), PacketType::Announce, DestinationType::Single);
        router.route(&pkt5, iface);

        // addr 0 is now evicted — should be accepted again.
        let pkt0 = make_packet(make_addr(0), PacketType::Announce, DestinationType::Single);
        assert_eq!(router.route(&pkt0, iface), RouteDecision::Broadcast);
    }
}
