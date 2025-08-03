use log::warn;
use postcard::{Serializer, ser_flavors};
use serde::{Deserialize, Serialize};

use crate::{Address, AnyAllAppendix, FrameKind, HeaderSeq, Key, ProtocolError, nash::NameHash};

#[derive(Serialize, Deserialize, Debug)]
pub struct CommonHeader {
    pub src: Address,
    pub dst: Address,
    pub seq_no: u16,
    pub kind: FrameKind,
    pub ttl: u8,
}

pub enum PartialDecodeTail<'a> {
    Specific(&'a [u8]),
    AnyAll {
        apdx: AnyAllAppendix,
        body: &'a [u8],
    },
    Err(ProtocolError),
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

// must not be error
// doesn't check if dest is actually any/all
pub fn encode_frame_ty<F, T>(
    flav: F,
    hdr: &CommonHeader,
    apdx: Option<&AnyAllAppendix>,
    body: &T,
) -> Result<F::Output, ()>
where
    F: ser_flavors::Flavor,
    T: Serialize,
{
    let mut serializer = Serializer { output: flav };
    hdr.serialize(&mut serializer).map_err(drop)?;

    if let Some(app) = apdx {
        serializer.output.try_extend(&app.key.0).map_err(drop)?;
        let val: u32 = app.nash.as_ref().map(NameHash::to_u32).unwrap_or(0);
        val.serialize(&mut serializer).map_err(drop)?;
    }

    body.serialize(&mut serializer).map_err(drop)?;
    serializer.output.finalize().map_err(drop)
}

pub fn encode_frame_err<F>(flav: F, hdr: &CommonHeader, err: ProtocolError) -> Result<F::Output, ()>
where
    F: ser_flavors::Flavor,
{
    let mut serializer = Serializer { output: flav };
    hdr.serialize(&mut serializer).map_err(drop)?;
    err.serialize(&mut serializer).map_err(drop)?;
    serializer.output.finalize().map_err(drop)
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
        hdr_raw: res.hdr_raw,
    })
}

pub struct BorrowedFrame<'a> {
    pub hdr: HeaderSeq,
    pub hdr_raw: &'a [u8],
    pub body: Result<&'a [u8], ProtocolError>,
}
