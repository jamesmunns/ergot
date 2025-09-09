//! Cobs Stream
//!
//! The "Cobs Stream" is one flavor of interface sinks. It is intended for serial-like
//! interfaces that require framing in software.

use bbq2::{prod_cons::stream::StreamProducer, traits::bbqhdl::BbqHandle};
use postcard::{
    Serializer,
    ser_flavors::{self, Flavor},
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
    pub(crate) prod: StreamProducer<Q>,
}

#[allow(clippy::result_unit_err)] // todo
impl<Q> Sink<Q>
where
    Q: BbqHandle,
{
    pub fn new_from_handle(q: Q, mtu: u16) -> Self {
        Self {
            mtu,
            prod: q.stream_producer(),
        }
    }

    pub const fn new(prod: StreamProducer<Q>, mtu: u16) -> Self {
        Self { mtu, prod }
    }
}

#[allow(clippy::result_unit_err)] // todo
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

        let max_len = cobs::max_encoding_length(self.mtu as usize);
        let mut wgr = self.prod.grant_exact(max_len).map_err(drop)?;

        let ser = ser_flavors::Cobs::try_new(ser_flavors::Slice::new(&mut wgr)).map_err(drop)?;
        let used = wire_frames::encode_frame_ty(ser, hdr, body).map_err(drop)?;
        let len = used.len();
        wgr.commit(len);

        Ok(())
    }

    fn send_raw(&mut self, hdr: &HeaderSeq, body: &[u8]) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR;

        if is_err {
            // todo: use a different interface for this
            return Err(());
        }
        let max_len = cobs::max_encoding_length(MAX_HDR_ENCODED_SIZE + body.len());
        let mut wgr = self.prod.grant_exact(max_len).map_err(drop)?;

        let mut ser = Serializer {
            output: ser_flavors::Cobs::try_new(ser_flavors::Slice::new(&mut wgr)).map_err(drop)?,
        };
        encode_frame_hdr(&mut ser, hdr).map_err(drop)?;
        ser.output.try_extend(body).map_err(drop)?;
        let fin = ser.output.finalize().map_err(drop)?;
        let len = fin.len();
        wgr.commit(len);

        Ok(())
    }

    fn send_err(&mut self, hdr: &HeaderSeq, err: ProtocolError) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR;

        // note: here it SHOULD be an err!
        if !is_err {
            // todo: use a different interface for this
            return Err(());
        }

        let max_len = cobs::max_encoding_length(self.mtu as usize);
        let mut wgr = self.prod.grant_exact(max_len).map_err(drop)?;

        let ser = ser_flavors::Cobs::try_new(ser_flavors::Slice::new(&mut wgr)).map_err(drop)?;
        let used = wire_frames::encode_frame_err(ser, hdr, err).map_err(drop)?;
        let len = used.len();
        wgr.commit(len);

        Ok(())
    }
}
