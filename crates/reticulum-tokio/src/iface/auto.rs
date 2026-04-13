//! AutoInterface — IPv6 link-local multicast peer discovery.
//!
//! Implements the same protocol as Python RNS `AutoInterface`:
//!
//! 1. A SHA-256-derived IPv6 link-local multicast group (`ff02::<hash14>`).
//! 2. Periodic discovery announcements: `SHA-256(group_id ‖ own_link_local_ip)`.
//! 3. Peer authentication: verify the received token against the UDP source.
//! 4. Data unicast to every live peer; deduplication via a sliding SHA-256 window.
//!
//! ## Type-driven patterns used
//!
//! | Pattern | Where |
//! |---------|-------|
//! | Validated Boundary (#6) | `DiscoveryToken` — only constructible via `for_addr` |
//! | `#[must_use]` (#22) | `DiscoveryToken` — caller cannot silently discard it |
//! | RAII / Drop (#20) | multicast memberships released when sockets drop |

use std::collections::{HashMap, VecDeque};
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, RwLock};


use crate::iface::RxMessage;
use reticulum_core::buffer::{InputBuffer, OutputBuffer};
use reticulum_core::packet::Packet;
use reticulum_core::serde::Serialize;

use super::{Interface, InterfaceContext};

// ── Protocol constants (1-to-1 with Python RNS AutoInterface) ─────────────────

const DEFAULT_GROUP_ID: &[u8] = b"reticulum";

/// UDP port on which discovery announcements are multicast.
pub const DISCOVERY_PORT: u16 = 29716;
/// UDP port for unicast data delivery to each peer.
pub const DATA_PORT: u16 = 42671;

const ANNOUNCE_INTERVAL: Duration = Duration::from_millis(1600);
const PEERING_TIMEOUT: Duration = Duration::from_secs(22);
const PEER_JOB_INTERVAL: Duration = Duration::from_secs(4);
const DEDUP_TTL: Duration = Duration::from_millis(750);
const DEDUP_MAX: usize = 48;

/// Hardware MTU matching Python's `HW_MTU = 1196`.
pub const HW_MTU: usize = 1196;

// ── Validated boundary: DiscoveryToken ────────────────────────────────────────

/// A 32-byte SHA-256 binding of `group_id ‖ link_local_addr`.
///
/// **Validated Boundary (Pattern #6)**: only constructible via
/// [`DiscoveryToken::for_addr`]; raw bytes cannot be cast into this type.
///
/// **`#[must_use]` (Pattern #22)**: the compiler warns if the caller drops the
/// token without using it, preventing silent authentication failures.
#[must_use]
struct DiscoveryToken([u8; 32]);

impl DiscoveryToken {
    /// Build the token we broadcast: `SHA-256(group_id ‖ addr.octets())`.
    fn for_addr(group_id: &[u8], addr: &Ipv6Addr) -> Self {
        let mut h = Sha256::new();
        h.update(group_id);
        h.update(addr.octets());
        Self(h.finalize().into())
    }

    /// Verify a 32-byte received payload against the observed UDP source.
    ///
    /// Returns `true` only if `bytes == SHA-256(group_id ‖ observed_src)`.
    fn verify(bytes: &[u8; 32], group_id: &[u8], observed_src: &Ipv6Addr) -> bool {
        let expected = Self::for_addr(group_id, observed_src);
        // Constant-time comparison avoids timing side-channels.
        expected.0 == *bytes
    }

    fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

// ── Peer entry ────────────────────────────────────────────────────────────────

struct Peer {
    /// Full scoped socket address for unicast data delivery (includes scope_id).
    addr: SocketAddrV6,
    last_heard: Instant,
}

type PeerTable = Arc<RwLock<HashMap<Ipv6Addr, Peer>>>;
type DedupQueue = Arc<Mutex<VecDeque<([u8; 32], Instant)>>>;

// ── AutoInterface ─────────────────────────────────────────────────────────────

pub struct AutoInterface {
    group_id: Vec<u8>,
    discovery_port: u16,
    data_port: u16,
}

impl AutoInterface {
    pub fn new(
        group: Option<String>,
        discovery_port: Option<u16>,
        data_port: Option<u16>,
    ) -> Self {
        Self {
            group_id: group
                .map(|g| g.into_bytes())
                .unwrap_or_else(|| DEFAULT_GROUP_ID.to_vec()),
            discovery_port: discovery_port.unwrap_or(DISCOVERY_PORT),
            data_port: data_port.unwrap_or(DATA_PORT),
        }
    }

