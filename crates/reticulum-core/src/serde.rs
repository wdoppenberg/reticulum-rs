use crate::{
    buffer::{InputBuffer, OutputBuffer, StaticBuffer},
    error::RnsError,
    hash::AddressHash,
    packet::{Header, HeaderType, IfacFlag, Packet, PacketContext, PacketIfac, RETICULUM_MTU},
};

pub trait Serialize {
    fn serialize(&self, buffer: &mut OutputBuffer) -> Result<usize, RnsError>;
}

impl Serialize for AddressHash {
    fn serialize(&self, buffer: &mut OutputBuffer) -> Result<usize, RnsError> {
        buffer.write(self.as_slice())
    }
}

impl Serialize for Header {
    fn serialize(&self, buffer: &mut OutputBuffer) -> Result<usize, RnsError> {
        buffer.write(&[self.to_meta(), self.hops])
    }
}
impl Serialize for PacketContext {
    fn serialize(&self, buffer: &mut OutputBuffer) -> Result<usize, RnsError> {
        buffer.write(&[*self as u8])
    }
}

impl Serialize for Packet {
    fn serialize(&self, buffer: &mut OutputBuffer) -> Result<usize, RnsError> {
        let size_hint = self.wire_size_hint()?;
        if size_hint > RETICULUM_MTU {
            return Err(RnsError::InvalidArgument);
        }

        self.header.serialize(buffer)?;

        if self.header.ifac_flag == IfacFlag::Authenticated {
            let ifac = self.ifac.ok_or(RnsError::InvalidArgument)?;
            if ifac.length == 0 || ifac.length > u8::MAX as usize {
                return Err(RnsError::InvalidArgument);
            }
            buffer.write_byte(ifac.length as u8)?;
            buffer.write(ifac.as_slice())?;
        }

        if self.header.header_type == HeaderType::Type2 {
            if let Some(transport) = &self.transport {
                transport.serialize(buffer)?;
            } else {
                return Err(RnsError::InvalidArgument);
            }
        }

        self.destination.serialize(buffer)?;

        self.context.serialize(buffer)?;

        buffer.write(self.data.as_slice())
    }
}

impl Header {
    pub fn deserialize(buffer: &mut InputBuffer) -> Result<Header, RnsError> {
        let mut header = Header::from_meta(buffer.read_byte()?);
        header.hops = buffer.read_byte()?;

        Ok(header)
    }
}

impl AddressHash {
    pub fn deserialize(buffer: &mut InputBuffer) -> Result<AddressHash, RnsError> {
        let mut address = AddressHash::new_empty();

        buffer.read(address.as_mut_slice())?;

        Ok(address)
    }
}

