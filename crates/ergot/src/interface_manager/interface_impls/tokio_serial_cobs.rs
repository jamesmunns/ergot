//! std serial cobs interface impl
//!
//! std serial cobs uses COBS for framing over a serial stream.

use crate::interface_manager::{
    utils::{cobs_stream, std::StdQueue},
    Interface,
};

/// An interface implementation for serial using tokio
pub struct TokioSerialInterface {}

impl Interface for TokioSerialInterface {
    type Sink = cobs_stream::Sink<StdQueue>;
}
