//! Framed Stream
//!
//! The "Framed Stream" is one flavor of interface sinks. It is intended for packet-like
//! interfaces that do NOT require framing in software.

use bbq2::{prod_cons::framed::FramedProducer, traits::bbqhdl::BbqHandle};
use postcard::{
    Serializer,
    ser_flavors::{self, Flavor, Slice},
};
use serde::Serialize;

use crate::{
    FrameKind, HeaderSeq, ProtocolError,
    interface_manager::InterfaceSink,
    wire_frames::{self, MAX_HDR_ENCODED_SIZE, encode_frame_hdr},
};

pub struct Sink<Q>
where
    Q: BbqHandle,
{
    pub(crate) mtu: u16,
    pub(crate) prod: FramedProducer<Q, u16>,
}

#[allow(clippy::result_unit_err)] // todo
impl<Q> Sink<Q>
where
    Q: BbqHandle,
{
    pub fn new_from_handle(q: Q, mtu: u16) -> Self {
        Self {
            mtu,
            prod: q.framed_producer(),
        }
    }

    pub const fn new(prod: FramedProducer<Q, u16>, mtu: u16) -> Self {
        Self { mtu, prod }
    }
}

impl<Q> InterfaceSink for Sink<Q>
where
    Q: BbqHandle,
{
    fn send_ty<T: Serialize>(&mut self, hdr: &HeaderSeq, body: &T) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR;

        if is_err {
            // todo: use a different interface for this
            return Err(());
        }
        let mut wgr = self.prod.grant(self.mtu).map_err(drop)?;

        let ser = ser_flavors::Slice::new(&mut wgr);
        let used = wire_frames::encode_frame_ty(ser, hdr, body).map_err(drop)?;
        let len = used.len() as u16;
        wgr.commit(len);

        Ok(())
    }

    fn send_raw(&mut self, hdr: &HeaderSeq, body: &[u8]) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR;

        if is_err {
            // todo: use a different interface for this
            return Err(());
        }
        let max_len = MAX_HDR_ENCODED_SIZE + body.len();
        let Ok(max_len) = u16::try_from(max_len) else {
            return Err(());
        };
        let mut wgr = self.prod.grant(max_len).map_err(drop)?;
        let mut ser = Serializer {
            output: Slice::new(&mut wgr),
        };
        encode_frame_hdr(&mut ser, hdr).map_err(drop)?;
        ser.output.try_extend(body).map_err(drop)?;
        let used = ser.output.finalize().map_err(drop)?.len();
        wgr.commit(u16::try_from(used).map_err(drop)?);

        Ok(())
    }

    fn send_err(&mut self, hdr: &HeaderSeq, err: ProtocolError) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR;

        // note: here it SHOULD be an err!
        if !is_err {
            // todo: use a different interface for this
            return Err(());
        }
        let mut wgr = self.prod.grant(self.mtu).map_err(drop)?;

        let ser = ser_flavors::Slice::new(&mut wgr);
        let used = wire_frames::encode_frame_err(ser, hdr, err).map_err(drop)?;
        let len = used.len() as u16;
        wgr.commit(len);

        Ok(())
    }
}
