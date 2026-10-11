//! CAN FD Interface Implementation
//!
//! This implementation uses CAN FD frames with an optimized header layout that puts
//! hardware-filterable fields in the CAN extended ID (29-bit), reducing payload overhead.
//!
//! Since CAN-FD messages do not have a lot of bytes available, there are multiple header variants to choose from:
//! - FULL:
//!   This header variant is the default, it transports the same information the normal ergot [`HeaderSeq`] does.
//!   (Currently the max amount of destination network bits that get transported are 10, making the highest network ID possible 1023).
//!   Because it transports everything it takes up some space. It allows CAN message filtering on the priority,
//!   and the destination network ID (or at least ten bits of it), node ID and port ID
//! - END:
//!   This variant uses the way ergot's networking works to it's advantage to make the header a few bytes smaller,
//!   but it only works for networks where the CAN-FD transport is the last hop between a router and one or many direct edge nodes.
//!   In those cases both one network ID and the TTL can be ignored.
//!   Since it is the last hop the TTL doesn't matter anymore. Either the packet gets dropped before being sent via CAN-FD or it has arrived at it's target.
//!   For messages that go from router to a direct edge node the destination network ID can be ignored,
//!   since the direct edge node knows the network ID has to be the ID of the network it is connected to.
//!   And for messages that go in the other direction (direct edge to router) the router knows that the
//!   source network ID has to be the ID of the CAN-FD network the message was just received through.
//!
//!   By using this information we can make the header a few bytes smaller without loosing any kind of data.
//!
//! If both of these header variants don't work for you, you have an idea on how to make the header even smaller
//! or you need the CAN extended ID to be a specific value that has nothing to do with ergot while still transmitting ergot packages
//! you can build a new header variant yourself by implementing the [`CANHeader`] trait.
//!
//!
//! ## CAN Extended ID Layout (29 bits) in FULL mode
//!
//! ```text
//! ┌──────────┬───────────┬──────────┬──────────┐
//! │ Priority │  dst_net  │ dst_node │ dst_port │
//! │ (3 bits) │ (10 bits) │ (8 bits) │ (8 bits) │
//! └──────────┴───────────┴──────────┴──────────┘
//!  Bits 28-26    25-16       15-8       7-0
//!
//! Note that since the Destination network id stored in the CAN Extended ID Layout has only ten bits, network ID's larger than 1023 will not work
//!
//! This layout enables CAN hardware filtering on:
//! - Priority (CAN arbitration - lower ID = higher priority)
//! - Destination network ID (filter messages for the network behind this router)
//! - Destination node ID (filter messages for this device)
//! - Destination port ID (filter messages for specific services)
//!
//! ## Payload Layout
//!
//! The remaining header fields are encoded in the CAN FD payload using postcard varint encoding:
//!
//! ```text
//! ┌──────────────┬────────────┬──────────┬────────────┬────────────┬──────────┐
//! │   src_net    │  src_node  │ src_port │ ttl        │ frame_kind │ body     │
//! │ (1-3 bytes)  │  (1 byte   │ (1 byte) │ (1 byte)   │ (1 byte)   │ (N bytes)│
//! └──────────────┴────────────┴──────────┴────────────┴────────────┴──────────┘
//!
//!
//! For broadcast/any-port messages (port 0 or 255), the AnyAllAppendix is also included.
//!
//! ## Overhead Comparison
//!
//! For a typical message with low network IDs:
//! - Standard ergot header: ~12-14 bytes
//! - CAN FD optimized: ~5-7 bytes in payload (rest in CAN ID)
//!
//!
//! ## CAN Extended ID Layout (29 bits) in END mode
//!
//! ```text
//! ┌──────────┬──────────┬──────────┬───────────┬──────────┐
//! │ Priority │ dst_node │ dst_port │ frame_kind│ free_data│
//! │ (3 bits) │ (8 bits) │ (8 bits) │  (3 bits) │ (7 bits) │
//! └──────────┴──────────┴──────────┴───────────┴──────────┘
//!  Bits 28-26   25-18      17-10       9-7         6-0
//!
//! This layout enables CAN hardware filtering on:
//! - Destination node ID (filter messages for this device)
//! - Destination port ID (filter messages for specific services)
//! - Frame kind (filter requests, responses, or topic messages)
//! - Priority (CAN arbitration - lower ID = higher priority)
//! - Free Data bits (Usable by the user to add custom filtering options, set to zero by default)
//!
//! ## Payload Layout
//!
//! Since the assumption in END mode is that the transport via CAN is the last hop, no Destination network ID or TTL information are included
//! The remaining header fields are encoded in the CAN FD payload using postcard varint encoding:
//!
//! ```text
//! ┌─────────────────┬──────────┬──────────┬──────────┐
//! │ (src/dst).net_id│ src_node │ src_port │   body   │
//! │ (1-3 bytes)     │ (1 byte) │ (1 byte) │ (N bytes)│
//! └─────────────────┴──────────┴──────────┴──────────┘
//!
//!
//! For broadcast/any-port messages (port 0 or 255), the AnyAllAppendix is also included.
//!
//! ## Overhead Comparison
//!
//! For a typical message with low network IDs:
//! - Standard ergot header: ~12-14 bytes
//! - CAN FD optimized: ~3-5 bytes in payload (rest in CAN ID)

// ============================================================================
// Constants
// ============================================================================

use core::{fmt, marker::PhantomData};

use postcard::{
    Serializer,
    ser_flavors::{self, Flavor},
};
use serde::{Deserialize, Serialize};

use crate::{
    AnyAllAppendix, FrameKind, HeaderSeq, Key, ProtocolError, interface_manager::InterfaceSink,
    nash::NameHash,
};

/// Maximum CAN FD payload size
pub const CAN_FD_MAX_PAYLOAD: usize = 64;

// ============================================================================
// CAN ID Encoding
// ============================================================================

/// Priority level for CAN arbitration (lower = higher priority)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum CanPriority {
    /// Highest priority (0) - for critical/real-time messages
    Critical = 0,
    /// High priority (1)
    High = 1,
    /// Normal priority (2) - default
    #[default]
    Normal = 2,
    /// Low priority (3)
    Low = 3,
    /// Bulk priority (4) - for large transfers
    Bulk = 4,
    /// Background priority (5)
    Background = 5,
    /// Lowest priority (6)
    Lowest = 6,
    /// Reserved (7)
    Reserved = 7,
}

impl CanPriority {
    /// Convert from raw 3-bit value
    pub const fn from_bits(bits: u8) -> Self {
        match bits & 0x7 {
            0 => Self::Critical,
            1 => Self::High,
            2 => Self::Normal,
            3 => Self::Low,
            4 => Self::Bulk,
            5 => Self::Background,
            6 => Self::Lowest,
            _ => Self::Reserved,
        }
    }

