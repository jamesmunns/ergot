//! std udp interface impl

use crate::interface_manager::{
    Interface,
    utils::{framed_stream, std::StdQueue},
};

/// An interface implementation for UDP using tokio
pub struct TokioUdpInterface {}

impl Interface for TokioUdpInterface {
    type Sink = framed_stream::Sink<StdQueue>;
}
