//! Addressing
//!
//! Addresses in Ergot have three main components:
//!
//! * A 16-bit **Network ID**
//! * An 8-bit **Node ID**
//! * An 8-bit **Socket ID**
//!
//! This addressing is similar in form to AppleTalk's addressing. This
//! addressing is quite different to how TCP/IP IPv4 addressing works.
//!
//! ### Network IDs
//!
//! Network IDs represent a single "network segment", where all nodes of a
//! network segment can hear all messages sent on that segment.
//!
//! For example, in a point-to-point link (e.g. UART, TCP, USB), that link will
//! be a single Network ID, containing two nodes. In a bus-style link
//! (e.g. RS-485, I2C), the bus will be a single Network ID, with one or more
//! nodes residing on that link.
//!
//! The Network ID of "0" is reserved, and is generally used when sending
//! messages within the local device, or used to mean "the current network
//! segment" before an interface has discovered the Network ID of the Network
//! Segment it resides on.
//!
//! The Network ID of "65535" is reserved.
//!
//! Network IDs are intended to be discovered/negotiated at runtime, and are
//! not typically hardcoded. The general process of negotiating Network IDs,
//! particularly across multiple network segment hops, is not yet defined.
//!
//! Networks that require more than 65534 network segments are not supported
//! by Ergot. At that point, you should probably just use IPv4/v6.
//!
//! ### Node IDs
//!
//! Node IDs represent a single entity on a network segment.
//!
//! The Node ID of "0" is reserved, and is generally used when sending messages
//! within the local device.
//!
//! The Node ID of "255" is reserved.
//!
//! Network segments that require more than 254 nodes are not supported by
//! Ergot.
//!
//! Network IDs are intended to be discovered/negotiated at runtime, and are
//! not typically hardcoded. One exception to this is for known point-to-point
//! network segments that have a defined "controller" and "target" role, such
//! as USB (where the "host" is the "controller", and the "device" is the
//! "target"). In these cases, the "controller" typically hardcodes the Node ID
//! of "1", and the "target" hardcodes the Node ID of "2". This is done to
//! reduce complexity on these interface implementations.
//!
//! ### Socket IDs
//!
//! Socket IDs represent a single receiving socket within a [`NetStack`].
//!
//! The Socket ID of "0" is reserved, and is generally used as a "wildcard"
//! when sending messages to a device.
//!
//! The Socket ID of "255" is reserved.
//!
//! Systems that require more than 254 active sockets are not supported by
//! Ergot.
//!
//! Socket IDs are assigned dynamically by the [`NetStack`], and are never
//! intended to be hardcoded. Socket IDs may be recycled over time.
//!
//! If a device has multiple interfaces, and therefore has multiple (Network ID,
//! Node ID) tuples that refer to it, the same Socket ID is used on all
//! interfaces.
//!
//! ### Form on the wire
//!
//! When serialized into a packet, addresses are encoded with Network ID as the
//! most significant bytes, and the socket ID as the least significant bytes,
//! and then varint encoded. This means that in many cases, where "0" is used
//! for the Network or Node ID, or low numbers are used, Addresses can be
//! encoded in fewer than 4 bytes on the wire.
//!
//! For this reason, when negotiating any ID, lower numbers should be preferred
//! when possible. Addresses are only encoded as larger than 4 bytes when
//! addressing a network ID >= 4096.
//!
//! [`NetStack`]: crate::NetStack

use serde::{Deserialize, Serialize};

/// The Ergot Address type
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, PartialEq, Hash, Eq)]
pub struct Address {
    pub network_id: u16,
    pub node_id: u8,
    pub port_id: u8,
}

// ---- impl Address ----

impl Address {
    pub const fn unknown() -> Self {
        Self {
            network_id: 0,
            node_id: 0,
            port_id: 0,
        }
    }

    #[inline]
    pub fn net_node_any(&self) -> bool {
        self.network_id == 0 && self.node_id == 0
    }

    #[inline]
    pub fn as_u32(&self) -> u32 {
        ((self.network_id as u32) << 16) | ((self.node_id as u32) << 8) | (self.port_id as u32)
    }

    #[inline]
    pub fn from_word(word: u32) -> Self {
        Self {
            network_id: (word >> 16) as u16,
            node_id: (word >> 8) as u8,
            port_id: word as u8,
        }
    }
}

impl Serialize for Address {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let val = self.as_u32();
        val.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Address {
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let val = u32::deserialize(deserializer)?;
        Ok(Self::from_word(val))
    }
}

impl postcard_schema::Schema for Address {
    const SCHEMA: &'static postcard_schema::schema::NamedType =
        &postcard_schema::schema::NamedType {
            name: "Address",
            ty: u32::SCHEMA.ty,
        };
}