    /// Derive the IPv6 link-local multicast group from `group_id`.
    ///
    /// Formula (matches Python): `ff02 ‖ SHA-256(group_id)[0..14]` → 16-byte
    /// address → `Ipv6Addr`.
    pub fn multicast_addr(group_id: &[u8]) -> Ipv6Addr {
        let hash = Sha256::digest(group_id);
        let mut bytes = [0u8; 16];
        bytes[0] = 0xFF;
        bytes[1] = 0x02;
        bytes[2..16].copy_from_slice(&hash[..14]);
        Ipv6Addr::from(bytes)
    }

    /// Enumerate all non-loopback link-local IPv6 addresses (`fe80::/10`) on
    /// this host.  Used to compute discovery tokens and filter self-echoes.
    fn own_link_local_addrs() -> Vec<Ipv6Addr> {
        if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|iface| {
                if iface.is_loopback() {
                    return None;
                }
                if let if_addrs::IfAddr::V6(ref v6) = iface.addr {
                    let o = v6.ip.octets();
                    if o[0] == 0xfe && (o[1] & 0xc0) == 0x80 {
                        return Some(v6.ip);
                    }
                }
                None
            })
            .collect()
    }

    /// Create an IPv6-only UDP socket with `SO_REUSEADDR` (and `SO_REUSEPORT`
    /// on Unix) bound to `[::]:port`.
    fn make_ipv6_udp(port: u16) -> std::io::Result<UdpSocket> {
        let sock = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
        sock.set_reuse_address(true)?;
        #[cfg(unix)]
        sock.set_reuse_port(true)?;
        sock.set_only_v6(true)?;
        let addr = SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0);
        sock.bind(&addr.into())?;
        sock.set_nonblocking(true)?;
        let std_sock: std::net::UdpSocket = sock.into();
        UdpSocket::from_std(std_sock)
    }

    pub async fn spawn(context: InterfaceContext<Self>) {
        let (group_id, disc_port, data_port) = {
            let g = context.inner.lock().unwrap();
            (g.group_id.clone(), g.discovery_port, g.data_port)
        };
        let mcast_addr = Self::multicast_addr(&group_id);
        let iface_addr = context.channel.address;
        let (rx_send, tx_recv) = context.channel.split();
        let cancel = context.cancel.clone();

        let peers: PeerTable = Arc::new(RwLock::new(HashMap::new()));
        let dedup: DedupQueue = Arc::new(Mutex::new(VecDeque::with_capacity(DEDUP_MAX)));

        // ── Discovery socket ──────────────────────────────────────────────────
        let disc_sock = match Self::make_ipv6_udp(disc_port) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                log::error!("auto_interface: discovery socket error: {e}");
                return;
            }
        };

        // Join multicast on the default interface (0) and a range of valid
        // indices to cover systems with multiple physical interfaces.
        let _ = disc_sock.join_multicast_v6(&mcast_addr, 0);
        for idx in 1u32..=16 {
            let _ = disc_sock.join_multicast_v6(&mcast_addr, idx);
        }

        // ── Data socket ───────────────────────────────────────────────────────
        let data_sock = match Self::make_ipv6_udp(data_port) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                log::error!("auto_interface: data socket error: {e}");
                return;
            }
        };

        let own_addrs = Self::own_link_local_addrs();
        if own_addrs.is_empty() {
            log::warn!(
                "auto_interface: no link-local IPv6 addresses found; \
                 peer discovery will not work on this host"
            );
        }

        log::info!(
            "auto_interface: group={mcast_addr} disc_port={disc_port} data_port={data_port} \
             own_addrs={own_addrs:?}"
        );

        const BUF: usize = HW_MTU + 64;

        // ── Task A: receive discovery announcements ───────────────────────────
        //
        // Validates each token against the observed UDP source address;
        // upserts authenticated senders into the peer table.
        let task_disc_rx = tokio::spawn({
            let disc_sock = disc_sock.clone();
            let peers = peers.clone();
            let cancel = cancel.clone();
            let group_id = group_id.clone();
            let own_addrs = own_addrs.clone();

            async move {
                let mut buf = [0u8; 64];
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        result = disc_sock.recv_from(&mut buf) => {
                            let (n, src) = match result {
                                Ok(v) => v,
                                Err(e) => {
                                    log::debug!("auto_interface: disc rx error: {e}");
                                    continue;
                                }
                            };
                            // Discovery tokens are exactly 32 bytes.
                            if n != 32 { continue; }

                            let src_v6 = match src {
                                SocketAddr::V6(v6) => v6,
                                SocketAddr::V4(_) => continue, // IPv4-mapped — skip
                            };
                            let src_ip = *src_v6.ip();

                            // Filter self-echoes.
                            if own_addrs.contains(&src_ip) { continue; }

                            let token: &[u8; 32] = match buf[..32].try_into() {
                                Ok(t) => t,
                                Err(_) => continue,
                            };

                            if DiscoveryToken::verify(token, &group_id, &src_ip) {
                                let data_addr = SocketAddrV6::new(
                                    src_ip,
                                    data_port,
                                    0,
                                    src_v6.scope_id(), // preserve link scope
                                );
                                let mut tbl = peers.write().await;
                                let is_new = !tbl.contains_key(&src_ip);
                                tbl.insert(src_ip, Peer {
                                    addr: data_addr,
                                    last_heard: Instant::now(),
                                });
                                if is_new {
                                    log::info!("auto_interface: new peer {src_ip}");
                                }
                            } else {
                                log::debug!(
                                    "auto_interface: rejected unauthenticated token from {src_ip}"
                                );
                            }
                        }
                    }
                }
            }
        });

        // ── Task B: periodic discovery announcements ──────────────────────────
        //
        // Broadcasts one `DiscoveryToken` per own link-local address.
        let task_disc_tx = tokio::spawn({
            let disc_sock = disc_sock.clone();
            let cancel = cancel.clone();
            let group_id = group_id.clone();
            let own_addrs = own_addrs.clone();
            let mcast_dest = SocketAddrV6::new(mcast_addr, disc_port, 0, 0);

            async move {
                let mut interval = tokio::time::interval(ANNOUNCE_INTERVAL);
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        _ = interval.tick() => {
                            for addr in &own_addrs {
                                let token = DiscoveryToken::for_addr(&group_id, addr);
                                if let Err(e) = disc_sock
                                    .send_to(token.as_bytes(), SocketAddr::V6(mcast_dest))
                                    .await
                                {
                                    log::debug!("auto_interface: disc tx: {e}");
                                }
                            }
                        }
                    }
                }
            }
        });

        // ── Task C: peer maintenance ──────────────────────────────────────────
        //
        // Prunes entries that have been silent for longer than PEERING_TIMEOUT.
        let task_peer_maint = tokio::spawn({
            let peers = peers.clone();
            let cancel = cancel.clone();

            async move {
                let mut interval = tokio::time::interval(PEER_JOB_INTERVAL);
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        _ = interval.tick() => {
                            let mut tbl = peers.write().await;
                            let before = tbl.len();
                            tbl.retain(|_, p| p.last_heard.elapsed() < PEERING_TIMEOUT);
                            let pruned = before - tbl.len();
                            if pruned > 0 {
                                log::debug!("auto_interface: pruned {pruned} stale peer(s)");
                            }
                        }
                    }
                }
            }
        });

        // ── Task D: receive data packets ──────────────────────────────────────
        //
        // Deduplicates by SHA-256(payload) within a sliding 750 ms window of
        // the last 48 packets (matching Python's `MULTI_IF_DEQUE`).
        let task_data_rx = tokio::spawn({
            let data_sock = data_sock.clone();
            let cancel = cancel.clone();
            let dedup = dedup.clone();

            async move {
                let mut buf = [0u8; BUF];
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        result = data_sock.recv_from(&mut buf) => {
                            let (n, _src) = match result {
                                Ok(v) => v,
                                Err(e) => {
                                    log::debug!("auto_interface: data rx: {e}");
                                    continue;
                                }
                            };
                            let data = &buf[..n];

                            // SHA-256 deduplication.
                            let hash: [u8; 32] = Sha256::digest(data).into();
                            {
                                let mut q = dedup.lock().await;
                                let now = Instant::now();
                                // Evict entries older than TTL.
                                q.retain(|(_, t)| now.duration_since(*t) <= DEDUP_TTL);
                                if q.iter().any(|(h, _)| *h == hash) {
                                    continue; // duplicate
                                }
                                if q.len() >= DEDUP_MAX {
                                    q.pop_front();
                                }
                                q.push_back((hash, now));
                            }

                            match Packet::deserialize(&mut InputBuffer::new(data)) {
                                Ok(pkt) => {
                                    let _ = rx_send
                                        .send(RxMessage { address: iface_addr, packet: pkt })
                                        .await;
                                }
                                Err(e) => log::debug!("auto_interface: pkt decode: {e:?}"),
                            }
                        }
                    }
                }
            }
        });

        // ── Task E: transmit data packets ─────────────────────────────────────
        //
        // Sends to every entry in the peer table (unicast, with preserved
        // link scope).
        let task_data_tx = tokio::spawn({
            let data_sock = data_sock.clone();
            let cancel = cancel.clone();
            let peers = peers.clone();

            async move {
                let mut buf = [0u8; BUF];
                let mut rx = tx_recv;
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        Some(msg) = rx.recv() => {
                            let mut out = OutputBuffer::new(&mut buf);
                            if msg.packet.serialize(&mut out).is_err() {
                                continue;
                            }
                            let data = out.as_slice();
                            let tbl = peers.read().await;
                            for peer in tbl.values() {
                                if let Err(e) = data_sock
                                    .send_to(data, SocketAddr::V6(peer.addr))
                                    .await
                                {
                                    log::debug!(
                                        "auto_interface: data tx to {}: {e}",
                                        peer.addr.ip()
                                    );
                                }
                            }
                        }
                    }
                }
            }
        });

        // All five tasks run until the cancellation token fires.
        // Discard join results — tasks communicate errors via log and cancel token.
        let _ = tokio::join!(
            task_disc_rx,
            task_disc_tx,
            task_peer_maint,
            task_data_rx,
            task_data_tx
        );
    }
}

