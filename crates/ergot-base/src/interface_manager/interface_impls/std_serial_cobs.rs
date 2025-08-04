//! std serial cobs interface impl
//!
//! std serial cobs uses COBS for framing over a serial stream.

use crate::interface_manager::{
    Interface,
    utils::{cobs_stream, std::StdQueue},
};

/// An interface implementation for serial on std
pub struct StdSerialInterface {}

impl Interface for StdSerialInterface {
    type Sink = cobs_stream::Sink<StdQueue>;
}
