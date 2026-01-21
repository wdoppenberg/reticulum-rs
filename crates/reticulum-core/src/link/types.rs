use core::cmp::min;
use sha2::Digest;

use crate::{
    hash::AddressHash,
    hash::Hash,
    packet::{Packet, PACKET_MDU, PUBLIC_KEY_LENGTH},
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

#[derive(Clone)]
pub struct LinkPayload {
    buffer: [u8; PACKET_MDU],
    len: usize,
}

impl LinkPayload {
    pub fn new() -> Self {
        Self {
            buffer: [0u8; PACKET_MDU],
            len: 0,
        }
    }

    pub fn new_from_slice(data: &[u8]) -> Self {
        let mut buffer = [0u8; PACKET_MDU];

        let len = min(data.len(), buffer.len());

        buffer[..len].copy_from_slice(&data[..len]);

        Self { buffer, len }
    }

    #[cfg(feature = "alloc")]
    pub fn new_from_vec(data: &alloc::vec::Vec<u8>) -> Self {
        let mut buffer = [0u8; PACKET_MDU];

        for i in 0..min(buffer.len(), data.len()) {
            buffer[i] = data[i];
        }

        Self {
            buffer,
            len: data.len(),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buffer[..self.len]
    }
}

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
                .chain_update(&[packet.header.to_meta() & 0b00001111])
                .chain_update(packet.destination.as_slice())
                .chain_update(&[packet.context as u8])
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

#[derive(Clone)]
pub enum LinkEvent {
    Activated,
    Data(LinkPayload),
    Closed,
}
