#![doc = include_str!("../README.md")]

pub mod address;
pub mod interface_manager;
pub mod net_stack;
pub mod socket;

pub use address::Address;
use interface_manager::InterfaceSendError;
use log::warn;
pub use net_stack::{NetStack, NetStackSendError};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameKind(pub u8);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Key(pub [u8; 8]);

#[derive(Debug, Clone)]
pub struct Header {
    pub src: Address,
    pub dst: Address,
    pub key: Option<Key>,
    pub seq_no: Option<u16>,
    pub kind: FrameKind,
    pub ttl: u8,
}

#[derive(Debug, Clone)]
pub struct HeaderSeq {
    pub src: Address,
    pub dst: Address,
    pub key: Option<Key>,
    pub seq_no: u16,
    pub kind: FrameKind,
    pub ttl: u8,
}

impl FrameKind {
    pub const RESERVED: Self = Self(0);
    pub const ENDPOINT_REQ: Self = Self(1);
    pub const ENDPOINT_RESP: Self = Self(2);
    pub const TOPIC_MSG: Self = Self(3);
}

impl Header {
    #[inline]
    pub fn with_seq(self, seq_no: u16) -> HeaderSeq {
        let Self {
            src,
            dst,
            key,
            seq_no: _,
            kind,
            ttl,
        } = self;
        HeaderSeq {
            src,
            dst,
            key,
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
            key: self.key,
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
            key: val.key,
            seq_no: Some(val.seq_no),
            kind: val.kind,
            ttl: val.ttl,
        }
    }
}

pub const DEFAULT_TTL: u8 = 16;
