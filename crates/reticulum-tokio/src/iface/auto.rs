//! AutoInterface — IPv6 link-local multicast peer discovery.
//!
//! Implements the same protocol as Python RNS `AutoInterface`:
//!
//! 1. A SHA-256-derived IPv6 link-local multicast group (`ff02::<hash14>`).
//! 2. Periodic discovery announcements: `SHA-256(group_id ‖ own_link_local_ip)`.
//! 3. Peer authentication: verify the received token against the UDP source.
//! 4. Data unicast to every live peer; deduplication via a sliding SHA-256 window.
//!
//! ## Lifecycle
//!
//! Call [`AutoInterface::connect`] to bind sockets and spawn discovery background
//! tasks.  The returned `AutoInterface` implements
//! `reticulum_core::interface::Interface`: `receive` reads from the data socket
//! (deduplicating) and `transmit` unicasts to every known peer.
//!
//! Background tasks are aborted when the `AutoInterface` is dropped.

use std::collections::{HashMap, VecDeque};
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::iface::TokioInterface;

// ── Protocol constants ─────────────────────────────────────────────────────────

const DEFAULT_GROUP_ID: &[u8] = b"reticulum";

pub const DISCOVERY_PORT: u16 = 29716;
pub const DATA_PORT: u16 = 42671;

const ANNOUNCE_INTERVAL: Duration = Duration::from_millis(1600);
const PEERING_TIMEOUT: Duration = Duration::from_secs(22);
const PEER_JOB_INTERVAL: Duration = Duration::from_secs(4);
const DEDUP_TTL: Duration = Duration::from_millis(750);
const DEDUP_MAX: usize = 48;

pub const HW_MTU: usize = 1196;

// ── Validated boundary: DiscoveryToken ────────────────────────────────────────

#[must_use]
struct DiscoveryToken([u8; 32]);

impl DiscoveryToken {
    fn for_addr(group_id: &[u8], addr: &Ipv6Addr) -> Self {
        let mut h = Sha256::new();
        h.update(group_id);
        h.update(addr.octets());
        Self(h.finalize().into())
    }

    fn verify(bytes: &[u8; 32], group_id: &[u8], observed_src: &Ipv6Addr) -> bool {
        let expected = Self::for_addr(group_id, observed_src);
        expected.0 == *bytes
    }

    fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

// ── Peer entry ────────────────────────────────────────────────────────────────

struct Peer {
    addr: SocketAddrV6,
    last_heard: Instant,
}

type PeerTable = Arc<RwLock<HashMap<Ipv6Addr, Peer>>>;

// ── AutoInterface ─────────────────────────────────────────────────────────────

pub struct AutoInterface {
    /// UDP socket used for data rx/tx with known peers.
    data_sock: UdpSocket,
    /// Live peer table, maintained by background discovery tasks.
    peers: PeerTable,
    #[allow(dead_code)]
    data_port: u16,
    /// SHA-256 deduplication queue (owned solely by the receive path).
    dedup: VecDeque<([u8; 32], Instant)>,
    /// Cancels the background discovery/maintenance tasks on drop.
    _bg_cancel: CancellationToken,
}

impl Drop for AutoInterface {
    fn drop(&mut self) {
        self._bg_cancel.cancel();
    }
}

impl AutoInterface {
    pub fn multicast_addr(group_id: &[u8]) -> Ipv6Addr {
        let hash = Sha256::digest(group_id);
        let mut bytes = [0u8; 16];
        bytes[0] = 0xFF;
        bytes[1] = 0x02;
        bytes[2..16].copy_from_slice(&hash[..14]);
        Ipv6Addr::from(bytes)
    }

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

