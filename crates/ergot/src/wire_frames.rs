use crate::logging::warn;
use postcard::{Serializer, ser_flavors};
use serde::{Deserialize, Serialize};

use crate::{
    Address, AnyAllAppendix, FrameKind, Header, Key, MAX_TTL, ProtocolError, TrafficClass,
    nash::NameHash,
};

/// The fixed part of every frame header, as decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct CommonHeader {
    pub src: Address,
    pub dst: Address,
    pub kind: FrameKind,
    pub class: TrafficClass,
    pub ttl: u8,
}

/// The fixed part of every frame header, as encoded: `kind`, `class` and
/// `ttl` share one byte.
///
/// ```text
///  bit 7 6 | 5 4   | 3 2 1 0
///      kind| class | ttl
/// ```
#[derive(Serialize, Deserialize, Debug)]
struct WireCommonHeader {
    // WARNING: Update MAX_HDR_ENCODED_SIZE if you add/remove anything here!
    src: Address,
    dst: Address,
    meta: u8,
    // WARNING: Update MAX_HDR_ENCODED_SIZE if you add/remove anything here!
}

const KIND_SHIFT: u8 = 6;
const CLASS_SHIFT: u8 = 4;
const TTL_MASK: u8 = 0x0F;

/// Pack `kind`, `class` and `ttl` into the wire byte. `ttl` is clamped to
/// [`MAX_TTL`]; `kind` must be one of the four wire kinds (debug-asserted,
/// masked in release).
pub const fn pack_meta(kind: FrameKind, class: TrafficClass, ttl: u8) -> u8 {
    debug_assert!(kind.is_valid());
    let ttl = if ttl > MAX_TTL { MAX_TTL } else { ttl };
    ((kind.0 & FrameKind::MAX_BITS) << KIND_SHIFT) | (class.to_bits() << CLASS_SHIFT) | ttl
}

/// Unpack the wire byte. Every bit pattern is a valid header (all four kinds
/// and classes exist), so this cannot fail.
pub const fn unpack_meta(meta: u8) -> (FrameKind, TrafficClass, u8) {
    (
        FrameKind((meta >> KIND_SHIFT) & FrameKind::MAX_BITS),
        TrafficClass::from_bits(meta >> CLASS_SHIFT),
        meta & TTL_MASK,
    )
}

impl From<&Header> for CommonHeader {
    fn from(value: &Header) -> Self {
        Self {
            src: value.src,
            dst: value.dst,
            kind: value.kind,
            class: value.class,
            ttl: value.ttl,
        }
    }
}

impl From<&CommonHeader> for WireCommonHeader {
    fn from(value: &CommonHeader) -> Self {
        Self {
            src: value.src,
            dst: value.dst,
            meta: pack_meta(value.kind, value.class, value.ttl),
        }
    }
}

impl From<WireCommonHeader> for CommonHeader {
    fn from(value: WireCommonHeader) -> Self {
        let (kind, class, ttl) = unpack_meta(value.meta);
        Self {
            src: value.src,
            dst: value.dst,
            kind,
            class,
            ttl,
        }
    }
}

pub enum PartialDecodeTail<'a> {
    // WARNING: Update MAX_HDR_ENCODED_SIZE if you add/remove anything here!
    Specific(&'a [u8]),
    AnyAll {
        apdx: AnyAllAppendix,
        body: &'a [u8],
    },
    Err(ProtocolError),
    // WARNING: Update MAX_HDR_ENCODED_SIZE if you add/remove anything here!
}

pub struct PartialDecode<'a> {
    pub hdr: CommonHeader,
    pub tail: PartialDecodeTail<'a>,
    /// hdr_raw MUST contain the full serialized header, inclusive of the
    /// "AnyAllAppendix" if present.
    pub hdr_raw: &'a [u8],
}

