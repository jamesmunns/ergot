#![cfg_attr(not(any(test, feature = "std")), no_std)]

/// The call to `push_reset` failed due to overflow
#[derive(Debug)]
pub struct PushResetOverflow;

/// The call to `push` failed due to overflow, and the given number of
/// bytes WOULD have been consumed prior to overflow.
#[derive(Debug)]
pub struct PushOverflow(pub usize);

pub trait Storage {
    /// Push the data into the storage
    ///
    /// Returns an error and resets the idx to zero if data does not fit
    fn push(&mut self, data: &[u8]) -> Result<(), PushOverflow>;
    /// Get a slice `..idx`, then reset the index to zero
    ///
    /// Returns an error and resets the idx to zero if data does not fit
    fn push_reset(&'_ mut self, data: &[u8]) -> Result<&'_ mut [u8], PushResetOverflow>;
}

/// Basically postcard's cobs accumulator, but without the deser part
pub struct CobsAccumulator<S: Storage> {
    buf: S,
}

/// The result of feeding the accumulator.
pub enum FeedResult<'input, 'buf> {
    /// Consumed all data, still pending.
    Consumed,

    /// Buffer was filled. Contains remaining section of input, if any.
    OverFull(&'input [u8]),

    /// Reached end of chunk, but deserialization failed. Contains remaining section of input, if.
    /// any
    DeserError(&'input [u8]),

    Success {
        /// Decoded data.
        data: &'buf [u8],

        /// Remaining data left in the buffer after deserializing.
        remaining: &'input [u8],
    },
}

impl<S: Storage> CobsAccumulator<S> {
    /// Create a new accumulator.
    pub fn new(s: S) -> Self {
        CobsAccumulator { buf: s }
    }

    /// Appends data to the internal buffer and attempts to deserialize the accumulated data into
    /// `T`.
    ///
    /// This differs from feed, as it allows the `T` to reference data within the internal buffer, but
    /// mutably borrows the accumulator for the lifetime of the deserialization.
    /// If `T` does not require the reference, the borrow of `self` ends at the end of the function.
    pub fn feed_raw<'me, 'input>(&'me mut self, input: &'input [u8]) -> FeedResult<'input, 'me> {
        if input.is_empty() {
            return FeedResult::Consumed;
        }

        let zero_pos = input.iter().position(|&i| i == 0);

        if let Some(n) = zero_pos {
            // Yes! We have an end of message here.
            // Add one to include the zero in the "take" portion
            // of the buffer, rather than in "release".
            let (take, release) = input.split_at(n + 1);

            // TODO(AJM): We could special case when idx == 0 to avoid copying
            // into the dest buffer if there's a whole packet in the input

            // Does it fit?
            match self.buf.push_reset(take) {
                Ok(used) => match cobs::decode_in_place(used) {
                    Ok(ct) => FeedResult::Success {
                        data: &used[..ct],
                        remaining: release,
                    },
                    Err(_) => FeedResult::DeserError(release),
                },
                Err(PushResetOverflow) => FeedResult::OverFull(release),
            }
        } else {
            // Does it fit?
            match self.buf.push(input) {
                Ok(()) => FeedResult::Consumed,
                Err(PushOverflow(new_start)) => {
                    FeedResult::OverFull(&input[new_start..])
                }
            }
        }
    }
}

pub struct SliceStorage<'a> {
    data: &'a mut [u8],
    idx: usize,
}

impl<'a> SliceStorage<'a> {
    pub fn new(sli: &'a mut [u8]) -> Self {
        Self { data: sli, idx: 0 }
    }
}

impl<'a> Storage for SliceStorage<'a> {
    #[inline]
    fn push(&mut self, data: &[u8]) -> Result<(), PushOverflow> {
        let old_idx = self.idx;
        let new_end = old_idx + data.len();
        if let Some(sli) = self.data.get_mut(old_idx..new_end) {
            sli.copy_from_slice(data);
            self.idx = self.data.len().min(new_end);
            Ok(())
        } else {
            let would_have_taken = new_end.checked_sub(self.data.len()).unwrap_or(0);
            self.idx = 0;
            Err(PushOverflow(would_have_taken))
        }
    }

    #[inline]
    fn push_reset(&'_ mut self, data: &[u8]) -> Result<&'_ mut [u8], PushResetOverflow> {
        let old_idx = self.idx;
        let new_end = old_idx + data.len();
        let res = if let Some(sli) = self.data.get_mut(old_idx..new_end) {
            sli.copy_from_slice(data);
            Ok(sli)
        } else {
            Err(PushResetOverflow)
        };
        self.idx = 0;
        res
    }
}

#[cfg(any(feature = "std", test))]
pub use use_std::BoxSliceStorage;

#[cfg(any(feature = "std", test))]
mod use_std {
    use super::{PushResetOverflow, PushOverflow, Storage};

    pub struct BoxSliceStorage {
        data: Box<[u8]>,
        idx: usize,
    }

    impl BoxSliceStorage {
        pub fn new(size: usize) -> Self {
            Self {
                data: vec![0u8; size].into_boxed_slice(),
                idx: 0,
            }
        }
    }

    impl Storage for BoxSliceStorage {
        #[inline]
        fn push(&mut self, data: &[u8]) -> Result<(), PushOverflow> {
            let old_idx = self.idx;
            let new_end = old_idx + data.len();
            if let Some(sli) = self.data.get_mut(old_idx..new_end) {
                sli.copy_from_slice(data);
                self.idx = self.data.len().min(new_end);
                Ok(())
            } else {
                let would_have_taken = new_end.checked_sub(self.data.len()).unwrap_or(0);
                self.idx = 0;
                Err(PushOverflow(would_have_taken))
            }
        }

        #[inline]
        fn push_reset(&'_ mut self, data: &[u8]) -> Result<&'_ mut [u8], PushResetOverflow> {
            let old_idx = self.idx;
            let new_end = old_idx + data.len();
            let res = if let Some(sli) = self.data.get_mut(old_idx..new_end) {
                sli.copy_from_slice(data);
                Ok(sli)
            } else {
                Err(PushResetOverflow)
            };
            self.idx = 0;
            res
        }
    }
}
