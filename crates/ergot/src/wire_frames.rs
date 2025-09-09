use log::warn;
use postcard::{Serializer, ser_flavors};
use serde::{Deserialize, Serialize};

use crate::{Address, AnyAllAppendix, FrameKind, HeaderSeq, Key, ProtocolError, nash::NameHash};

#[derive(Serialize, Deserialize, Debug)]
pub struct CommonHeader {
    // WARNING: Update MAX_HDR_ENCODED_SIZE if you add/remove anything here!
    pub src: Address,
    pub dst: Address,
    pub seq_no: u16,
    pub kind: FrameKind,
    pub ttl: u8,
    // WARNING: Update MAX_HDR_ENCODED_SIZE if you add/remove anything here!
}

impl From<&HeaderSeq> for CommonHeader {
    fn from(value: &HeaderSeq) -> Self {
        Self {
            src: value.src,
            dst: value.dst,
            seq_no: value.seq_no,
            kind: value.kind,
            ttl: value.ttl,
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
    let (common, remain) = postcard::take_from_bytes::<CommonHeader>(data).ok()?;
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
/// CommonHeader=================================
/// src: Address,            u32, varint: 5 bytes
/// dst: Address,            u32, varint: 5 bytes
/// seq_no: u16,             u16, varint: 3 bytes
/// kind: FrameKind,         u8, !varint: 1 byte
/// ttl: u8,                 u8, !varint: 1 byte
/// AnyAllAppendix===============================
/// key: Key,                [u8; 8]:     8 bytes
/// nash: Option<NameHash>,  u32, varint: 5 bytes
/// ==================================== 28 bytes
/// ```
//
// TODO: A more automatic way of handling this. This is currently tested with a
// unit test below.
pub const MAX_HDR_ENCODED_SIZE: usize = 28;

/// Encode the frame header to the given serializer
pub fn encode_frame_hdr<F>(ser: &mut Serializer<F>, hdr: &HeaderSeq) -> Result<(), EncodeFrameError>
where
    F: ser_flavors::Flavor,
{
    let chdr: CommonHeader = hdr.into();
    chdr.serialize(&mut *ser)?;

    if let Some(app) = hdr.any_all.as_ref() {
        ser.output.try_extend(&app.key.0)?;
        let val: u32 = app.nash.as_ref().map(NameHash::to_u32).unwrap_or(0);
        val.serialize(ser)?;
    }

    Ok(())
}

// must not be error
// doesn't check if dest is actually any/all
pub fn encode_frame_ty<F, T>(
    flav: F,
    hdr: &HeaderSeq,
    body: &T,
) -> Result<F::Output, EncodeFrameError>
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
    hdr: &HeaderSeq,
    err: ProtocolError,
) -> Result<F::Output, EncodeFrameError>
where
    F: ser_flavors::Flavor,
{
    let mut serializer = Serializer { output: flav };
    let chdr: CommonHeader = hdr.into();
    chdr.serialize(&mut serializer)?;
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
        seq_no,
        kind,
        ttl,
    } = res.hdr;

    Some(BorrowedFrame {
        hdr: HeaderSeq {
            src,
            dst,
            seq_no,
            any_all: app,
            kind,
            ttl,
        },
        body,
    })
}

pub struct BorrowedFrame<'a> {
    pub hdr: HeaderSeq,
    pub body: Result<&'a [u8], ProtocolError>,
}

#[cfg(all(test, feature = "std"))]
mod test {
    use postcard::{Serializer, ser_flavors::Flavor};

    use crate::{
        Address, AnyAllAppendix, FrameKind, HeaderSeq, Key, nash::NameHash,
        wire_frames::MAX_HDR_ENCODED_SIZE,
    };

    use super::encode_frame_hdr;

    #[test]
    fn max_hdr_ser_size() {
        let hdr = HeaderSeq {
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
            seq_no: u16::MAX,
            kind: FrameKind(u8::MAX),
            ttl: u8::MAX,
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
}