pub(crate) fn decode_frame_partial(data: &[u8]) -> Option<PartialDecode<'_>> {
    let (common, remain) = postcard::take_from_bytes::<WireCommonHeader>(data).ok()?;
    let common = CommonHeader::from(common);
    let is_err = common.kind == FrameKind::PROTOCOL_ERROR;
    let any_all = [0, 255].contains(&common.dst.port_id);

    match (is_err, any_all) {
        // Not allowed: any/all AND is err
        (true, true) => {
            warn!("Rejecting any/all protocol error message");
            None
        }
        (true, false) => {
            let hdr_raw_len = data.len() - remain.len();
            let hdr_raw = &data[..hdr_raw_len];
            // err
            let (err, remain) = postcard::take_from_bytes::<ProtocolError>(remain).ok()?;
            if !remain.is_empty() {
                warn!("Excess data, rejecting");
                return None;
            }
            Some(PartialDecode {
                hdr: common,
                tail: PartialDecodeTail::Err(err),
                hdr_raw,
            })
        }
        (false, true) => {
            let (key, remain) = postcard::take_from_bytes::<Key>(remain).ok()?;
            let (nash, remain) = postcard::take_from_bytes::<u32>(remain).ok()?;
            let hdr_raw_len = data.len() - remain.len();
            let hdr_raw = &data[..hdr_raw_len];

            Some(PartialDecode {
                hdr: common,
                tail: PartialDecodeTail::AnyAll {
                    apdx: AnyAllAppendix {
                        key,
                        nash: NameHash::from_u32(nash),
                    },
                    body: remain,
                },
                hdr_raw,
            })
        }
        (false, false) => {
            let hdr_raw_len = data.len() - remain.len();
            let hdr_raw = &data[..hdr_raw_len];

            Some(PartialDecode {
                hdr: common,
                tail: PartialDecodeTail::Specific(remain),
                hdr_raw,
            })
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum EncodeFrameError {
    SerializationError(postcard::Error),
}

impl From<postcard::Error> for EncodeFrameError {
    fn from(value: postcard::Error) -> Self {
        Self::SerializationError(value)
    }
}
/// The largest encoded size of a header, usable for creating a max-sized buffer
///
/// ```text
/// WireCommonHeader=============================
/// src: Address,            u32, varint: 5 bytes
/// dst: Address,            u32, varint: 5 bytes
/// meta (kind|class|ttl),   u8, !varint: 1 byte
/// AnyAllAppendix===============================
/// key: Key,                [u8; 8]:     8 bytes
/// nash: Option<NameHash>,  u32, varint: 5 bytes
/// ==================================== 24 bytes
/// ```
//
// TODO: A more automatic way of handling this. This is currently tested with a
// unit test below.
pub const MAX_HDR_ENCODED_SIZE: usize = 24;

/// Encode the frame header to the given serializer
pub fn encode_frame_hdr<F>(ser: &mut Serializer<F>, hdr: &Header) -> Result<(), EncodeFrameError>
where
    F: ser_flavors::Flavor,
{
    let chdr: CommonHeader = hdr.into();
    let whdr: WireCommonHeader = (&chdr).into();
    whdr.serialize(&mut *ser)?;

    if let Some(app) = hdr.any_all.as_ref() {
        ser.output.try_extend(&app.key.0)?;
        let val: u32 = app.nash.as_ref().map(NameHash::to_u32).unwrap_or(0);
        val.serialize(ser)?;
    }

    Ok(())
}

// must not be error
// doesn't check if dest is actually any/all
pub fn encode_frame_ty<F, T>(flav: F, hdr: &Header, body: &T) -> Result<F::Output, EncodeFrameError>
where
    F: ser_flavors::Flavor,
    T: Serialize,
{
    let mut serializer = Serializer { output: flav };
    encode_frame_hdr(&mut serializer, hdr)?;

    body.serialize(&mut serializer)?;
    Ok(serializer.output.finalize()?)
}

pub fn encode_frame_err<F>(
    flav: F,
    hdr: &Header,
    err: ProtocolError,
) -> Result<F::Output, EncodeFrameError>
where
    F: ser_flavors::Flavor,
{
    let mut serializer = Serializer { output: flav };
    let chdr: CommonHeader = hdr.into();
    let whdr: WireCommonHeader = (&chdr).into();
    whdr.serialize(&mut serializer)?;
    err.serialize(&mut serializer)?;
    Ok(serializer.output.finalize()?)
}

pub fn de_frame(remain: &[u8]) -> Option<BorrowedFrame<'_>> {
    let res = decode_frame_partial(remain)?;

    let app;
    let body = match res.tail {
        PartialDecodeTail::Specific(body) => {
            app = None;
            Ok(body)
        }
        PartialDecodeTail::AnyAll { apdx, body } => {
            app = Some(apdx);
            Ok(body)
        }
        PartialDecodeTail::Err(protocol_error) => {
            app = None;
            Err(protocol_error)
        }
    };

    let CommonHeader {
        src,
        dst,
        kind,
        class,
        ttl,
    } = res.hdr;

    Some(BorrowedFrame {
        hdr: Header {
            src,
            dst,
            any_all: app,
            kind,
            class,
            ttl,
        },
        body,
    })
}

pub struct BorrowedFrame<'a> {
    pub hdr: Header,
    pub body: Result<&'a [u8], ProtocolError>,
}

