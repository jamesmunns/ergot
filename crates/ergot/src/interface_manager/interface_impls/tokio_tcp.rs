//! std tcp interface impl
//!
//! std tcp uses COBS for framing over a TCP stream.

use crate::interface_manager::{
    utils::{cobs_stream, std::StdQueue},
    Interface,
};

/// An interface implementation for TCP using tokio
pub struct TokioTcpInterface {}

impl Interface for TokioTcpInterface {
    type Sink = cobs_stream::Sink<StdQueue>;
}
