//! Framed Stream
//!
//! The "Framed Stream" is one flavor of interface sinks. It is intended for packet-like
//! interfaces that do NOT require framing in software.

use bbq2::{prod_cons::framed::FramedProducer, traits::bbqhdl::BbqHandle};
use postcard::ser_flavors;
use serde::Serialize;

use crate::{
    AnyAllAppendix, FrameKind, ProtocolError,
    interface_manager::InterfaceSink,
    wire_frames::{self, CommonHeader},
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
    fn send_ty<T: Serialize>(
        &mut self,
        hdr: &CommonHeader,
        apdx: Option<&AnyAllAppendix>,
        body: &T,
    ) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR;

        if is_err {
            // todo: use a different interface for this
            return Err(());
        }
        let mut wgr = self.prod.grant(self.mtu).map_err(drop)?;

        let ser = ser_flavors::Slice::new(&mut wgr);
        let used = wire_frames::encode_frame_ty(ser, hdr, apdx, body).map_err(drop)?;
        let len = used.len() as u16;
        wgr.commit(len);

        Ok(())
    }

    fn send_raw(&mut self, hdr: &CommonHeader, hdr_raw: &[u8], body: &[u8]) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR;

        if is_err {
            // todo: use a different interface for this
            return Err(());
        }
        let len = hdr_raw.len() + body.len();
        let Ok(len) = u16::try_from(len) else {
            return Err(());
        };
        let mut wgr = self.prod.grant(len).map_err(drop)?;
        let (ghdr, gbody) = wgr.split_at_mut(hdr_raw.len());
        ghdr.copy_from_slice(hdr_raw);
        gbody.copy_from_slice(body);

        wgr.commit(len);

        Ok(())
    }

    fn send_err(&mut self, hdr: &CommonHeader, err: ProtocolError) -> Result<(), ()> {
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
