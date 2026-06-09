use std::sync::Arc;

use bbqueue::{
    BBQueue,
    traits::{coordination::cas::AtomicCoord, notifier::maitake::MaiNotSpsc, storage::BoxedSlice},
};

#[derive(Debug, PartialEq)]
pub enum ReceiverError {
    SocketClosed,
}

/// A type alias for the kind of queue used on std devices.
pub type StdQueue = Arc<BBQueue<BoxedSlice, AtomicCoord, MaiNotSpsc>>;

/// Create a new StdQueue with the given buffer size
pub fn new_std_queue(buffer: usize) -> StdQueue {
    Arc::new(BBQueue::new_with_storage(BoxedSlice::new(buffer)))
}