    /// Convert to raw 3-bit value
    pub const fn to_bits(self) -> u8 {
        self as u8
    }
}

/// Convert FrameKind to 3-bit representation
const fn frame_kind_to_bits(kind: FrameKind) -> u8 {
    match kind.0 {
        0 => 0,   // RESERVED
        1 => 1,   // ENDPOINT_REQ
        2 => 2,   // ENDPOINT_RESP
        3 => 3,   // TOPIC_MSG
        255 => 7, // PROTOCOL_ERROR
        _ => 0,   // Unknown -> RESERVED
    }
}

/// Convert 3-bit representation back to FrameKind
const fn bits_to_frame_kind(bits: u8) -> FrameKind {
    match bits {
        0 => FrameKind::RESERVED,
        1 => FrameKind::ENDPOINT_REQ,
        2 => FrameKind::ENDPOINT_RESP,
        3 => FrameKind::TOPIC_MSG,
        7 => FrameKind::PROTOCOL_ERROR,
        _ => FrameKind::RESERVED,
    }
}

pub trait CanFrameId
where
    Self: Sized,
{
    /// Maximum valid extended CAN ID (29 bits)
    const MAX_EXTENDED_ID: u32 = 0x1FFF_FFFF;

    /// Parse from raw 29-bit CAN extended ID
    fn from_raw(id: u32) -> Self {
        Self::from_raw_unchecked(id & Self::MAX_EXTENDED_ID)
    }

    fn from_raw_unchecked(id: u32) -> Self;

    fn to_raw_unchecked(&self) -> u32;

    fn to_raw(&self) -> u32 {
        Self::to_raw_unchecked(self) & Self::MAX_EXTENDED_ID
    }
}

pub trait CanPayloadHeader {
    fn from_header(hdr: &HeaderSeq) -> Self;
}

pub trait CANHeader<'a> {
    type PayloadHeader: Serialize + Deserialize<'a> + CanPayloadHeader;
    type CanFrameId: CanFrameId + fmt::Display;

    fn convert_from_ergot_header(hdr: &HeaderSeq) -> (Self::CanFrameId, Self::PayloadHeader) {
        Self::convert_from_ergot_header_with_priority(hdr, CanPriority::default())
    }

    fn convert_from_ergot_header_with_priority(
        hdr: &HeaderSeq,
        priority: CanPriority,
    ) -> (Self::CanFrameId, Self::PayloadHeader);

    fn convert_to_ergot_header(
        id: &Self::CanFrameId,
        payload_header: &Self::PayloadHeader,
    ) -> HeaderSeq;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanPayloadHeaderEND {
    net_id: u16,
    src_node: u8,
    src_port: u8,
}

impl CanPayloadHeader for CanPayloadHeaderEND {
    fn from_header(hdr: &HeaderSeq) -> Self {
        Self {
            net_id: hdr.dst.network_id,
            src_node: hdr.src.node_id,
            src_port: hdr.src.port_id,
        }
    }
}

/// Layout (29 bits) in END mode:
/// - Bits 28-26: Priority (3 bits)
/// - Bits 25-18: Destination node ID (8 bits)
/// - Bits 17-10: Destination port ID (8 bits)
/// - Bits 9-7: Frame kind (3 bits, maps ENDPOINT_REQ=1, ENDPOINT_RESP=2, TOPIC_MSG=3, ERROR=7)
/// - Bits 6-0: Free data (7 bits)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanFrameIdEND(u32);

impl CanFrameId for CanFrameIdEND {
    fn from_raw_unchecked(id: u32) -> Self {
        Self(id)
    }

    fn to_raw_unchecked(&self) -> u32 {
        self.0
    }
}

impl CanFrameIdEND {
    // Bit positions
    const PRIORITY_SHIFT: u32 = 26;
    const DST_NODE_SHIFT: u32 = 18;
    const DST_PORT_SHIFT: u32 = 10;
    const KIND_SHIFT: u32 = 7;
    const FREE_DATA_SHIFT: u32 = 0;

    // Masks
    const PRIORITY_MASK: u32 = 0x7 << Self::PRIORITY_SHIFT;
    const DST_NODE_MASK: u32 = 0xFF << Self::DST_NODE_SHIFT;
    const DST_PORT_MASK: u32 = 0xFF << Self::DST_PORT_SHIFT;
    const KIND_MASK: u32 = 0x7 << Self::KIND_SHIFT;
    const FREE_DATA_MASK: u32 = 0x7F << Self::FREE_DATA_SHIFT;

    /// Create a new CAN frame ID from header fields
    pub const fn new(
        priority: CanPriority,
        dst_node_id: u8,
        dst_port_id: u8,
        kind: FrameKind,
        free_data: u8,
    ) -> Self {
        let kind_bits = frame_kind_to_bits(kind);
        let id = ((priority.to_bits() as u32) << Self::PRIORITY_SHIFT)
            | ((dst_node_id as u32) << Self::DST_NODE_SHIFT)
            | ((dst_port_id as u32) << Self::DST_PORT_SHIFT)
            | ((kind_bits as u32) << Self::KIND_SHIFT)
            | ((free_data as u32) << Self::FREE_DATA_SHIFT);
        Self(id)
    }

    /// Create from an ergot HeaderSeq with default priority
    pub const fn from_header(hdr: &HeaderSeq) -> Self {
        Self::from_header_with_priority(hdr, CanPriority::Normal)
    }

    /// Create from an ergot HeaderSeq with specified priority
    pub const fn from_header_with_priority(hdr: &HeaderSeq, priority: CanPriority) -> Self {
        Self::new(priority, hdr.dst.node_id, hdr.dst.port_id, hdr.kind, 0)
    }

    /// Create from an ergot HeaderSeq with specified priority
    pub const fn from_header_with_priority_and_free_data(
        hdr: &HeaderSeq,
        priority: CanPriority,
        free_data: u8,
    ) -> Self {
        Self::new(
            priority,
            hdr.dst.node_id,
            hdr.dst.port_id,
            hdr.kind,
            free_data,
        )
    }

    /// Extract priority
    pub const fn priority(self) -> CanPriority {
        CanPriority::from_bits(((self.0 & Self::PRIORITY_MASK) >> Self::PRIORITY_SHIFT) as u8)
    }

    /// Extract destination node ID
    pub const fn dst_node_id(self) -> u8 {
        ((self.0 & Self::DST_NODE_MASK) >> Self::DST_NODE_SHIFT) as u8
    }

