#![doc = include_str!("../README.md")]
#![cfg_attr(not(any(test, feature = "std")), no_std)]

pub mod address;
pub mod interface_manager;
pub mod nash;
pub mod net_stack;
pub mod socket;
pub mod wire_frames;

pub use address::Address;
use interface_manager::InterfaceSendError;
use log::warn;
use nash::NameHash;
pub use net_stack::{NetStack, NetStackSendError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrameKind(pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Key(pub [u8; 8]);

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProtocolError(pub u16);

#[derive(Debug, Clone)]
pub struct AnyAllAppendix {
    pub key: Key,
    pub nash: Option<NameHash>,
}

#[derive(Debug, Clone)]
pub struct Header {
    pub src: Address,
    pub dst: Address,
    pub any_all: Option<AnyAllAppendix>,
    pub seq_no: Option<u16>,
    pub kind: FrameKind,
    pub ttl: u8,
}

#[derive(Debug, Clone)]
pub struct HeaderSeq {
    pub src: Address,
    pub dst: Address,
    pub any_all: Option<AnyAllAppendix>,
    pub seq_no: u16,
    pub kind: FrameKind,
    pub ttl: u8,
}

impl FrameKind {
    pub const RESERVED: Self = Self(0);
    pub const ENDPOINT_REQ: Self = Self(1);
    pub const ENDPOINT_RESP: Self = Self(2);
    pub const TOPIC_MSG: Self = Self(3);
    pub const PROTOCOL_ERROR: Self = Self(u8::MAX);
}

#[cfg(feature = "postcard-schema-v0_2")]
impl postcard_schema::Schema for FrameKind {
    const SCHEMA: &'static postcard_schema::schema::NamedType =
        &postcard_schema::schema::NamedType {
            name: "FrameKind",
            ty: u8::SCHEMA.ty,
        };
}

#[cfg(feature = "postcard-schema-v0_2")]
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
    pub const ISE_PLACEHOLDER_OH_NO: Self = Self(14);
    pub const ISE_ANY_PORT_MISSING_KEY: Self = Self(15);
    pub const ISE_TTL_EXPIRED: Self = Self(16);
    // 21..31: NetStackSendError
    pub const NSSE_NO_ROUTE: Self = Self(21);
    pub const NSSE_ANY_PORT_MISSING_KEY: Self = Self(22);
    pub const NSSE_WRONG_PORT_KIND: Self = Self(23);
    pub const NSSE_ANY_PORT_NOT_UNIQUE: Self = Self(24);
    pub const NSSE_ALL_PORT_MISSING_KEY: Self = Self(25);
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
            warn!("Header TTL expired: {self:?}");
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
