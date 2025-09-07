//! std tcp interface impl
//!
//! std tcp uses COBS for framing over a MPSC queues

use crate::interface_manager::{
    Interface,
    utils::{framed_stream, std::StdQueue},
};

/// An interface implementation for MPSC channel using tokio
pub struct TokioMpscInterface {}

impl Interface for TokioMpscInterface {
    type Sink = framed_stream::Sink<StdQueue>;
}
