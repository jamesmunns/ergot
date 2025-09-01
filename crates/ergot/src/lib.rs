#![doc = include_str!("../README.md")]
#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![allow(clippy::uninlined_format_args)]

pub mod address;
pub mod book;
pub mod fmtlog;
pub mod interface_manager;
pub mod nash;
pub mod net_stack;
pub mod socket;
pub mod toolkits;
pub mod traits;
pub mod well_known;
pub mod wire_frames;

#[cfg(any(test, feature = "std"))]
pub mod conformance;

pub use address::Address;
use interface_manager::InterfaceSendError;
use log::warn;
use nash::NameHash;
pub use net_stack::{NetStack, NetStackSendError};
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
    pub const ISE_PLACEHOLDER_OH_NO: Self = Self(14);
    pub const ISE_ANY_PORT_MISSING_KEY: Self = Self(15);
    pub const ISE_TTL_EXPIRED: Self = Self(16);
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

use logger::logger;
pub use logger::set_logger_racy;

mod logger {
    use core::sync::atomic::Ordering;

    use log::Log;
    use portable_atomic::AtomicUsize;

    static NOP_LOGGER: NopLogger = NopLogger;
    static mut LOGGER: StaticLogger = StaticLogger::new();

    #[allow(static_mut_refs)]
    pub unsafe fn set_logger_racy(logger: &'static dyn Log) -> Result<(), ()> {
        unsafe { LOGGER.set_logger_racy(logger) }
    }

    #[allow(static_mut_refs)]
    pub fn logger() -> &'static dyn Log {
        unsafe { &mut LOGGER }.logger()
    }

    struct StaticLogger {
        logger: &'static dyn Log,
        state: AtomicUsize,
    }

    impl StaticLogger {
        pub const fn new() -> Self {
            Self {
                logger: &NOP_LOGGER,
                state: AtomicUsize::new(State::Uninitialized as usize),
            }
        }

        pub unsafe fn set_logger_racy(&mut self, logger: &'static dyn Log) -> Result<(), ()> {
            match unsafe {
                core::mem::transmute::<usize, State>(self.state.load(Ordering::Acquire))
            } {
                State::Initialized => Err(()),
                State::Uninitialized => {
                    self.logger = logger;
                    self.state
                        .store(State::Initialized as usize, Ordering::Release);
                    Ok(())
                }
                State::Initializing => {
                    unreachable!("set_logger_racy should not be used with other logging functions")
                }
            }
        }

        pub fn logger(&self) -> &'static dyn Log {
            if self.state.load(Ordering::Acquire) != State::Initialized as usize {
                &NOP_LOGGER
            } else {
                self.logger
            }
        }
    }

    #[repr(usize)]
    enum State {
        Uninitialized,
        Initializing,
        Initialized,
    }

    pub struct NopLogger;

    impl Log for NopLogger {
        fn enabled(&self, _: &log::Metadata) -> bool {
            false
        }

        fn log(&self, _: &log::Record) {}
        fn flush(&self) {}
    }
}

/// Exports of used crate versions
pub mod exports {
    pub use bbq2;
    pub use mutex;
}
