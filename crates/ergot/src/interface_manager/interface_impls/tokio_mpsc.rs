//! std tcp interface impl
//!
//! std tcp uses COBS for framing over a MPSC queues

use crate::interface_manager::{
    Interface,
    utils::{cobs_stream, std::StdQueue},
};

/// An interface implementation for MPSC channel using tokio
pub struct TokioMpscInterface {}

impl Interface for TokioMpscInterface {
    type Sink = cobs_stream::Sink<StdQueue>;
}