impl PacketContext {
    pub fn deserialize(buffer: &mut InputBuffer) -> Result<PacketContext, RnsError> {
        Ok(PacketContext::from(buffer.read_byte()?))
    }
}
impl Packet {
    pub fn deserialize(buffer: &mut InputBuffer) -> Result<Packet, RnsError> {
        if buffer.bytes_left() > RETICULUM_MTU {
            return Err(RnsError::InvalidArgument);
        }

        let header = Header::deserialize(buffer)?;

        let ifac = if header.ifac_flag == IfacFlag::Authenticated {
            let ifac_len = buffer.read_byte()? as usize;
            if ifac_len == 0 {
                return Err(RnsError::InvalidArgument);
            }

            let ifac_bytes = buffer.read_slice(ifac_len)?;
            Some(PacketIfac::try_new_from_slice(ifac_bytes)?)
        } else {
            None
        };

        let transport = if header.header_type == HeaderType::Type2 {
            Some(AddressHash::deserialize(buffer)?)
        } else {
            None
        };

        let destination = AddressHash::deserialize(buffer)?;

        let context = PacketContext::deserialize(buffer)?;

        let mut packet = Packet {
            header,
            ifac,
            destination,
            transport,
            context,
            data: StaticBuffer::new(),
        };

        buffer.read(
            packet
                .data
                .acquire_buf(buffer.bytes_left())
                .map_err(|_| RnsError::OutOfMemory)?,
        )?;

        if packet.wire_size_hint()? > RETICULUM_MTU {
            return Err(RnsError::InvalidArgument);
        }

        Ok(packet)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use getrandom::SysRng;
    use std::println;

    use crate::{
        buffer::{InputBuffer, OutputBuffer, StaticBuffer},
        hash::AddressHash,
        packet::{
            DestinationType, Header, HeaderType, IfacFlag, Packet, PacketContext, PacketIfac,
            PacketType, PropagationType, RETICULUM_MTU,
        },
    };

    use super::Serialize;

    #[test]
    fn serialize_packet() {
        let mut output_data = [0u8; 4096];

        let mut buffer = OutputBuffer::new(&mut output_data);

        let packet = Packet {
            header: Header {
                ifac_flag: IfacFlag::Open,
                header_type: HeaderType::Type1,
                propagation_type: PropagationType::Broadcast,
                destination_type: DestinationType::Single,
                packet_type: PacketType::Announce,
                hops: 0,
            },
            ifac: None,
            destination: AddressHash::try_new_from_rand(SysRng).expect("system RNG"),
            transport: None,
            context: PacketContext::None,
            data: StaticBuffer::new(),
        };

        packet.serialize(&mut buffer).expect("serialized packet");

        println!("{}", buffer);
    }

    #[test]
    fn deserialize_packet() {
        let mut output_data = [0u8; 4096];

        let mut buffer = OutputBuffer::new(&mut output_data);

        let mut packet = Packet {
            header: Header {
                ifac_flag: IfacFlag::Open,
                header_type: HeaderType::Type1,
                propagation_type: PropagationType::Broadcast,
                destination_type: DestinationType::Single,
                packet_type: PacketType::Announce,
                hops: 0,
            },
            ifac: None,
            destination: AddressHash::try_new_from_rand(SysRng).expect("system RNG"),
            transport: None,
            context: PacketContext::None,
            data: StaticBuffer::new(),
        };

        packet.data.safe_write(b"Hello, world!");

        packet.serialize(&mut buffer).expect("serialized packet");

        let mut input_buffer = InputBuffer::new(buffer.as_slice());

        let new_packet = Packet::deserialize(&mut input_buffer).expect("deserialized packet");

        assert_eq!(packet.header, new_packet.header);
        assert_eq!(packet.destination, new_packet.destination);
        assert_eq!(packet.transport, new_packet.transport);
        assert_eq!(packet.context, new_packet.context);
        assert_eq!(packet.data.as_slice(), new_packet.data.as_slice());
    }

    #[test]
    fn serialize_deserialize_ifac_packet() {
        let mut output_data = [0u8; 4096];
        let mut buffer = OutputBuffer::new(&mut output_data);

        let packet = Packet {
            header: Header {
                ifac_flag: IfacFlag::Authenticated,
                ..Default::default()
            },
            ifac: Some(PacketIfac::try_new_from_slice(&[0xAA, 0xBB, 0xCC]).expect("valid ifac")),
            destination: AddressHash::try_new_from_rand(SysRng).expect("system RNG"),
            transport: None,
            context: PacketContext::None,
            data: StaticBuffer::new_from_slice(b"ifac"),
        };

        packet.serialize(&mut buffer).expect("serialized packet");

        let mut input_buffer = InputBuffer::new(buffer.as_slice());
        let parsed = Packet::deserialize(&mut input_buffer).expect("deserialized packet");

        assert_eq!(parsed.header.ifac_flag, IfacFlag::Authenticated);
        assert_eq!(parsed.ifac.expect("ifac").as_slice(), &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn reject_oversized_packet_for_wire() {
        let mut output_data = [0u8; 4096];
        let mut buffer = OutputBuffer::new(&mut output_data);

        let packet = Packet {
            header: Header {
                ifac_flag: IfacFlag::Open,
                ..Default::default()
            },
            ifac: None,
            destination: AddressHash::try_new_from_rand(SysRng).expect("system RNG"),
            transport: None,
            context: PacketContext::None,
            data: StaticBuffer::new_from_slice(&[0x42; RETICULUM_MTU]),
        };

        assert!(packet.serialize(&mut buffer).is_err());
    }
}
