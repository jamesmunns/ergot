use crate::interface_manager::{
    Interface,
    utils::{cobs_stream, std::StdQueue},
};

pub struct StdTcpInterface {}

impl Interface for StdTcpInterface {
    type Sink = cobs_stream::Sink<StdQueue>;
}
