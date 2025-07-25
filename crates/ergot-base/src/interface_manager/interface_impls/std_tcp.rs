//! std tcp interface impl
//!
//! std tcp uses COBS for framing over a TCP stream.

use crate::interface_manager::{
    Interface,
    utils::{cobs_stream, std::StdQueue},
};

/// An interface implementation for TCP on std
pub struct StdTcpInterface {}

impl Interface for StdTcpInterface {
    type Sink = cobs_stream::Sink<StdQueue>;
}
