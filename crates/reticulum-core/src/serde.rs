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
        let mut header = Header::try_from_meta(buffer.read_byte()?)?;
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
        PacketContext::try_from(buffer.read_byte()?)
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

    // ── Codec hardening ──────────────────────────────────────────────────────
    //
    // These tests exercise the wire codec against arbitrary byte input
    // (deserialize must never panic) and against arbitrary structured packets
    // (serialize → deserialize must round-trip).  They are randomised rather
    // than a dedicated proptest dependency so the core crate stays no_std and
    // free of dev-cycle dependencies.

    use crate::packet::{
        DestinationType as DT, HeaderType as HT, IfacFlag as IF, PacketType as PT,
        PropagationType as PRT,
    };

    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn deserialize_never_panics_on_arbitrary_input() {
        let mut state: u64 = 0xdeadbeefcafebabe;
        for _ in 0..2048 {
            let len = (xorshift(&mut state) as usize) % (RETICULUM_MTU + 4);
            let mut bytes = std::vec::Vec::with_capacity(len);
            for _ in 0..len {
                bytes.push(xorshift(&mut state) as u8);
            }
            let mut input = InputBuffer::new(&bytes);
            // Must return Result, never panic.
            let _ = Packet::deserialize(&mut input);
        }
    }

    #[test]
    fn header_meta_round_trip_rejects_reserved() {
        // Walk every possible meta byte and make sure try_from_meta either
        // round-trips through to_meta or rejects with PacketError.
        for meta in 0u8..=255 {
            match Header::try_from_meta(meta) {
                Ok(hdr) => assert_eq!(hdr.to_meta(), meta, "meta {meta:08b} did not round-trip"),
                Err(_) => {
                    // Reserved propagation bits (0b10 / 0b11) must be the
                    // only reason a parse fails today.
                    let prop_bits = (meta >> 4) & 0b11;
                    assert!(
                        prop_bits == 0b10 || prop_bits == 0b11,
                        "unexpected rejection for meta {meta:08b}"
                    );
                }
            }
        }
    }

    #[test]
    fn packet_round_trip_random() {
        let mut state: u64 = 0x0123456789abcdef;
        for _ in 0..256 {
            let r = xorshift(&mut state);

            // Pick valid enum values (never reserved).
            let header = Header {
                ifac_flag: if r & 1 == 0 { IF::Open } else { IF::Authenticated },
                header_type: if (r >> 1) & 1 == 0 { HT::Type1 } else { HT::Type2 },
                propagation_type: if (r >> 2) & 1 == 0 { PRT::Broadcast } else { PRT::Transport },
                destination_type: match (r >> 3) & 0b11 {
                    0 => DT::Single,
                    1 => DT::Group,
                    2 => DT::Plain,
                    _ => DT::Link,
                },
                packet_type: match (r >> 5) & 0b11 {
                    0 => PT::Data,
                    1 => PT::Announce,
                    2 => PT::LinkRequest,
                    _ => PT::Proof,
                },
                hops: (r >> 7) as u8,
            };

            // Bound data so total wire size fits inside RETICULUM_MTU.
            let payload_len = ((xorshift(&mut state) as usize) % 64) + 1;
            let mut payload = std::vec::Vec::with_capacity(payload_len);
            for _ in 0..payload_len {
                payload.push(xorshift(&mut state) as u8);
            }

            let ifac = if header.ifac_flag == IF::Authenticated {
                let ifac_len = ((xorshift(&mut state) as usize) % 16) + 1;
                let mut buf = [0u8; crate::packet::PACKET_IFAC_MAX_LENGTH];
                for slot in buf.iter_mut().take(ifac_len) {
                    *slot = xorshift(&mut state) as u8;
                }
                Some(PacketIfac::try_new_from_slice(&buf[..ifac_len]).expect("valid ifac"))
            } else {
                None
            };

            let transport = if header.header_type == HT::Type2 {
                let mut bytes = [0u8; 16];
                for slot in &mut bytes {
                    *slot = xorshift(&mut state) as u8;
                }
                Some(AddressHash::new(bytes))
            } else {
                None
            };

            let mut dest_bytes = [0u8; 16];
            for slot in &mut dest_bytes {
                *slot = xorshift(&mut state) as u8;
            }

            let context = PacketContext::try_from((r >> 9) as u8).unwrap_or(PacketContext::None);

            let packet = Packet {
                header,
                ifac,
                destination: AddressHash::new(dest_bytes),
                transport,
                context,
                data: StaticBuffer::new_from_slice(&payload),
            };

            let mut buf = [0u8; RETICULUM_MTU];
            let mut out = OutputBuffer::new(&mut buf);
            packet.serialize(&mut out).expect("round-trip serialize");

            let mut input = InputBuffer::new(out.as_slice());
            let parsed = Packet::deserialize(&mut input).expect("round-trip deserialize");

            assert_eq!(parsed.header, packet.header);
            assert_eq!(parsed.ifac, packet.ifac);
            assert_eq!(parsed.transport, packet.transport);
            assert_eq!(parsed.destination, packet.destination);
            assert_eq!(parsed.context, packet.context);
            assert_eq!(parsed.data.as_slice(), packet.data.as_slice());
        }
    }
}