    /// Bind sockets, join the multicast group, and spawn background discovery
    /// tasks.
    ///
    /// Returns a ready `AutoInterface` that implements
    /// `reticulum_core::interface::Interface`.
    pub fn connect(
        group: Option<String>,
        discovery_port: Option<u16>,
        data_port: Option<u16>,
    ) -> std::io::Result<Self> {
        let group_id: Vec<u8> = group
            .map(|g| g.into_bytes())
            .unwrap_or_else(|| DEFAULT_GROUP_ID.to_vec());
        let disc_port = discovery_port.unwrap_or(DISCOVERY_PORT);
        let data_port = data_port.unwrap_or(DATA_PORT);

        let mcast_addr = Self::multicast_addr(&group_id);

        let disc_sock = Arc::new(Self::make_ipv6_udp(disc_port)?);
        let data_sock = Self::make_ipv6_udp(data_port)?;

        let _ = disc_sock.join_multicast_v6(&mcast_addr, 0);
        for idx in 1u32..=16 {
            let _ = disc_sock.join_multicast_v6(&mcast_addr, idx);
        }

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

        let peers: PeerTable = Arc::new(RwLock::new(HashMap::new()));
        let bg_cancel = CancellationToken::new();

        // ── Task A: receive discovery announcements ───────────────────────────
        tokio::spawn({
            let disc_sock = disc_sock.clone();
            let peers = peers.clone();
            let cancel = bg_cancel.clone();
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
                                Err(e) => { log::debug!("auto_interface: disc rx: {e}"); continue; }
                            };
                            if n != 32 { continue; }
                            let src_v6 = match src {
                                SocketAddr::V6(v6) => v6,
                                SocketAddr::V4(_) => continue,
                            };
                            let src_ip = *src_v6.ip();
                            if own_addrs.contains(&src_ip) { continue; }
                            let token: &[u8; 32] = match buf[..32].try_into() {
                                Ok(t) => t,
                                Err(_) => continue,
                            };
                            if DiscoveryToken::verify(token, &group_id, &src_ip) {
                                let data_addr = SocketAddrV6::new(src_ip, data_port, 0, src_v6.scope_id());
                                let mut tbl = peers.write().await;
                                let is_new = !tbl.contains_key(&src_ip);
                                tbl.insert(src_ip, Peer { addr: data_addr, last_heard: Instant::now() });
                                if is_new { log::info!("auto_interface: new peer {src_ip}"); }
                            }
                        }
                    }
                }
            }
        });

        // ── Task B: periodic discovery announcements ──────────────────────────
        tokio::spawn({
            let disc_sock = disc_sock.clone();
            let cancel = bg_cancel.clone();
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
                                let _ = disc_sock
                                    .send_to(token.as_bytes(), SocketAddr::V6(mcast_dest))
                                    .await;
                            }
                        }
                    }
                }
            }
        });

        // ── Task C: peer maintenance ──────────────────────────────────────────
        tokio::spawn({
            let peers = peers.clone();
            let cancel = bg_cancel.clone();

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

        Ok(Self {
            data_sock,
            peers,
            data_port,
            dedup: VecDeque::with_capacity(DEDUP_MAX),
            _bg_cancel: bg_cancel,
        })
    }
}

impl TokioInterface for AutoInterface {
    type Error = std::io::Error;

    async fn receive(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        loop {
            let (n, _src) = self.data_sock.recv_from(buf).await?;

            // SHA-256 deduplication (matching Python's MULTI_IF_DEQUE).
            let hash: [u8; 32] = Sha256::digest(&buf[..n]).into();
            let now = Instant::now();
            self.dedup
                .retain(|(_, t)| now.duration_since(*t) <= DEDUP_TTL);
            if self.dedup.iter().any(|(h, _)| *h == hash) {
                continue; // duplicate
            }
            if self.dedup.len() >= DEDUP_MAX {
                self.dedup.pop_front();
            }
            self.dedup.push_back((hash, now));

            return Ok(n);
        }
    }

    async fn transmit(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        let peers = self.peers.read().await;
        for peer in peers.values() {
            if let Err(e) = self
                .data_sock
                .send_to(frame, SocketAddr::V6(peer.addr))
                .await
            {
                log::debug!("auto_interface: tx to {}: {e}", peer.addr.ip());
            }
        }
        Ok(())
    }

    fn mtu(&self) -> usize {
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
        assert_eq!(octets[0], 0xFF);
        assert_eq!(octets[1], 0x02);
    }

    #[test]
    fn multicast_addr_matches_python_formula() {
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
        assert!(DiscoveryToken::verify(token.as_bytes(), group_id, &addr));
    }

    #[test]
    fn discovery_token_rejects_wrong_addr() {
        let group_id = b"reticulum";
        let addr: Ipv6Addr = "fe80::1".parse().unwrap();
        let other: Ipv6Addr = "fe80::2".parse().unwrap();
        let token = DiscoveryToken::for_addr(group_id, &addr);
        assert!(!DiscoveryToken::verify(token.as_bytes(), group_id, &other));
    }

    #[test]
    fn discovery_token_rejects_wrong_group() {
        let addr: Ipv6Addr = "fe80::1".parse().unwrap();
        let token = DiscoveryToken::for_addr(b"reticulum", &addr);
        assert!(!DiscoveryToken::verify(
            token.as_bytes(),
            b"othergroup",
            &addr
        ));
    }

    #[test]
    fn different_group_ids_produce_different_multicast_addrs() {
        let a = AutoInterface::multicast_addr(b"reticulum");
        let b = AutoInterface::multicast_addr(b"other");
        assert_ne!(a, b);
    }
}