    /// Extract destination port ID
    pub const fn dst_port_id(self) -> u8 {
        ((self.0 & Self::DST_PORT_MASK) >> Self::DST_PORT_SHIFT) as u8
    }

    /// Extract frame kind
    pub const fn frame_kind(self) -> FrameKind {
        let bits = ((self.0 & Self::KIND_MASK) >> Self::KIND_SHIFT) as u8;
        bits_to_frame_kind(bits)
    }

    /// Extract free data
    pub const fn free_data(self) -> u8 {
        ((self.0 & Self::FREE_DATA_MASK) >> Self::FREE_DATA_SHIFT) as u8
    }

    /// Create a filter mask for matching destination node ID only
    pub const fn filter_mask_node_only() -> u32 {
        Self::DST_NODE_MASK
    }

    /// Create a filter mask for matching destination node and port
    pub const fn filter_mask_node_port() -> u32 {
        Self::DST_NODE_MASK | Self::DST_PORT_MASK
    }

    /// Create a filter mask for matching destination node, port, and kind
    pub const fn filter_mask_full() -> u32 {
        Self::DST_NODE_MASK | Self::DST_PORT_MASK | Self::KIND_MASK
    }

    /// Create a filter mask for matching the free data
    pub const fn free_data_mask() -> u32 {
        Self::FREE_DATA_MASK
    }
}

impl fmt::Display for CanFrameIdEND {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CanId(pri={:?}, dst={}:{}, kind={:?}, free={:?})",
            self.priority(),
            self.dst_node_id(),
            self.dst_port_id(),
            self.frame_kind().0,
            self.free_data(),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct END;
impl<'a> CANHeader<'a> for END {
    type PayloadHeader = CanPayloadHeaderEND;
    type CanFrameId = CanFrameIdEND;

    fn convert_from_ergot_header_with_priority(
        hdr: &HeaderSeq,
        priority: CanPriority,
    ) -> (Self::CanFrameId, Self::PayloadHeader) {
        (
            CanFrameIdEND::from_header_with_priority(hdr, priority),
            CanPayloadHeaderEND::from_header(hdr),
        )
    }

