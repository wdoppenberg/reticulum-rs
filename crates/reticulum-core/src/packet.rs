use core::fmt;

use sha2::Digest;

use crate::buffer::StaticBuffer;
use crate::error::RnsError;
use crate::hash::AddressHash;
use crate::hash::Hash;

pub use crate::identity::PUBLIC_KEY_LENGTH;

/// Maximum payload bytes carried inside a single [`Packet`].
///
/// Sized to fit one wire frame at [`RETICULUM_MTU`].  Keeping this small is
/// important on embedded targets: every `Packet` value carries an inline
/// [`PacketDataBuffer`] of this size on the stack.
pub const PACKET_MDU: usize = 512usize;
pub const PACKET_IFAC_MAX_LENGTH: usize = 64usize;
pub const RETICULUM_MTU: usize = 500usize;

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum IfacFlag {
    Open = 0b0,
    Authenticated = 0b1,
}

impl TryFrom<u8> for IfacFlag {
    type Error = RnsError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(IfacFlag::Open),
            1 => Ok(IfacFlag::Authenticated),
            _ => Err(RnsError::PacketError),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum HeaderType {
    Type1 = 0b0,
    Type2 = 0b1,
}

impl TryFrom<u8> for HeaderType {
    type Error = RnsError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(HeaderType::Type1),
            1 => Ok(HeaderType::Type2),
            _ => Err(RnsError::PacketError),
        }
    }
}

/// Packet propagation mode.
///
/// The wire encoding uses 2 bits; values `0b10` and `0b11` are reserved by
/// the Reticulum specification and rejected by [`TryFrom<u8>`].
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum PropagationType {
    Broadcast = 0b00,
    Transport = 0b01,
}

impl TryFrom<u8> for PropagationType {
    type Error = RnsError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0b00 => Ok(PropagationType::Broadcast),
            0b01 => Ok(PropagationType::Transport),
            // 0b10 and 0b11 are reserved by spec — reject on the wire.
            _ => Err(RnsError::PacketError),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum DestinationType {
    Single = 0b00,
    Group = 0b01,
    Plain = 0b10,
    Link = 0b11,
}

impl TryFrom<u8> for DestinationType {
    type Error = RnsError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0b00 => Ok(DestinationType::Single),
            0b01 => Ok(DestinationType::Group),
            0b10 => Ok(DestinationType::Plain),
            0b11 => Ok(DestinationType::Link),
            _ => Err(RnsError::PacketError),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum PacketType {
    Data = 0b00,
    Announce = 0b01,
    LinkRequest = 0b10,
    Proof = 0b11,
}

impl TryFrom<u8> for PacketType {
    type Error = RnsError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0b00 => Ok(PacketType::Data),
            0b01 => Ok(PacketType::Announce),
            0b10 => Ok(PacketType::LinkRequest),
            0b11 => Ok(PacketType::Proof),
            _ => Err(RnsError::PacketError),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum PacketContext {
    None = 0x00,                    // Generic data packet
    Resource = 0x01,                // Packet is part of a resource
    ResourceAdvertisement = 0x02,   // Packet is a resource advertisement
    ResourceRequest = 0x03,         // Packet is a resource part request
    ResourceHashUpdate = 0x04,      // Packet is a resource hashmap update
    ResourceProof = 0x05,           // Packet is a resource proof
    ResourceInitiatorCancel = 0x06, // Packet is a resource initiator cancel message
    ResourceReceiverCancel = 0x07,  // Packet is a resource receiver cancel message
    CacheRequest = 0x08,            // Packet is a cache request
    Request = 0x09,                 // Packet is a request
    Response = 0x0A,                // Packet is a response to a request
    PathResponse = 0x0B,            // Packet is a response to a path request
    Command = 0x0C,                 // Packet is a command
    CommandStatus = 0x0D,           // Packet is a status of an executed command
    Channel = 0x0E,                 // Packet contains link channel data
    KeepAlive = 0xFA,               // Packet is a keepalive packet
    LinkIdentify = 0xFB,            // Packet is a link peer identification proof
    LinkClose = 0xFC,               // Packet is a link close message
    LinkProof = 0xFD,               // Packet is a link packet proof
    LinkRTT = 0xFE,                 // Packet is a link request round-trip time measurement
    LinkRequestProof = 0xFF,        // Packet is a link request proof
}

impl TryFrom<u8> for PacketContext {
    type Error = RnsError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(PacketContext::None),
            0x01 => Ok(PacketContext::Resource),
            0x02 => Ok(PacketContext::ResourceAdvertisement),
            0x03 => Ok(PacketContext::ResourceRequest),
            0x04 => Ok(PacketContext::ResourceHashUpdate),
            0x05 => Ok(PacketContext::ResourceProof),
            0x06 => Ok(PacketContext::ResourceInitiatorCancel),
            0x07 => Ok(PacketContext::ResourceReceiverCancel),
            0x08 => Ok(PacketContext::CacheRequest),
            0x09 => Ok(PacketContext::Request),
            0x0A => Ok(PacketContext::Response),
            0x0B => Ok(PacketContext::PathResponse),
            0x0C => Ok(PacketContext::Command),
            0x0D => Ok(PacketContext::CommandStatus),
            0x0E => Ok(PacketContext::Channel),
            0xFA => Ok(PacketContext::KeepAlive),
            0xFB => Ok(PacketContext::LinkIdentify),
            0xFC => Ok(PacketContext::LinkClose),
            0xFD => Ok(PacketContext::LinkProof),
            0xFE => Ok(PacketContext::LinkRTT),
            0xFF => Ok(PacketContext::LinkRequestProof),
            _ => Err(RnsError::PacketError),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct Header {
    pub ifac_flag: IfacFlag,
    pub header_type: HeaderType,
    pub propagation_type: PropagationType,
    pub destination_type: DestinationType,
    pub packet_type: PacketType,
    pub hops: u8,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            ifac_flag: IfacFlag::Open,
            header_type: HeaderType::Type1,
            propagation_type: PropagationType::Broadcast,
            destination_type: DestinationType::Single,
            packet_type: PacketType::Data,
            hops: 0,
        }
    }
}

impl Header {
    #[must_use]
    pub fn to_meta(&self) -> u8 {
        (self.ifac_flag as u8) << 7
            | (self.header_type as u8) << 6
            | (self.propagation_type as u8) << 4
            | (self.destination_type as u8) << 2
            | (self.packet_type as u8)
    }