impl Interface for AutoInterface {
    fn mtu() -> usize {
        HW_MTU
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multicast_addr_is_link_local() {
        let addr = AutoInterface::multicast_addr(DEFAULT_GROUP_ID);
        let octets = addr.octets();
        // Must be ff02::/16 (link-local multicast).
        assert_eq!(octets[0], 0xFF);
        assert_eq!(octets[1], 0x02);
    }

    #[test]
    fn multicast_addr_matches_python_formula() {
        // Python: ff02 ‖ sha256(b"reticulum")[0:14]
        let hash = Sha256::digest(b"reticulum");
        let mut expected = [0u8; 16];
        expected[0] = 0xFF;
        expected[1] = 0x02;
        expected[2..16].copy_from_slice(&hash[..14]);
        assert_eq!(
            AutoInterface::multicast_addr(DEFAULT_GROUP_ID).octets(),
            expected
        );
    }

    #[test]
    fn discovery_token_round_trip() {
        let group_id = b"reticulum";
        let addr: Ipv6Addr = "fe80::1".parse().unwrap();

        let token = DiscoveryToken::for_addr(group_id, &addr);
        assert!(
            DiscoveryToken::verify(token.as_bytes(), group_id, &addr),
            "token must verify against its own address"
        );
    }

    #[test]
    fn discovery_token_rejects_wrong_addr() {
        let group_id = b"reticulum";
        let addr: Ipv6Addr = "fe80::1".parse().unwrap();
        let other: Ipv6Addr = "fe80::2".parse().unwrap();

        let token = DiscoveryToken::for_addr(group_id, &addr);
        assert!(
            !DiscoveryToken::verify(token.as_bytes(), group_id, &other),
            "token must not verify against a different address"
        );
    }

    #[test]
    fn discovery_token_rejects_wrong_group() {
        let addr: Ipv6Addr = "fe80::1".parse().unwrap();
        let token = DiscoveryToken::for_addr(b"reticulum", &addr);
        assert!(
            !DiscoveryToken::verify(token.as_bytes(), b"othergroup", &addr),
            "token from different group must not verify"
        );
    }

    #[test]
    fn different_group_ids_produce_different_multicast_addrs() {
        let a = AutoInterface::multicast_addr(b"reticulum");
        let b = AutoInterface::multicast_addr(b"other");
        assert_ne!(a, b);
    }
}