    fn convert_to_ergot_header(
        id: &Self::CanFrameId,
        payload_header: &Self::PayloadHeader,
    ) -> HeaderSeq {
        HeaderSeq {
            seq_no: 0,
            ttl: 16,
            any_all: None,
            kind: id.frame_kind(),
            dst: crate::Address {
                network_id: 0, // In END mode we assume we're always the last hop, so we know that the network will always be correct
                node_id: id.dst_node_id(),
                port_id: id.dst_port_id(),
            },
            src: crate::Address {
                network_id: payload_header.net_id,
                node_id: payload_header.src_node,
                port_id: payload_header.src_port,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FULL;

/// Layout (29 bits) in FULL mode:
/// - Bits 28-26: Priority (3 bits)
/// - Bits 25-16: Destination network ID (10 bits)
/// - Bits 15-8: Destination node ID (8 bits)
/// - Bits  7-0: Destination port ID (8 bits)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanFrameIdFULL(u32);

impl CanFrameId for CanFrameIdFULL {
    fn from_raw_unchecked(id: u32) -> Self {
        Self(id)
    }

    fn to_raw_unchecked(&self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanPayloadHeaderFULL {
    src_net: u16,
    src_node: u8,
    src_port: u8,
    ttl: u8,
    kind: u8,
}

impl CanPayloadHeader for CanPayloadHeaderFULL {
    fn from_header(hdr: &HeaderSeq) -> Self {
        Self {
            src_net: hdr.src.network_id,
            src_node: hdr.src.node_id,
            src_port: hdr.src.port_id,
            ttl: hdr.ttl,
            kind: frame_kind_to_bits(hdr.kind),
        }
    }
}

impl<'a> CANHeader<'a> for FULL {
    type PayloadHeader = CanPayloadHeaderFULL;
    type CanFrameId = CanFrameIdFULL;

    fn convert_from_ergot_header_with_priority(
        hdr: &HeaderSeq,
        priority: CanPriority,
    ) -> (Self::CanFrameId, Self::PayloadHeader) {
        (
            CanFrameIdFULL::from_header_with_priority(hdr, priority),
            CanPayloadHeaderFULL::from_header(hdr),
        )
    }

    fn convert_to_ergot_header(
        id: &Self::CanFrameId,
        payload_header: &Self::PayloadHeader,
    ) -> HeaderSeq {
        HeaderSeq {
            seq_no: 0,
            ttl: payload_header.ttl,
            any_all: None,
            kind: bits_to_frame_kind(payload_header.kind),
            dst: crate::Address {
                network_id: id.dst_net_id(),
                node_id: id.dst_node_id(),
                port_id: id.dst_port_id(),
            },
            src: crate::Address {
                network_id: payload_header.src_net,
                node_id: payload_header.src_node,
                port_id: payload_header.src_port,
            },
        }
    }
}

impl CanFrameIdFULL {
    // Bit positions
    const PRIORITY_SHIFT: u32 = 26;
    const DST_NET_SHIFT: u32 = 16;
    const DST_NODE_SHIFT: u32 = 8;
    const DST_PORT_SHIFT: u32 = 0;

    // Masks
    const PRIORITY_MASK: u32 = 0x7 << Self::PRIORITY_SHIFT;
    const DST_NET_MASK: u32 = 0x3FF << Self::DST_NET_SHIFT;
    const DST_NODE_MASK: u32 = 0xFF << Self::DST_NODE_SHIFT;
    const DST_PORT_MASK: u32 = 0xFF << Self::DST_PORT_SHIFT;

    /// Create a new CAN frame ID from header fields
    pub const fn new(
        priority: CanPriority,
        dst_net_id: u16,
        dst_node_id: u8,
        dst_port_id: u8,
    ) -> Self {
        let id = ((priority.to_bits() as u32) << Self::PRIORITY_SHIFT)
            | ((dst_net_id as u32) << Self::DST_NET_SHIFT)
            | ((dst_node_id as u32) << Self::DST_NODE_SHIFT)
            | ((dst_port_id as u32) << Self::DST_PORT_SHIFT);
        Self(id)
    }

    /// Create from an ergot HeaderSeq with default priority
    pub const fn from_header(hdr: &HeaderSeq) -> Self {
        Self::from_header_with_priority(hdr, CanPriority::Normal)
    }

    /// Create from an ergot HeaderSeq with specified priority
    pub const fn from_header_with_priority(hdr: &HeaderSeq, priority: CanPriority) -> Self {
        Self::new(
            priority,
            hdr.dst.network_id,
            hdr.dst.node_id,
            hdr.dst.port_id,
        )
    }

    /// Create from an ergot HeaderSeq with specified priority
    pub const fn from_header_with_priority_and_free_data(
        hdr: &HeaderSeq,
        priority: CanPriority,
    ) -> Self {
        Self::new(
            priority,
            hdr.dst.network_id,
            hdr.dst.node_id,
            hdr.dst.port_id,
        )
    }

    /// Extract priority
    pub const fn priority(self) -> CanPriority {
        CanPriority::from_bits(((self.0 & Self::PRIORITY_MASK) >> Self::PRIORITY_SHIFT) as u8)
    }

    /// Extract destination network ID
    pub const fn dst_net_id(self) -> u16 {
        ((self.0 & Self::DST_NET_MASK) >> Self::DST_NET_SHIFT) as u16
    }

    /// Extract destination node ID
    pub const fn dst_node_id(self) -> u8 {
        ((self.0 & Self::DST_NODE_MASK) >> Self::DST_NODE_SHIFT) as u8
    }

    /// Extract destination port ID
    pub const fn dst_port_id(self) -> u8 {
        ((self.0 & Self::DST_PORT_MASK) >> Self::DST_PORT_SHIFT) as u8
    }

    /// Create a filter mask for matching destination node ID only
    pub const fn filter_mask_node_only() -> u32 {
        Self::DST_NODE_MASK
    }

    /// Create a filter mask for matching destination node and port
    pub const fn filter_mask_node_port() -> u32 {
        Self::DST_NODE_MASK | Self::DST_PORT_MASK
    }

    /// Create a filter mask for matching destination network, node and port
    pub const fn filter_mask_full() -> u32 {
        Self::DST_NET_MASK | Self::DST_NODE_MASK | Self::DST_PORT_MASK
    }
}

impl fmt::Display for CanFrameIdFULL {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CanId(pri={:?}, dst={}:{}:{})",
            self.priority(),
            self.dst_net_id(),
            self.dst_node_id(),
            self.dst_port_id(),
        )
    }
}

// ============================================================================
// Frame Encoding/Decoding
// ============================================================================

/// Error when encoding a CAN frame
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanEncodeError {
    /// Message too large for CAN FD payload
    PayloadTooLarge,
    /// Serialization failed
    SerializationError,
}

impl CanEncodeError {
    /// Map a postcard error to CanEncodeError
    ///
    /// Buffer-full errors are mapped to PayloadTooLarge for clearer diagnostics.
    fn from_postcard(err: postcard::Error) -> Self {
        match err {
            postcard::Error::SerializeBufferFull => Self::PayloadTooLarge,
            _ => Self::SerializationError,
        }
    }
}

/// Error when decoding a CAN frame
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanDecodeError {
    /// Payload too short
    PayloadTooShort,
    /// Deserialization failed
    DeserializationError,
    /// Invalid frame kind in CAN ID
    InvalidFrameKind,
}

/// A decoded CAN FD frame
#[derive(Debug)]
pub struct CanFrame<'a> {
    /// Reconstructed header
    pub header: HeaderSeq,
    /// Message body (or error)
    pub body: Result<&'a [u8], ProtocolError>,
}

/// Encode an ergot message into CAN ID + payload
///
/// Returns (CAN extended ID, payload slice length used)
pub fn encode_frame<'a, T: Serialize, Header: CANHeader<'a>>(
    hdr: &HeaderSeq,
    body: &T,
    priority: CanPriority,
    buf: &mut [u8],
) -> Result<(u32, usize), CanEncodeError> {
    // Don't do an early bounds check here - the actual header size varies based on
    // address values (varint encoding) and presence of AnyAll appendix. Let
    // serialization fail if there's not enough space, then check final size against
    // CAN_FD_MAX_PAYLOAD.

    let (can_id, payload_hdr) = Header::convert_from_ergot_header_with_priority(hdr, priority);

    let ser = ser_flavors::Slice::new(buf);
    let mut serializer = Serializer { output: ser };

    // Serialize payload header
    payload_hdr
        .serialize(&mut serializer)
        .map_err(CanEncodeError::from_postcard)?;

    // Serialize any/all appendix if present
    if let Some(app) = hdr.any_all.as_ref() {
        serializer
            .output
            .try_extend(&app.key.0)
            .map_err(CanEncodeError::from_postcard)?;
        let nash_val: u32 = app.nash.as_ref().map(NameHash::to_u32).unwrap_or(0);
        nash_val
            .serialize(&mut serializer)
            .map_err(CanEncodeError::from_postcard)?;
    }

    // Serialize body
    body.serialize(&mut serializer)
        .map_err(CanEncodeError::from_postcard)?;

    let used = serializer
        .output
        .finalize()
        .map_err(CanEncodeError::from_postcard)?;

    if used.len() > CAN_FD_MAX_PAYLOAD {
        return Err(CanEncodeError::PayloadTooLarge);
    }

    Ok((can_id.to_raw(), used.len()))
}

/// Encode a raw (pre-serialized) ergot message into CAN ID + payload
pub fn encode_frame_raw<'a, Header: CANHeader<'a>>(
    hdr: &HeaderSeq,
    body: &[u8],
    priority: CanPriority,
    buf: &mut [u8],
) -> Result<(u32, usize), CanEncodeError> {
    // Don't do an early bounds check here - the actual header size varies based on
    // address values (varint encoding) and presence of AnyAll appendix. Let
    // serialization fail if there's not enough space, then check final size against
    // CAN_FD_MAX_PAYLOAD.

    let (can_id, payload_hdr) = Header::convert_from_ergot_header_with_priority(hdr, priority);

    let ser = ser_flavors::Slice::new(buf);
    let mut serializer = Serializer { output: ser };

    // Serialize payload header
    payload_hdr
        .serialize(&mut serializer)
        .map_err(CanEncodeError::from_postcard)?;

    // Serialize any/all appendix if present
    if let Some(app) = hdr.any_all.as_ref() {
        serializer
            .output
            .try_extend(&app.key.0)
            .map_err(CanEncodeError::from_postcard)?;
        let nash_val: u32 = app.nash.as_ref().map(NameHash::to_u32).unwrap_or(0);
        nash_val
            .serialize(&mut serializer)
            .map_err(CanEncodeError::from_postcard)?;
    }

    // Append raw body
    serializer
        .output
        .try_extend(body)
        .map_err(CanEncodeError::from_postcard)?;

    let used = serializer
        .output
        .finalize()
        .map_err(CanEncodeError::from_postcard)?;

    if used.len() > CAN_FD_MAX_PAYLOAD {
        return Err(CanEncodeError::PayloadTooLarge);
    }

    Ok((can_id.to_raw(), used.len()))
}

/// Encode a protocol error into CAN ID + payload
pub fn encode_frame_err<'a, Header: CANHeader<'a>>(
    hdr: &HeaderSeq,
    err: ProtocolError,
    priority: CanPriority,
    buf: &mut [u8],
) -> Result<(u32, usize), CanEncodeError> {
    // Don't do an early bounds check - let serialization fail if buffer is too small,
    // which will be mapped to PayloadTooLarge via from_postcard.

    let (can_id, payload_hdr) = Header::convert_from_ergot_header_with_priority(hdr, priority);

    let ser = ser_flavors::Slice::new(buf);
    let mut serializer = Serializer { output: ser };

    // Serialize payload header
    payload_hdr
        .serialize(&mut serializer)
        .map_err(CanEncodeError::from_postcard)?;

    // Serialize error
    err.serialize(&mut serializer)
        .map_err(CanEncodeError::from_postcard)?;

    let used = serializer
        .output
        .finalize()
        .map_err(CanEncodeError::from_postcard)?;

    Ok((can_id.to_raw(), used.len()))
}

/// Decode a CAN FD frame into an ergot message
pub fn decode_frame<'a, Header: CANHeader<'a>>(
    can_id: u32,
    payload: &'a [u8],
) -> Result<CanFrame<'a>, CanDecodeError> {
    // Deserialize payload header
    let (payload_hdr, remain) = postcard::take_from_bytes::<Header::PayloadHeader>(payload)
        .map_err(|_| CanDecodeError::DeserializationError)?;

    let mut header =
        Header::convert_to_ergot_header(&Header::CanFrameId::from_raw(can_id), &payload_hdr);

    let is_err = header.kind == FrameKind::PROTOCOL_ERROR;
    let any_all = [0, 255].contains(&header.dst.port_id);

    // Reject any/all + protocol error combination (matches wire_frames::decode_frame_partial)
    if is_err && any_all {
        return Err(CanDecodeError::InvalidFrameKind);
    }
    // Parse any/all appendix if needed
    let (any_all_appendix, body_data) = if any_all {
        if remain.len() < 8 + 1 {
            return Err(CanDecodeError::PayloadTooShort);
        }
        let key = Key(remain[..8].try_into().unwrap());
        let (nash_val, body) = postcard::take_from_bytes::<u32>(&remain[8..])
            .map_err(|_| CanDecodeError::DeserializationError)?;
        let nash = NameHash::from_u32(nash_val);
        (Some(AnyAllAppendix { key, nash }), body)
    } else {
        (None, remain)
    };

    // Handle error frames
    let body = if is_err {
        let (err, remain) = postcard::take_from_bytes::<ProtocolError>(body_data)
            .map_err(|_| CanDecodeError::DeserializationError)?;
        // Reject error frames with trailing data (matches wire_frames::decode_frame_partial)
        if !remain.is_empty() {
            return Err(CanDecodeError::DeserializationError);
        }
        Err(err)
    } else {
        Ok(body_data)
    };

    header.any_all = any_all_appendix;

    Ok(CanFrame { header, body })
}

// ============================================================================
// Interface Implementation
// ============================================================================

use crate::interface_manager::Interface;

/// A CAN FD interface implementation
///
/// This interface encodes ergot messages with routing-critical fields in the
/// CAN extended ID for hardware filtering, and remaining fields in the payload.
pub struct CanFdInterface<'a, Header = FULL> {
    _marker: PhantomData<Header>,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a, Header: CANHeader<'a>> Interface for CanFdInterface<'a, Header> {
    type Sink = CanFdSink<'a, Header>;
}

/// Configuration for the CAN FD interface
#[derive(Debug, Clone)]
pub struct CanFdConfig {
    /// Default priority for outgoing messages
    pub default_priority: CanPriority,
}

impl Default for CanFdConfig {
    fn default() -> Self {
        Self {
            default_priority: CanPriority::Normal,
        }
    }
}

/// Trait for sending CAN FD frames
///
/// Implement this trait to integrate with your CAN driver (e.g., embedded-can, socketcan)
pub trait CanFdTransmit {
    /// Error type for transmission failures
    type Error;

    /// Transmit a CAN FD frame
    ///
    /// # Arguments
    /// * `id` - The 29-bit extended CAN ID
    /// * `data` - The payload data (up to 64 bytes)
    fn transmit(&mut self, id: u32, data: &[u8]) -> Result<(), Self::Error>;
}

/// Interface sink for CAN FD
///
/// Wraps a CAN transmitter and encodes ergot messages into CAN FD frames.
pub struct CanFdSink<'a, Header: CANHeader<'a> = FULL, T: CanFdTransmit = DummyTransmit> {
    tx: T,
    config: CanFdConfig,
    buf: [u8; CAN_FD_MAX_PAYLOAD],
    _marker: PhantomData<Header>,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a, T: CanFdTransmit, Header: CANHeader<'a>> CanFdSink<'a, Header, T> {
    /// Create a new CAN FD sink with the given transmitter and config
    pub fn new(tx: T, config: CanFdConfig) -> Self {
        Self {
            tx,
            config,
            buf: [0u8; CAN_FD_MAX_PAYLOAD],
            _marker: PhantomData,
            _lifetime: PhantomData,
        }
    }

    /// Get mutable access to the underlying transmitter
    pub fn transmitter_mut(&mut self) -> &mut T {
        &mut self.tx
    }
}

impl<'a, T: CanFdTransmit, Header: CANHeader<'a>> InterfaceSink for CanFdSink<'a, Header, T> {
    fn send_ty<B: Serialize>(&mut self, hdr: &HeaderSeq, body: &B) -> Result<(), ()> {
        if hdr.kind == FrameKind::PROTOCOL_ERROR {
            return Err(());
        }

        let (can_id, len) =
            encode_frame::<B, Header>(hdr, body, self.config.default_priority, &mut self.buf)
                .map_err(|_| ())?;

        self.tx.transmit(can_id, &self.buf[..len]).map_err(|_| ())
    }

    fn send_raw(&mut self, hdr: &HeaderSeq, body: &[u8]) -> Result<(), ()> {
        if hdr.kind == FrameKind::PROTOCOL_ERROR {
            return Err(());
        }

        let (can_id, len) =
            encode_frame_raw::<Header>(hdr, body, self.config.default_priority, &mut self.buf)
                .map_err(|_| ())?;

        self.tx.transmit(can_id, &self.buf[..len]).map_err(|_| ())
    }

    fn send_err(&mut self, hdr: &HeaderSeq, err: ProtocolError) -> Result<(), ()> {
        if hdr.kind != FrameKind::PROTOCOL_ERROR {
            return Err(());
        }

        let (can_id, len) =
            encode_frame_err::<Header>(hdr, err, self.config.default_priority, &mut self.buf)
                .map_err(|_| ())?;

        self.tx.transmit(can_id, &self.buf[..len]).map_err(|_| ())
    }

    fn mtu(&self) -> u16 {
        CAN_FD_MAX_PAYLOAD as u16
    }
}

/// Dummy transmitter for type signatures (not usable)
#[doc(hidden)]
pub struct DummyTransmit;

impl CanFdTransmit for DummyTransmit {
    type Error = ();

    fn transmit(&mut self, _id: u32, _data: &[u8]) -> Result<(), Self::Error> {
        Err(())
    }
}
// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use crate::Address;

    use super::*;

    mod end {
        use super::*;

        #[test]
        fn test_can_id_roundtrip() {
            let id = CanFrameIdEND::new(
                CanPriority::High,
                42,  // dst_node
                123, // dst_port
                FrameKind::ENDPOINT_REQ,
                0,
            );

            assert_eq!(id.priority(), CanPriority::High);
            assert_eq!(id.dst_node_id(), 42);
            assert_eq!(id.dst_port_id(), 123);
            assert_eq!(id.frame_kind(), FrameKind::ENDPOINT_REQ);
            assert_eq!(id.free_data(), 0);

            // Verify it fits in 29 bits
            assert!(id.to_raw() <= CanFrameIdEND::MAX_EXTENDED_ID);
        }

        #[test]
        fn test_can_id_from_header() {
            let hdr = HeaderSeq {
                src: Address {
                    network_id: 1,
                    node_id: 10,
                    port_id: 20,
                },
                dst: Address {
                    network_id: 2,
                    node_id: 30,
                    port_id: 40,
                },
                any_all: None,
                seq_no: 0x1234,
                kind: FrameKind::TOPIC_MSG,
                ttl: 16,
            };

            let (id, _) = END::convert_from_ergot_header(&hdr);

            assert_eq!(id.dst_node_id(), 30);
            assert_eq!(id.dst_port_id(), 40);
            assert_eq!(id.frame_kind(), FrameKind::TOPIC_MSG);
            assert_eq!(id.priority(), CanPriority::Normal);
        }

        #[test]
        fn test_frame_kind_encoding() {
            // Test all frame kinds round-trip correctly
            for (kind, expected_bits) in [
                (FrameKind::RESERVED, 0),
                (FrameKind::ENDPOINT_REQ, 1),
                (FrameKind::ENDPOINT_RESP, 2),
                (FrameKind::TOPIC_MSG, 3),
                (FrameKind::PROTOCOL_ERROR, 7),
            ] {
                let id = CanFrameIdEND::new(CanPriority::Normal, 0, 0, kind, 0);
                assert_eq!(id.frame_kind(), kind, "Frame kind {:?} failed", kind);
                let bits = (id.to_raw() >> CanFrameIdEND::KIND_SHIFT) & 0x7;
                assert_eq!(bits, expected_bits as u32);
            }
        }

        #[test]
        fn test_encode_decode_roundtrip() {
            let hdr = HeaderSeq {
                src: Address {
                    network_id: 100,
                    node_id: 5,
                    port_id: 10,
                },
                dst: Address {
                    network_id: 50,
                    node_id: 15,
                    port_id: 20,
                },
                any_all: None,
                seq_no: 0xABCD,
                kind: FrameKind::ENDPOINT_REQ,
                ttl: 8,
            };

            let body: u32 = 0x12345678;
            let mut buf = [0u8; CAN_FD_MAX_PAYLOAD];

            let (can_id, len) =
                encode_frame::<u32, END>(&hdr, &body, CanPriority::High, &mut buf).unwrap();

            // Decode
            let decoded = decode_frame::<END>(can_id, &buf[..len]).unwrap();

            // assert_eq!(decoded.header.src, hdr.src); //TODO: Fix these two asserts. They currently fail because for the END header variant the network ID we encode/decode depends on the direction in the network we are going (router -> bus node or router <- bus node). See explanation at the top
            // assert_eq!(decoded.header.dst, hdr.dst);
            assert_eq!(decoded.header.kind, hdr.kind);
            // assert_eq!(decoded.header.ttl, hdr.ttl); //TODO: We reset the TTL for the END variant as well. We'll need to rewrite this test

            // Verify body
            let decoded_body: u32 = postcard::from_bytes(decoded.body.unwrap()).unwrap();
            assert_eq!(decoded_body, body);
        }

        #[test]
        fn test_priority_ordering() {
            // Lower CAN ID = higher priority in CAN arbitration
            let high =
                CanFrameIdEND::new(CanPriority::Critical, 10, 10, FrameKind::ENDPOINT_REQ, 0);
            let low = CanFrameIdEND::new(CanPriority::Lowest, 10, 10, FrameKind::ENDPOINT_REQ, 0);

            assert!(high.to_raw() < low.to_raw());
        }

        #[test]
        fn test_filter_masks() {
            // Create two IDs differing only in port
            let id1 = CanFrameIdEND::new(CanPriority::Normal, 42, 1, FrameKind::ENDPOINT_REQ, 0);
            let id2 = CanFrameIdEND::new(CanPriority::Normal, 42, 2, FrameKind::ENDPOINT_REQ, 0);

            // They should match with node-only mask
            let mask = CanFrameIdEND::filter_mask_node_only();
            assert_eq!(id1.to_raw() & mask, id2.to_raw() & mask);

            // But differ with node+port mask
            let mask = CanFrameIdEND::filter_mask_node_port();
            assert_ne!(id1.to_raw() & mask, id2.to_raw() & mask);
        }

        #[test]
        fn test_payload_size() {
            // Verify we can fit a reasonable payload after headers
            let hdr = HeaderSeq {
                src: Address {
                    network_id: 0xFFFF,
                    node_id: 0xFF,
                    port_id: 0xFF,
                },
                dst: Address {
                    network_id: 0xFFFF,
                    node_id: 0xFF,
                    port_id: 0xFF,
                },
                any_all: None,
                seq_no: 0xFFFF,
                kind: FrameKind::ENDPOINT_REQ,
                ttl: 0xFF,
            };

            let body: [u8; 32] = [0xAB; 32];
            let mut buf = [0u8; CAN_FD_MAX_PAYLOAD];

            let result = encode_frame_raw::<END>(&hdr, &body, CanPriority::Normal, &mut buf);

            // With worst-case header (no any/all), we should fit 32 bytes of body
            // Header: ~13 bytes worst case without any/all
            assert!(result.is_ok(), "Should fit 32-byte body with max header");
        }

        #[test]
        fn test_large_raw_payload_with_small_header() {
            // Regression test: encode_frame_raw should not reject valid payloads
            // that fit when the actual header is small (low network IDs, no any/all)
            let hdr = HeaderSeq {
                src: Address {
                    network_id: 1,
                    node_id: 2,
                    port_id: 3,
                },
                dst: Address {
                    network_id: 1,
                    node_id: 4,
                    port_id: 5,
                },
                any_all: None,
                seq_no: 100,
                kind: FrameKind::ENDPOINT_REQ,
                ttl: 16,
            };

            // With small addresses, header is ~7-8 bytes, so 50 bytes of body should fit
            let body: [u8; 50] = [0xCD; 50];
            let mut buf = [0u8; CAN_FD_MAX_PAYLOAD];

            let result = encode_frame_raw::<END>(&hdr, &body, CanPriority::Normal, &mut buf);
            assert!(
                result.is_ok(),
                "Should fit 50-byte body with minimal header, got {:?}",
                result
            );

            let (_, len) = result.unwrap();
            assert!(len <= CAN_FD_MAX_PAYLOAD);
            assert!(len >= 50); // At least the body size
        }

        #[test]
        fn test_oversize_payload_returns_payload_too_large() {
            // Verify that oversize payloads return PayloadTooLarge, not SerializationError
            let hdr = HeaderSeq {
                src: Address {
                    network_id: 1,
                    node_id: 2,
                    port_id: 3,
                },
                dst: Address {
                    network_id: 1,
                    node_id: 4,
                    port_id: 5,
                },
                any_all: None,
                seq_no: 100,
                kind: FrameKind::ENDPOINT_REQ,
                ttl: 16,
            };

            // 65 bytes is guaranteed to exceed CAN FD max (64 bytes)
            let body: [u8; 65] = [0xEE; 65];
            let mut buf = [0u8; CAN_FD_MAX_PAYLOAD];

            let result = encode_frame_raw::<END>(&hdr, &body, CanPriority::Normal, &mut buf);
            assert_eq!(
                result,
                Err(CanEncodeError::PayloadTooLarge),
                "Oversize payload should return PayloadTooLarge, not SerializationError"
            );
        }

        #[test]
        fn test_reject_any_all_protocol_error() {
            // Protocol errors to any/all ports (0 or 255) are invalid per wire_frames spec
            let payload_hdr = CanPayloadHeaderEND {
                net_id: 1,
                src_node: 2,
                src_port: 3,
            };
            let mut buf = [0u8; 32];
            let hdr_len = postcard::to_slice(&payload_hdr, &mut buf).unwrap().len();
            // Serialize a proper ProtocolError after the header
            let err = ProtocolError::Reserved;
            let err_len = postcard::to_slice(&err, &mut buf[hdr_len..]).unwrap().len();
            let total_len = hdr_len + err_len;

            // Error to port 0 (any)
            let can_id =
                CanFrameIdEND::new(CanPriority::Normal, 10, 0, FrameKind::PROTOCOL_ERROR, 0);
            let result = decode_frame::<END>(can_id.to_raw(), &buf[..total_len]);
            assert_eq!(
                result.err(),
                Some(CanDecodeError::InvalidFrameKind),
                "Should reject protocol error to any port (0)"
            );

            // Error to port 255 (all)
            let can_id =
                CanFrameIdEND::new(CanPriority::Normal, 10, 255, FrameKind::PROTOCOL_ERROR, 0);
            let result = decode_frame::<END>(can_id.to_raw(), &buf[..total_len]);
            assert_eq!(
                result.err(),
                Some(CanDecodeError::InvalidFrameKind),
                "Should reject protocol error to all port (255)"
            );

            // Error to specific port should be accepted
            let can_id =
                CanFrameIdEND::new(CanPriority::Normal, 10, 42, FrameKind::PROTOCOL_ERROR, 0);
            let result = decode_frame::<END>(can_id.to_raw(), &buf[..total_len]);
            assert!(
                result.is_ok(),
                "Should accept protocol error to specific port"
            );

            // Error with trailing data should be rejected
            buf[total_len] = 0xAB; // trailing byte
            let result = decode_frame::<END>(can_id.to_raw(), &buf[..total_len + 1]);
            assert_eq!(
                result.err(),
                Some(CanDecodeError::DeserializationError),
                "Should reject protocol error with trailing data"
            );
        }
    }

    mod full {
        use super::*;

        #[test]
        fn test_can_id_roundtrip() {
            let id = CanFrameIdFULL::new(
                CanPriority::High,
                2,
                42,  // dst_node
                123, // dst_port
            );

            assert_eq!(id.priority(), CanPriority::High);
            assert_eq!(id.dst_net_id(), 2);
            assert_eq!(id.dst_node_id(), 42);
            assert_eq!(id.dst_port_id(), 123);

            // Verify it fits in 29 bits
            assert!(id.to_raw() <= CanFrameIdFULL::MAX_EXTENDED_ID);
        }

        #[test]
        fn test_can_id_from_header() {
            let hdr = HeaderSeq {
                src: Address {
                    network_id: 1,
                    node_id: 10,
                    port_id: 20,
                },
                dst: Address {
                    network_id: 2,
                    node_id: 30,
                    port_id: 40,
                },
                any_all: None,
                seq_no: 0x1234,
                kind: FrameKind::TOPIC_MSG,
                ttl: 16,
            };

            let (id, _) = FULL::convert_from_ergot_header(&hdr);

            assert_eq!(id.dst_net_id(), 2);
            assert_eq!(id.dst_node_id(), 30);
            assert_eq!(id.dst_port_id(), 40);
            assert_eq!(id.priority(), CanPriority::Normal);
        }

        #[test]
        fn test_encode_decode_roundtrip() {
            let hdr = HeaderSeq {
                src: Address {
                    network_id: 100,
                    node_id: 5,
                    port_id: 10,
                },
                dst: Address {
                    network_id: 200,
                    node_id: 15,
                    port_id: 20,
                },
                any_all: None,
                seq_no: 0xABCD,
                kind: FrameKind::ENDPOINT_REQ,
                ttl: 8,
            };

            let body: u32 = 0x12345678;
            let mut buf = [0u8; CAN_FD_MAX_PAYLOAD];

            let (can_id, len) =
                encode_frame::<u32, FULL>(&hdr, &body, CanPriority::High, &mut buf).unwrap();

            // Decode
            let decoded = decode_frame::<FULL>(can_id, &buf[..len]).unwrap();

            assert_eq!(decoded.header.src, hdr.src);
            assert_eq!(decoded.header.dst, hdr.dst);
            assert_eq!(decoded.header.kind, hdr.kind);
            assert_eq!(decoded.header.ttl, hdr.ttl);

            // Verify body
            let decoded_body: u32 = postcard::from_bytes(decoded.body.unwrap()).unwrap();
            assert_eq!(decoded_body, body);
        }

        #[test]
        fn test_priority_ordering() {
            // Lower CAN ID = higher priority in CAN arbitration
            let high = CanFrameIdFULL::new(CanPriority::Critical, 10, 10, 10);
            let low = CanFrameIdFULL::new(CanPriority::Lowest, 10, 10, 10);

            assert!(high.to_raw() < low.to_raw());
        }

        #[test]
        fn test_filter_masks() {
            // Create two IDs differing only in port
            let id1 = CanFrameIdFULL::new(CanPriority::Normal, 1, 42, 1);
            let id2 = CanFrameIdFULL::new(CanPriority::Normal, 1, 42, 2);

            // They should match with node-only mask
            let mask = CanFrameIdFULL::filter_mask_node_only();
            assert_eq!(id1.to_raw() & mask, id2.to_raw() & mask);

            // But differ with node+port mask
            let mask = CanFrameIdFULL::filter_mask_node_port();
            assert_ne!(id1.to_raw() & mask, id2.to_raw() & mask);
        }

        #[test]
        fn test_payload_size() {
            // Verify we can fit a reasonable payload after headers
            let hdr = HeaderSeq {
                src: Address {
                    network_id: 0xFFFF,
                    node_id: 0xFF,
                    port_id: 0xFF,
                },
                dst: Address {
                    network_id: 0xFFFF,
                    node_id: 0xFF,
                    port_id: 0xFF,
                },
                any_all: None,
                seq_no: 0xFFFF,
                kind: FrameKind::ENDPOINT_REQ,
                ttl: 0xFF,
            };

            let body: [u8; 32] = [0xAB; 32];
            let mut buf = [0u8; CAN_FD_MAX_PAYLOAD];

            let result = encode_frame_raw::<FULL>(&hdr, &body, CanPriority::Normal, &mut buf);

            // With worst-case header (no any/all), we should fit 32 bytes of body
            // Header: ~13 bytes worst case without any/all
            assert!(result.is_ok(), "Should fit 32-byte body with max header");
        }

        #[test]
        fn test_large_raw_payload_with_small_header() {
            // Regression test: encode_frame_raw should not reject valid payloads
            // that fit when the actual header is small (low network IDs, no any/all)
            let hdr = HeaderSeq {
                src: Address {
                    network_id: 1,
                    node_id: 2,
                    port_id: 3,
                },
                dst: Address {
                    network_id: 1,
                    node_id: 4,
                    port_id: 5,
                },
                any_all: None,
                seq_no: 100,
                kind: FrameKind::ENDPOINT_REQ,
                ttl: 16,
            };

            // With small addresses, header is ~7-8 bytes, so 50 bytes of body should fit
            let body: [u8; 50] = [0xCD; 50];
            let mut buf = [0u8; CAN_FD_MAX_PAYLOAD];

            let result = encode_frame_raw::<FULL>(&hdr, &body, CanPriority::Normal, &mut buf);
            assert!(
                result.is_ok(),
                "Should fit 50-byte body with minimal header, got {:?}",
                result
            );

            let (_, len) = result.unwrap();
            assert!(len <= CAN_FD_MAX_PAYLOAD);
            assert!(len >= 50); // At least the body size
        }

        #[test]
        fn test_oversize_payload_returns_payload_too_large() {
            // Verify that oversize payloads return PayloadTooLarge, not SerializationError
            let hdr = HeaderSeq {
                src: Address {
                    network_id: 1,
                    node_id: 2,
                    port_id: 3,
                },
                dst: Address {
                    network_id: 1,
                    node_id: 4,
                    port_id: 5,
                },
                any_all: None,
                seq_no: 100,
                kind: FrameKind::ENDPOINT_REQ,
                ttl: 16,
            };

            // 65 bytes is guaranteed to exceed CAN FD max (64 bytes)
            let body: [u8; 65] = [0xEE; 65];
            let mut buf = [0u8; CAN_FD_MAX_PAYLOAD];

            let result = encode_frame_raw::<FULL>(&hdr, &body, CanPriority::Normal, &mut buf);
            assert_eq!(
                result,
                Err(CanEncodeError::PayloadTooLarge),
                "Oversize payload should return PayloadTooLarge, not SerializationError"
            );
        }

        #[test]
        fn test_reject_any_all_protocol_error() {
            // Protocol errors to any/all ports (0 or 255) are invalid per wire_frames spec
            let payload_hdr = CanPayloadHeaderFULL {
                src_net: 1,
                src_node: 2,
                src_port: 3,
                ttl: 16,
                kind: frame_kind_to_bits(FrameKind::PROTOCOL_ERROR),
            };
            let mut buf = [0u8; 32];
            let hdr_len = postcard::to_slice(&payload_hdr, &mut buf).unwrap().len();
            // Serialize a proper ProtocolError after the header
            let err = ProtocolError::Reserved;
            let err_len = postcard::to_slice(&err, &mut buf[hdr_len..]).unwrap().len();
            let total_len = hdr_len + err_len;

            // Error to port 0 (any)
            let can_id = CanFrameIdFULL::new(CanPriority::Normal, 0, 10, 0);
            let result = decode_frame::<FULL>(can_id.to_raw(), &buf[..total_len]);
            assert_eq!(
                result.err(),
                Some(CanDecodeError::InvalidFrameKind),
                "Should reject protocol error to any port (0)"
            );

            // Error to port 255 (all)
            let can_id = CanFrameIdFULL::new(CanPriority::Normal, 0, 10, 255);
            let result = decode_frame::<FULL>(can_id.to_raw(), &buf[..total_len]);
            assert_eq!(
                result.err(),
                Some(CanDecodeError::InvalidFrameKind),
                "Should reject protocol error to all port (255)"
            );

            // Error to specific port should be accepted
            let can_id = CanFrameIdFULL::new(CanPriority::Normal, 0, 10, 42);
            let result = decode_frame::<FULL>(can_id.to_raw(), &buf[..total_len]);
            assert!(
                result.is_ok(),
                "Should accept protocol error to specific port"
            );

            // Error with trailing data should be rejected
            buf[total_len] = 0xAB; // trailing byte
            let result = decode_frame::<FULL>(can_id.to_raw(), &buf[..total_len + 1]);
            assert_eq!(
                result.err(),
                Some(CanDecodeError::DeserializationError),
                "Should reject protocol error with trailing data"
            );
        }
    }
}
