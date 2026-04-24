use std::collections::{BTreeMap, BTreeSet, VecDeque};

use getrandom::SysRng;

use tokio::time::Instant;

use reticulum_core::destination::DestinationName;
use reticulum_core::destination::PlainInputDestination;
use reticulum_core::hash::AddressHash;
use reticulum_core::hash::ADDRESS_HASH_SIZE;
use reticulum_core::identity::EmptyIdentity;
use reticulum_core::packet::DestinationType;
use reticulum_core::packet::Header;
use reticulum_core::packet::HeaderType;
use reticulum_core::packet::IfacFlag;
use reticulum_core::packet::Packet;
use reticulum_core::packet::PacketContext;
use reticulum_core::packet::PacketDataBuffer;
use reticulum_core::packet::PacketType;
use reticulum_core::packet::PropagationType;

pub fn create_path_request_destination() -> PlainInputDestination {
    PlainInputDestination::new(
        EmptyIdentity {},
        DestinationName::new("rnstransport", "path.request"),
    )
}

pub type TagBytes = Vec<u8>;

pub fn create_random_tag() -> TagBytes {
    AddressHash::try_new_from_rand(SysRng)
        .map(|h| h.as_slice().into())
        .unwrap_or_else(|_| AddressHash::new_empty().as_slice().into())
}

pub struct PathRequest {
    pub destination: AddressHash,
    pub requesting_transport: Option<AddressHash>,
    pub tag_bytes: TagBytes,
}

impl PathRequest {
    fn decode(data: &[u8], transport_name: &str) -> Option<Self> {
        if data.len() <= ADDRESS_HASH_SIZE {
            log::info!(
                "tp({}): ignoring malformed path request: no {}",
                transport_name,
                if data.len() < ADDRESS_HASH_SIZE {
                    "destination"
                } else {
                    "tag"
                }
            );
            return None;
        }

        let mut destination = [0u8; ADDRESS_HASH_SIZE];
        destination.copy_from_slice(&data[..ADDRESS_HASH_SIZE]);
        let destination = AddressHash::new(destination);

        let mut requesting_transport = None;
        let mut tag_start = ADDRESS_HASH_SIZE;
        let mut tag_end = data.len();

        if data.len() > ADDRESS_HASH_SIZE * 2 {
            requesting_transport = Some(AddressHash::new_from_slice(
                &data[ADDRESS_HASH_SIZE..2 * ADDRESS_HASH_SIZE],
            ));
            tag_start = ADDRESS_HASH_SIZE * 2;
        }

        if tag_end - tag_start > ADDRESS_HASH_SIZE {
            tag_end = tag_start + ADDRESS_HASH_SIZE;
        }

        let tag_bytes = data[tag_start..tag_end].into();

        Some(Self {
            destination,
            requesting_transport,
            tag_bytes,
        })
    }
}

/// Maximum recursive path requests forwarded in a sliding window of `ANNOUNCE_CAP_WINDOW`.
/// This implements the 2% bandwidth cap from the Reticulum spec:
/// path request packets are ~50-80 bytes; a conservative limit of 64 requests
/// per second covers most link types without flooding the network.
const RECURSIVE_REQUEST_CAP: usize = 64;
/// Sliding window duration for the cap.
const ANNOUNCE_CAP_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);

pub struct PathRequests {
    cache: BTreeSet<(AddressHash, TagBytes)>,
    name: String,
    transport_id: Option<AddressHash>,
    controlled_destination: PlainInputDestination,
    discovery: BTreeMap<AddressHash, Instant>,
    /// Timestamps of recent recursive path requests (for cap enforcement).
    recent_recursive: VecDeque<std::time::Instant>,
}

impl PathRequests {
    pub fn new(name: &str, transport_id: Option<AddressHash>) -> Self {
        Self {
            cache: BTreeSet::new(),
            name: name.into(),
            transport_id,
            controlled_destination: create_path_request_destination(),
            discovery: BTreeMap::new(),
            recent_recursive: VecDeque::new(),
        }
    }

    pub fn decode(&mut self, data: &[u8]) -> Option<PathRequest> {
        let path_request = PathRequest::decode(data, &self.name);

        if let Some(ref request) = path_request {
            let is_new = self
                .cache
                .insert((request.destination, request.tag_bytes.clone()));

            if !is_new {
                log::info!(
                    "tp({}): ignoring duplicate path request for destination {}",
                    self.name,
                    request.destination
                );
                return None;
            }
        }

        path_request
    }

    pub fn generate(&mut self, destination: &AddressHash, tag: Option<TagBytes>) -> Packet {
        let mut data = PacketDataBuffer::new_from_slice(destination.as_slice());

        if let Some(transport_id) = self.transport_id {
            data.safe_write(transport_id.as_slice());
        }

        data.safe_write(tag.unwrap_or_else(create_random_tag).as_slice());

        let destination = self.controlled_destination.desc.address_hash;

        Packet {
            header: Header {
                ifac_flag: IfacFlag::Open,
                header_type: HeaderType::Type1,
                propagation_type: PropagationType::Broadcast,
                destination_type: DestinationType::Plain,
                packet_type: PacketType::Data,
                hops: 0,
            },
            ifac: None,
            destination,
            transport: self.transport_id,
            context: PacketContext::None,
            data,
        }
    }

    fn allow_recursive(
        &mut self,
        destination: &AddressHash,
        _on_iface: Option<AddressHash>,
    ) -> bool {
        let now = Instant::now();

        if let Some(timeout) = self.discovery.get(destination) {
            if *timeout < now {
                log::info!(
                    "tp({}): rejecting discovery path request for destination {} as a request is already pending",
                    self.name,
                    destination
                );
                return false;
            }
        }

        // Enforce a cap on recursive path requests forwarded per second to avoid
        // network flooding (Reticulum spec: max 2% of interface bandwidth).
        let now_std = std::time::Instant::now();
        // Evict timestamps outside the sliding window.
        while let Some(front) = self.recent_recursive.front() {
            if now_std.duration_since(*front) > ANNOUNCE_CAP_WINDOW {
                self.recent_recursive.pop_front();
            } else {
                break;
            }
        }
        if self.recent_recursive.len() >= RECURSIVE_REQUEST_CAP {
            log::debug!(
                "tp({}): recursive path request for {} rejected by announce cap ({} in last {:?})",
                self.name,
                destination,
                self.recent_recursive.len(),
                ANNOUNCE_CAP_WINDOW,
            );
            return false;
        }
        self.recent_recursive.push_back(now_std);

        true
    }

    pub fn generate_recursive(
        &mut self,
        destination: &AddressHash,
        on_iface: Option<AddressHash>,
        tag: Option<TagBytes>,
    ) -> Option<Packet> {
        if self.allow_recursive(destination, on_iface) {
            log::trace!(
                "tp({}): sending discovery path request for {}",
                self.name,
                destination
            );

            Some(self.generate(destination, tag))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_request_roundtrip() {
        let mut testee = PathRequests::new("", None);

        let dest = AddressHash::try_new_from_rand(SysRng).expect("system RNG");

        let encoded = testee.generate(&dest, None);
        let decoded = testee.decode(encoded.data.as_slice()).unwrap();

        assert_eq!(decoded.destination, dest);
    }
}
