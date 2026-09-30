//! CAN interface (classic CAN and CAN FD).
//!
//! The [`Interface`] marker for a CAN segment. The sink is
//! [`utils::can::Sink`], the worker is [`transports::can::CanRxTxWorker`],
//! and the two outgoing queues are ordinary framed bbqueues of whole ergot
//! frames — one for Control traffic, one for everything else.
//!
//! See `notes/2026-09-28-can-transport.md`.

use core::marker::PhantomData;

use bbqueue::traits::bbqhdl::BbqHandle;

use crate::interface_manager::{Interface, utils};

#[allow(unused_imports)]
use crate::interface_manager::transports;

/// A CAN segment interface. `Q` is the queue handle type of the two outgoing
/// frame queues, `MTU` the largest ergot frame carried (the reassembly
/// buffer size on the receiving side).
pub struct CanInterface<Q: BbqHandle, const MTU: usize> {
    _pd: PhantomData<Q>,
}

impl<Q: BbqHandle + 'static, const MTU: usize> Interface for CanInterface<Q, MTU> {
    type Sink = utils::can::Sink<Q, MTU>;
}
