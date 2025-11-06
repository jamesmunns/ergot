#![doc = include_str!("../README.md")]
#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![allow(clippy::uninlined_format_args)]

pub mod address;
pub mod book;
pub mod interface_manager;
pub mod logging;
pub mod nash;
pub mod net_stack;
pub mod socket;
pub mod toolkits;
pub mod traits;
pub mod well_known;
pub mod wire_frames;

#[cfg(any(test, feature = "std"))]
pub mod conformance;

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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProtocolError(pub u16);

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
    pub seq_no: Option<u16>,
    pub kind: FrameKind,
    pub ttl: u8,
}

#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone)]
pub struct HeaderSeq {
    pub src: Address,
    pub dst: Address,
    pub any_all: Option<AnyAllAppendix>,
    pub seq_no: u16,
    pub kind: FrameKind,
    pub ttl: u8,
}

impl core::fmt::Display for Header {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "({} -> {}; FK:{:03}, SQ:",
            self.src, self.dst, self.kind.0,
        )?;
        if let Some(seq) = self.seq_no {
            write!(f, "{:04X}", seq)?;
        } else {
            f.write_str("----")?;
        }
        f.write_str(")")?;
        Ok(())
    }
}

impl core::fmt::Display for HeaderSeq {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "({} -> {}; FK:{:03}, SQ:{:04X})",
            self.src, self.dst, self.kind.0, self.seq_no,
        )?;
        Ok(())
    }
}

impl FrameKind {
    pub const RESERVED: Self = Self(0);
    pub const ENDPOINT_REQ: Self = Self(1);
    pub const ENDPOINT_RESP: Self = Self(2);
    pub const TOPIC_MSG: Self = Self(3);
    pub const PROTOCOL_ERROR: Self = Self(u8::MAX);
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

impl ProtocolError {
    pub const RESERVED: Self = Self(0);
    // 1..11: SocketSendError
    pub const SSE_NO_SPACE: Self = Self(1);
    pub const SSE_DESER_FAILED: Self = Self(2);
    pub const SSE_TYPE_MISMATCH: Self = Self(3);
    pub const SSE_WHAT_THE_HELL: Self = Self(4);
    // 11..21: InterfaceSendError
    pub const ISE_DESTINATION_LOCAL: Self = Self(11);
    pub const ISE_NO_ROUTE_TO_DEST: Self = Self(12);
    pub const ISE_INTERFACE_FULL: Self = Self(13);
    pub const ISE_INTERNAL_ERROR: Self = Self(14);
    pub const ISE_ANY_PORT_MISSING_KEY: Self = Self(15);
    pub const ISE_TTL_EXPIRED: Self = Self(16);
    pub const ISE_ROUTING_LOOP: Self = Self(17);
    // 21..31: NetStackSendError
    pub const NSSE_NO_ROUTE: Self = Self(21);
    pub const NSSE_ANY_PORT_MISSING_KEY: Self = Self(22);
    pub const NSSE_WRONG_PORT_KIND: Self = Self(23);
    pub const NSSE_ANY_PORT_NOT_UNIQUE: Self = Self(24);
    pub const NSSE_ALL_PORT_MISSING_KEY: Self = Self(25);
    pub const NSSE_WOULD_DEADLOCK: Self = Self(26);
}

impl Header {
    #[inline]
    pub fn with_seq(self, seq_no: u16) -> HeaderSeq {
        let Self {
            src,
            dst,
            any_all,
            seq_no: _,
            kind,
            ttl,
        } = self;
        HeaderSeq {
            src,
            dst,
            any_all,
            seq_no,
            kind,
            ttl,
        }
    }

    #[inline]
    pub fn to_headerseq_or_with_seq<F: FnOnce() -> u16>(&self, f: F) -> HeaderSeq {
        HeaderSeq {
            src: self.src,
            dst: self.dst,
            any_all: self.any_all.clone(),
            seq_no: self.seq_no.unwrap_or_else(f),
            kind: self.kind,
            ttl: self.ttl,
        }
    }

    #[inline]
    pub fn decrement_ttl(&mut self) -> Result<(), InterfaceSendError> {
        self.ttl = self.ttl.checked_sub(1).ok_or_else(|| {
            warn!("Header TTL expired: {:?}", self);
            InterfaceSendError::TtlExpired
        })?;
        Ok(())
    }
}

impl HeaderSeq {
    #[inline]
    pub fn decrement_ttl(&mut self) -> Result<(), InterfaceSendError> {
        self.ttl = self.ttl.checked_sub(1).ok_or_else(|| {
            warn!("Header TTL expired: {:?}", self);
            InterfaceSendError::TtlExpired
        })?;
        Ok(())
    }
}

impl From<HeaderSeq> for Header {
    fn from(val: HeaderSeq) -> Self {
        Self {
            src: val.src,
            dst: val.dst,
            any_all: val.any_all.clone(),
            seq_no: Some(val.seq_no),
            kind: val.kind,
            ttl: val.ttl,
        }
    }
}

pub const DEFAULT_TTL: u8 = 16;

/// Exports of used crate versions
pub mod exports {
    pub use bbq2;
    pub use mutex;
}
