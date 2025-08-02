use std::sync::Arc;

use bbq2::{
    queue::BBQueue,
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

pub(crate) mod acc {
    //! Basically postcard's cobs accumulator, but without the deser part

    pub use cobs_acc::FeedResult;

    pub struct CobsAccumulator {
        inner: cobs_acc::CobsAccumulator<Box<[u8]>>,
    }

    impl CobsAccumulator {
        #[inline]
        pub fn new(size: usize) -> Self {
            Self {
                inner: cobs_acc::CobsAccumulator::new_boxslice(size),
            }
        }

        #[inline(always)]
        pub fn feed_raw<'me, 'input>(
            &'me mut self,
            input: &'input mut [u8],
        ) -> FeedResult<'input, 'me> {
            self.inner.feed_raw(input)
        }
    }
}
