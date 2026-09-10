#![doc = include_str!("../../../README.md")]
#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![allow(clippy::uninlined_format_args)]

pub mod address;
pub mod book;
pub mod interface_manager;
pub mod logging;
pub mod nash;
pub mod net_stack;
pub mod prelude;
pub mod socket;
pub mod toolkits;
pub mod traits;
pub mod well_known;
pub mod wire_frames;

#[cfg(any(test, feature = "std"))]
pub mod conformance;

pub mod transport;

// Compat hack, remove on next breaking change
pub use logging::fmtlog;

use crate::logging::warn;
pub use address::Address;
use interface_manager::InterfaceSendError;
use nash::NameHash;
pub use net_stack::{NetStack, NetStackSendError};
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Eq)]
pub struct FrameKind(pub u8);

#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Key(pub [u8; 8]);

#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ProtocolError {
    // 0: Reserved
    Reserved,

    // 1..11: SocketSendError
    /// Socket has no space for the message
    SseNoSpace,
    /// Deserialization failed at the socket
    SseDeserFailed,
    /// Type mismatch at the socket
    SseTypeMismatch,
    /// Internal socket error
    SseWhatTheHell,

    // 11..21: InterfaceSendError
    /// Refusing to send local destination remotely
    IseDestinationLocal,
    /// Profile does not know how to route to requested destination
    IseNoRouteToDest,
    /// Outgoing interface is full
    IseInterfaceFull,
    /// Internal interface manager error
    IseInternalError,
    /// Destination was an "any" port but key was missing
    IseAnyPortMissingKey,
    /// TTL expired
    IseTtlExpired,
    /// Routing loop detected
    IseRoutingLoop,
    /// Packet exceeds outgoing interface MTU
    IsePacketTooBig {
        /// The MTU of the bottleneck interface
        mtu: u16,
    },

    // 21..31: NetStackSendError
    /// No route to destination
    NsseNoRoute,
    /// "Any" port missing key
    NsseAnyPortMissingKey,
    /// Wrong port kind
    NsseWrongPortKind,
    /// "Any" port not unique
    NsseAnyPortNotUnique,
    /// "All" port missing key
    NsseAllPortMissingKey,
    /// Would deadlock
    NsseWouldDeadlock,
}

#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, PartialEq)]
pub struct AnyAllAppendix {
    pub key: Key,
    pub nash: Option<NameHash>,
}

#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub src: Address,
    pub dst: Address,
    pub any_all: Option<AnyAllAppendix>,
    pub kind: FrameKind,
    /// Traffic class hint — see [`TrafficClass`]. Not a delivery guarantee.
    pub class: TrafficClass,
    /// Remaining hops; at most [`MAX_TTL`] (4 bits on the wire).
    pub ttl: u8,
}

impl core::fmt::Display for Header {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "({} -> {}; FK:{} TC:{} TTL:{})",
            self.src, self.dst, self.kind.0, self.class as u8, self.ttl
        )
    }
}

impl FrameKind {
    /// A protocol error report (see [`ProtocolError`]); the body is the error.
    pub const PROTOCOL_ERROR: Self = Self(0);
    pub const ENDPOINT_REQ: Self = Self(1);
    pub const ENDPOINT_RESP: Self = Self(2);
    pub const TOPIC_MSG: Self = Self(3);

    /// The four kinds exactly fill the 2-bit wire field.
    pub const MAX_BITS: u8 = 0b11;

    /// Whether this is one of the four wire-representable kinds.
    #[inline]
    pub const fn is_valid(self) -> bool {
        self.0 <= Self::MAX_BITS
    }
}

/// Traffic class of a frame: a *hint* to interfaces about what to shed or
/// deprioritize under contention. It never changes delivery semantics —
/// every send is still at-most-once and a `Control` frame can still be
/// dropped — and interfaces are free to ignore it (the stream sinks do).
/// A CAN interface maps it onto arbitration priority; a bounded sink may
/// refuse `Bulk`/`Background` frames early to keep headroom for `Control`.
///
/// The class is a property of the endpoint/topic *type* (`Endpoint::CLASS`,
/// `Topic::CLASS`, set via the `endpoint!`/`topic!` macros) and is inherited
/// by responses and protocol-error replies.
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum TrafficClass {
    /// Small, latency-sensitive frames: setpoints, acks, bootstrap.
    Control = 0,
    /// Everything without a stated preference.
    #[default]
    Normal = 1,
    /// High-volume, low-value streams that should be shed first.
    Bulk = 2,
    /// Diagnostics that must never compete with anything else (logs).
    Background = 3,
}

impl TrafficClass {
    #[inline]
    pub const fn to_bits(self) -> u8 {
        self as u8
    }

    /// Decode from the 2-bit wire field (extra bits are ignored).
    #[inline]
    pub const fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0 => Self::Control,
            1 => Self::Normal,
            2 => Self::Bulk,
            _ => Self::Background,
        }
    }
}

impl postcard_schema::Schema for FrameKind {
    const SCHEMA: &'static postcard_schema::schema::NamedType =
        &postcard_schema::schema::NamedType {
            name: "FrameKind",
            ty: u8::SCHEMA.ty,
        };
}

impl postcard_schema::Schema for Key {
    const SCHEMA: &'static postcard_schema::schema::NamedType =
        &postcard_schema::schema::NamedType {
            name: "Key",
            ty: <[u8; 8]>::SCHEMA.ty,
        };
}

impl Header {
    #[inline]
    pub fn decrement_ttl(&mut self) -> Result<(), InterfaceSendError> {
        self.ttl = self.ttl.checked_sub(1).ok_or_else(|| {
            warn!("Header TTL expired: {:?}", self);
            InterfaceSendError::TtlExpired
        })?;
        Ok(())
    }
}

/// Version of the frame wire format (header layout, appendix, framing
/// contract). Bumped on every incompatible change; peers on different wire
/// versions cannot parse each other's frames, so this is reported in the
/// well-known [`DeviceInfo`](well_known::DeviceInfo) for diagnostics, not
/// negotiated per frame.
///
/// - 0: the original layout (u16 `seq_no`, separate `kind`/`ttl` bytes,
///   `PROTOCOL_ERROR = 255`, TTL up to 255).
/// - 1: `seq_no` removed; `kind`/`class`/`ttl` packed into one byte;
///   `PROTOCOL_ERROR = 0`; TTL up to 15; traffic class added.
pub const WIRE_VERSION: u8 = 1;

/// Largest hop count the 4-bit wire field can carry; headers with a larger
/// `ttl` are clamped to this on encode.
pub const MAX_TTL: u8 = 15;

pub const DEFAULT_TTL: u8 = MAX_TTL;

/// Exports of used crate versions
pub mod exports {
    pub use bbqueue;
    pub use maitake_sync;
    pub use mutex;
}

// Internal re-export of embedded-io-async (supports both v0.6 and v0.7 - API is identical)
#[cfg(feature = "embedded-io-async-v0_6")]
pub(crate) use embedded_io_async_0_6 as eio;
#[cfg(all(
    feature = "embedded-io-async-v0_7",
    not(feature = "embedded-io-async-v0_6")
))]
pub(crate) use embedded_io_async_0_7 as eio;
