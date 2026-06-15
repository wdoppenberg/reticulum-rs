//! Path-discovery request generation and deduplication.
//!
//! Requires the `alloc` feature: path requests carry a variable-length tag
//! (`Vec<u8>`) and track a duplicate-suppression cache that cannot be
//! expressed with a fixed-size `heapless` type without additional design
//! constraints.  The other transport tables expose `heapless` alternatives;
//! this one intentionally does not.

#[cfg(feature = "alloc")]
use alloc::collections::{BTreeMap, BTreeSet};
#[cfg(feature = "alloc")]
use alloc::collections::VecDeque;
#[cfg(feature = "alloc")]
use alloc::string::String;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use rand_core::TryCryptoRng;

use crate::destination::{DestinationName, PlainInputDestination};
use crate::hash::{AddressHash, ADDRESS_HASH_SIZE};
use crate::identity::EmptyIdentity;
use crate::packet::{
    DestinationType, Header, HeaderType, IfacFlag, Packet, PacketContext, PacketDataBuffer,
    PacketType, PropagationType,
};

#[cfg(feature = "alloc")]
pub fn create_path_request_destination() -> PlainInputDestination {
    PlainInputDestination::new(
        EmptyIdentity {},
        DestinationName::new("rnstransport", "path.request"),
    )
}

#[cfg(feature = "alloc")]
pub type TagBytes = Vec<u8>;

/// Generate a random tag for path requests using the supplied RNG.
#[cfg(feature = "alloc")]
pub fn create_random_tag<R: TryCryptoRng>(mut rng: R) -> TagBytes {
    let mut bytes = [0u8; ADDRESS_HASH_SIZE];
    if rng.try_fill_bytes(&mut bytes).is_ok() {
        bytes.to_vec()
    } else {
        AddressHash::new_empty().as_slice().to_vec()
    }
}

#[cfg(feature = "alloc")]
pub struct PathRequest {
    pub destination: AddressHash,
    pub requesting_transport: Option<AddressHash>,
    pub tag_bytes: TagBytes,
}

#[cfg(feature = "alloc")]
impl PathRequest {
    fn decode(data: &[u8], transport_name: &str) -> Option<Self> {
        if data.len() <= ADDRESS_HASH_SIZE {
            log::info!(
                "tp({}): ignoring malformed path request: no {}",
                transport_name,
                if data.len() < ADDRESS_HASH_SIZE { "destination" } else { "tag" }
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

        Some(Self {
            destination,
            requesting_transport,
            tag_bytes: data[tag_start..tag_end].to_vec(),
        })
    }
}

const RECURSIVE_REQUEST_CAP: usize = 64;
const ANNOUNCE_CAP_WINDOW_MS: u64 = 1_000;

#[cfg(feature = "alloc")]
pub struct PathRequests {
    cache: BTreeSet<(AddressHash, TagBytes)>,
    name: String,
    transport_id: Option<AddressHash>,
    controlled_destination: PlainInputDestination,
    discovery: BTreeMap<AddressHash, u64>,
    recent_recursive: VecDeque<u64>,
}

#[cfg(feature = "alloc")]
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
            if !self.cache.insert((request.destination, request.tag_bytes.clone())) {
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

    pub fn generate(&mut self, destination: &AddressHash, tag: TagBytes) -> Packet {
        let mut data = PacketDataBuffer::new_from_slice(destination.as_slice());
        if let Some(transport_id) = self.transport_id {
            data.safe_write(transport_id.as_slice());
        }
        data.safe_write(tag.as_slice());

        let dest_addr = self.controlled_destination.desc.address_hash;

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
            destination: dest_addr,
            transport: self.transport_id,
            context: PacketContext::None,
            data,
        }
    }

    fn allow_recursive(
        &mut self,
        destination: &AddressHash,
        _on_iface: Option<AddressHash>,
        now_ms: u64,
    ) -> bool {
        if let Some(&timeout_ms) = self.discovery.get(destination) {
            if timeout_ms > now_ms {
                log::info!(
                    "tp({}): rejecting discovery path request for destination {} — request already pending",
                    self.name, destination
                );
                return false;
            }
        }

        while let Some(&front_ms) = self.recent_recursive.front() {
            if now_ms.saturating_sub(front_ms) > ANNOUNCE_CAP_WINDOW_MS {
                self.recent_recursive.pop_front();
            } else {
                break;
            }
        }

        if self.recent_recursive.len() >= RECURSIVE_REQUEST_CAP {
            log::debug!(
                "tp({}): recursive path request for {} rejected by announce cap ({} in last {}ms)",
                self.name, destination, self.recent_recursive.len(), ANNOUNCE_CAP_WINDOW_MS,
            );
            return false;
        }

        self.recent_recursive.push_back(now_ms);
        true
    }

    pub fn generate_recursive(
        &mut self,
        destination: &AddressHash,
        on_iface: Option<AddressHash>,
        tag: TagBytes,
        now_ms: u64,
    ) -> Option<Packet> {
        if self.allow_recursive(destination, on_iface, now_ms) {
            log::trace!("tp({}): sending discovery path request for {}", self.name, destination);
            Some(self.generate(destination, tag))
        } else {
            None
        }
    }
}

#[cfg(all(test, feature = "alloc"))]
mod tests {
    use super::*;

    #[derive(Copy, Clone)]
    struct CounterRng(u64);
    impl CounterRng {
        fn new() -> Self { Self(0xABCD1234) }
    }

    #[derive(Debug)]
    struct CounterRngErr;
    impl core::fmt::Display for CounterRngErr {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { f.write_str("err") }
    }
    impl core::error::Error for CounterRngErr {}

    impl rand_core::TryRng for CounterRng {
        type Error = CounterRngErr;
        fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            Ok((self.0 >> 32) as u32)
        }
        fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
            let lo = self.try_next_u32()? as u64;
            let hi = self.try_next_u32()? as u64;
            Ok((hi << 32) | lo)
        }
        fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
            for chunk in dst.chunks_mut(8) {
                let n = self.try_next_u64()?;
                for (i, b) in chunk.iter_mut().enumerate() {
                    *b = (n >> (i * 8)) as u8;
                }
            }
            Ok(())
        }
    }
    impl rand_core::TryCryptoRng for CounterRng {}

    #[test]
    fn path_request_roundtrip() {
        let mut testee = PathRequests::new("", None);
        let dest = AddressHash::new_empty();
        let tag = create_random_tag(CounterRng::new());
        let encoded = testee.generate(&dest, tag);
        let decoded = testee.decode(encoded.data.as_slice()).unwrap();
        assert_eq!(decoded.destination, dest);
    }
}