#[cfg(all(test, feature = "std"))]
mod test {
    use postcard::{Serializer, ser_flavors::Flavor};

    use crate::{
        Address, AnyAllAppendix, FrameKind, Header, Key, MAX_TTL, TrafficClass,
        nash::NameHash,
        wire_frames::{MAX_HDR_ENCODED_SIZE, pack_meta, unpack_meta},
    };

    use super::encode_frame_hdr;

    #[test]
    fn max_hdr_ser_size() {
        let hdr = Header {
            // Addresses: maximum integer values
            src: Address {
                network_id: u16::MAX,
                node_id: u8::MAX,
                port_id: u8::MAX,
            },
            dst: Address {
                network_id: u16::MAX,
                node_id: u8::MAX,
                port_id: u8::MAX,
            },
            kind: FrameKind::TOPIC_MSG,
            class: TrafficClass::Background,
            ttl: MAX_TTL,
            any_all: Some(AnyAllAppendix {
                key: Key([0xFFu8; 8]),
                nash: NameHash::from_u32(u32::MAX),
            }),
        };
        let flav = postcard::ser_flavors::StdVec::new();
        let mut ser = Serializer { output: flav };
        encode_frame_hdr(&mut ser, &hdr).unwrap();
        let res = ser.output.finalize().unwrap();
        assert_eq!(res.len(), MAX_HDR_ENCODED_SIZE);
    }

    #[test]
    fn meta_byte_round_trips_every_kind_class_ttl() {
        for kind in [
            FrameKind::PROTOCOL_ERROR,
            FrameKind::ENDPOINT_REQ,
            FrameKind::ENDPOINT_RESP,
            FrameKind::TOPIC_MSG,
        ] {
            for class in [
                TrafficClass::Control,
                TrafficClass::Normal,
                TrafficClass::Bulk,
                TrafficClass::Background,
            ] {
                for ttl in 0..=MAX_TTL {
                    let meta = pack_meta(kind, class, ttl);
                    assert_eq!(unpack_meta(meta), (kind, class, ttl));
                }
            }
        }
    }

    #[test]
    fn meta_byte_clamps_oversized_ttl() {
        let meta = pack_meta(FrameKind::ENDPOINT_REQ, TrafficClass::Normal, 200);
        assert_eq!(unpack_meta(meta).2, MAX_TTL);
    }

    #[test]
    fn every_meta_byte_decodes() {
        // All 256 values are valid headers: no reserved kind, no reserved class.
        for meta in 0..=u8::MAX {
            let (kind, class, ttl) = unpack_meta(meta);
            assert!(kind.is_valid());
            assert_eq!(pack_meta(kind, class, ttl), meta);
        }
    }
}
