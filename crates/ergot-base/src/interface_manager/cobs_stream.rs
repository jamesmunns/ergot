use bbq2::{prod_cons::stream::StreamProducer, traits::bbqhdl::BbqHandle};
use postcard::ser_flavors::{self, Flavor};
use serde::Serialize;

use crate::{
    AnyAllAppendix, FrameKind, ProtocolError,
    wire_frames::{self, CommonHeader},
};

pub struct Interface<Q>
where
    Q: BbqHandle,
{
    pub(crate) mtu: u16,
    pub(crate) prod: StreamProducer<Q>,
}

#[allow(clippy::result_unit_err)] // todo
impl<Q> Interface<Q>
where
    Q: BbqHandle,
{
    pub fn send_ty<T: Serialize>(
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

        let max_len = cobs::max_encoding_length(self.mtu as usize);
        let mut wgr = self.prod.grant_exact(max_len).map_err(drop)?;

        let ser = ser_flavors::Cobs::try_new(ser_flavors::Slice::new(&mut wgr)).map_err(drop)?;
        let used = wire_frames::encode_frame_ty(ser, hdr, apdx, body).map_err(drop)?;
        let len = used.len();
        wgr.commit(len);

        Ok(())
    }

    pub fn send_raw(&mut self, hdr: &CommonHeader, hdr_raw: &[u8], body: &[u8]) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR;

        if is_err {
            // todo: use a different interface for this
            return Err(());
        }

        let max_len = cobs::max_encoding_length(hdr_raw.len() + body.len());
        let mut wgr = self.prod.grant_exact(max_len).map_err(drop)?;

        let mut ser =
            ser_flavors::Cobs::try_new(ser_flavors::Slice::new(&mut wgr)).map_err(drop)?;
        ser.try_extend(hdr_raw).map_err(drop)?;
        ser.try_extend(body).map_err(drop)?;
        let fin = ser.finalize().map_err(drop)?;
        let len = fin.len();
        wgr.commit(len);

        Ok(())
    }

    pub fn send_err(&mut self, hdr: &CommonHeader, err: ProtocolError) -> Result<(), ()> {
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
