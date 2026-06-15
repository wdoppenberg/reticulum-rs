#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod buffer;
pub mod channel;
pub mod clock;
pub mod crypt;
pub mod destination;
pub mod error;
pub mod hash;
pub mod hdlc;
pub mod identity;
pub mod interface;
pub mod link;
pub mod packet;
pub mod request;
pub mod resource;
pub mod routing;
pub mod serde;
#[cfg(any(feature = "alloc", feature = "heapless"))]
pub mod transport;