    /// Parse a wire-format header meta byte.
    ///
    /// Returns [`RnsError::PacketError`] if any of the bit fields decode to a
    /// reserved or otherwise invalid value (e.g. reserved propagation modes
    /// `0b10`/`0b11`).
    pub fn try_from_meta(meta: u8) -> Result<Self, RnsError> {
        Ok(Self {
            ifac_flag: IfacFlag::try_from((meta >> 7) & 0b1)?,
            header_type: HeaderType::try_from((meta >> 6) & 0b1)?,
            propagation_type: PropagationType::try_from((meta >> 4) & 0b11)?,
            destination_type: DestinationType::try_from((meta >> 2) & 0b11)?,
            packet_type: PacketType::try_from(meta & 0b11)?,
            hops: 0,
        })
    }
}

impl fmt::Display for Header {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:b}{:b}{:0>2b}{:0>2b}{:0>2b}.{}",
            self.ifac_flag as u8,
            self.header_type as u8,
            self.propagation_type as u8,
            self.destination_type as u8,
            self.packet_type as u8,
            self.hops,
        )
    }
}

pub type PacketDataBuffer = StaticBuffer<PACKET_MDU>;

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct PacketIfac {
    pub access_code: [u8; PACKET_IFAC_MAX_LENGTH],
    pub length: usize,
}

impl PacketIfac {
    pub fn try_new_from_slice(slice: &[u8]) -> Result<Self, RnsError> {
        if slice.is_empty() || slice.len() > PACKET_IFAC_MAX_LENGTH {
            return Err(RnsError::InvalidArgument);
        }

        let mut access_code = [0u8; PACKET_IFAC_MAX_LENGTH];
        access_code[..slice.len()].copy_from_slice(slice);
        Ok(Self {
            access_code,
            length: slice.len(),
        })
    }

    pub fn new_from_slice(slice: &[u8]) -> Self {
        debug_assert!(
            !slice.is_empty() && slice.len() <= PACKET_IFAC_MAX_LENGTH,
            "PacketIfac::new_from_slice expects 1..=PACKET_IFAC_MAX_LENGTH bytes"
        );
        let mut access_code = [0u8; PACKET_IFAC_MAX_LENGTH];
        let length = core::cmp::min(slice.len(), PACKET_IFAC_MAX_LENGTH);
        access_code[..length].copy_from_slice(&slice[..length]);
        Self {
            access_code,
            length,
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.access_code[..self.length]
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct Packet {
    pub header: Header,
    pub ifac: Option<PacketIfac>,
    pub destination: AddressHash,
    pub transport: Option<AddressHash>,
    pub context: PacketContext,
    pub data: PacketDataBuffer,
}

impl Packet {
    /// An empty placeholder packet — broadcast Data with no destination set.
    ///
    /// Intended for unit tests, ring-buffer slots, and other "fill me in
    /// later" scenarios.  Production code should construct packets with the
    /// destination, header, and data fields set explicitly.
    pub fn new_empty() -> Self {
        Self {
            header: Header::default(),
            ifac: None,
            destination: AddressHash::new_empty(),
            transport: None,
            context: PacketContext::None,
            data: PacketDataBuffer::new(),
        }
    }

    pub fn wire_size_hint(&self) -> Result<usize, RnsError> {
        let mut size = 2usize; // header meta + hops

        if self.header.ifac_flag == IfacFlag::Authenticated {
            let ifac = self.ifac.ok_or(RnsError::InvalidArgument)?;
            if ifac.length == 0 || ifac.length > PACKET_IFAC_MAX_LENGTH {
                return Err(RnsError::InvalidArgument);
            }
            size += 1 + ifac.length; // length prefix + IFAC bytes
        } else if self.ifac.is_some() {
            return Err(RnsError::InvalidArgument);
        }

        if self.header.header_type == HeaderType::Type2 {
            size += AddressHash::new_empty().len();
        }

        size += AddressHash::new_empty().len(); // destination
        size += 1; // context
        size += self.data.len();

        Ok(size)
    }

    pub fn hash(&self) -> Hash {
        Hash::new(
            Hash::generator()
                .chain_update([self.header.to_meta() & 0b00001111])
                .chain_update(self.destination.as_slice())
                .chain_update([self.context as u8])
                .chain_update(self.data.as_slice())
                .finalize()
                .into(),
        )
    }
}

impl fmt::Display for Packet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}", self.header)?;

        if let Some(transport) = self.transport {
            write!(f, " {}", transport)?;
        }

        write!(f, " {}", self.destination)?;

        write!(f, " 0x[{}]]", self.data.len())?;

        Ok(())
    }
}
