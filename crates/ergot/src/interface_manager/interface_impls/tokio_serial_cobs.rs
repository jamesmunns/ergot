//! std serial cobs interface impl
//!
//! std serial cobs uses COBS for framing over a serial stream.

use std::sync::Arc;

use bbqueue::traits::bbqhdl::BbqHandle;
use maitake_sync::WaitQueue;

use crate::interface_manager::{
    Interface, InterfaceSink,
    utils::{cobs_stream::Sink, std::StdQueue},
};

/// An interface implementation for serial using tokio
pub struct TokioSerialInterface {}

/// This allows making workers stop when the interface is deregistered, through
/// the dropping of the sink.
pub struct WrapperSink<Q: BbqHandle> {
    closer: Arc<WaitQueue>,
    sink: Sink<Q>,
}

impl<Q: BbqHandle> WrapperSink<Q> {
    pub fn new(sink: Sink<Q>, closer: Arc<WaitQueue>) -> Self {
        Self { closer, sink }
    }
}

impl<Q: BbqHandle> Drop for WrapperSink<Q> {
    fn drop(&mut self) {
        self.closer.close();
    }
}

impl<Q: BbqHandle> InterfaceSink for WrapperSink<Q> {
    fn send_ty<T: serde::Serialize>(&mut self, hdr: &crate::HeaderSeq, body: &T) -> Result<(), ()> {
        self.sink.send_ty(hdr, body)
    }
    fn send_raw(&mut self, hdr: &crate::HeaderSeq, body: &[u8]) -> Result<(), ()> {
        self.sink.send_raw(hdr, body)
    }
    fn send_err(&mut self, hdr: &crate::HeaderSeq, err: crate::ProtocolError) -> Result<(), ()> {
        self.sink.send_err(hdr, err)
    }
}

impl Interface for TokioSerialInterface {
    type Sink = WrapperSink<StdQueue>;
}
