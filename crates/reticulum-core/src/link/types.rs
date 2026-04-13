use sha2::Digest;

use crate::{
    buffer::StaticBuffer,
    hash::{AddressHash, Hash},
    packet::{Packet, PacketContext, PACKET_MDU, PUBLIC_KEY_LENGTH},
};

pub const LINK_MTU_SIZE: usize = 3;

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum LinkStatus {
    Pending = 0x00,
    Handshake = 0x01,
    Active = 0x02,
    Stale = 0x03,
    Closed = 0x04,
}

impl LinkStatus {
    pub fn not_yet_active(&self) -> bool {
        *self == LinkStatus::Pending || *self == LinkStatus::Handshake
    }
}

pub type LinkId = AddressHash;

pub type LinkPayload<const N: usize = PACKET_MDU> = StaticBuffer<N>;

impl From<&Packet> for LinkId {
    fn from(packet: &Packet) -> Self {
        let data = packet.data.as_slice();
        let data_diff = if data.len() > PUBLIC_KEY_LENGTH * 2 {
            data.len() - PUBLIC_KEY_LENGTH * 2
        } else {
            0
        };

        let hashable_data = &data[..data.len() - data_diff];

        AddressHash::new_from_hash(&Hash::new(
            Hash::generator()
                .chain_update([packet.header.to_meta() & 0b00001111])
                .chain_update(packet.destination.as_slice())
                .chain_update([packet.context as u8])
                .chain_update(hashable_data)
                .finalize()
                .into(),
        ))
    }
}

pub enum LinkHandleResult {
    None,
    Activated,
    KeepAlive,
}

/// Control-plane events for a link.  No payload — cheap to clone and broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    Activated,
    Closed,
}

/// Distinguishes which logical data stream a [`LinkDataFrame`] belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataKind {
    Data,
    ChannelData,
    ResourceData,
}

/// Data-plane frame.  Wrapped in `Arc` before being broadcast so that
/// cloning the broadcast message copies only a pointer, not the payload.
#[derive(Debug, Clone)]
pub struct LinkDataFrame<const N: usize = PACKET_MDU> {
    pub kind: DataKind,
    pub context: PacketContext,
    pub payload: LinkPayload<N>,
}
