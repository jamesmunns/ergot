use bbq2::{prod_cons::stream::StreamProducer, traits::bbqhdl::BbqHandle};
use postcard::ser_flavors;
use serde::Serialize;

use crate::{
    FrameKind, Key, ProtocolError,
    interface_manager::wire_frames::{self, CommonHeader},
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
        key: Option<&Key>,
        body: &T,
    ) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR.0;

        if is_err {
            // todo: use a different interface for this
            return Err(());
        }

        let max_len = cobs::max_encoding_length(self.mtu as usize);
        let mut wgr = self.prod.grant_exact(max_len)?;

        let ser = ser_flavors::Cobs::try_new(ser_flavors::Slice::new(&mut wgr)).map_err(drop)?;
        let used = wire_frames::encode_frame_ty(ser, hdr, key, body).map_err(drop)?;
        let len = used.len();
        wgr.commit(len);

        Ok(())
    }

    pub fn send_raw(
        &mut self,
        hdr: &CommonHeader,
        key: Option<&Key>,
        body: &[u8],
    ) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR.0;

        if is_err {
            // todo: use a different interface for this
            return Err(());
        }

        let max_len = cobs::max_encoding_length(self.mtu as usize);
        let mut wgr = self.prod.grant_exact(max_len)?;

        let ser = ser_flavors::Cobs::try_new(ser_flavors::Slice::new(&mut wgr)).map_err(drop)?;
        let used = wire_frames::encode_frame_raw(ser, hdr, key, body).map_err(drop)?;
        let len = used.len();
        wgr.commit(len);

        Ok(())
    }

    pub fn send_err(&mut self, hdr: &CommonHeader, err: ProtocolError) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR.0;

        // note: here it SHOULD be an err!
        if !is_err {
            // todo: use a different interface for this
            return Err(());
        }

        let max_len = cobs::max_encoding_length(self.mtu as usize);
        let mut wgr = self.prod.grant_exact(max_len)?;

        let ser = ser_flavors::Cobs::try_new(ser_flavors::Slice::new(&mut wgr)).map_err(drop)?;
        let used = wire_frames::encode_frame_err(ser, hdr, err).map_err(drop)?;
        let len = used.len();
        wgr.commit(len);

        Ok(())
    }
}
