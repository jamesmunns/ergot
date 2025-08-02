#![cfg_attr(not(any(test, feature = "std")), no_std)]

/// The call to `push_reset` failed due to overflow
#[derive(Debug)]
pub struct Overflow;

pub trait Storage {
    /// Is the storage currently empty, e.g. `idx == 0`?
    fn is_empty(&self) -> bool;
    /// Push the data into the storage
    ///
    /// Returns an error and resets the idx to zero if data does not fit
    fn push(&mut self, data: &[u8]) -> Result<(), Overflow>;
    /// Get a slice `..idx`, then reset the index to zero
    ///
    /// Returns an error and resets the idx to zero if data does not fit
    fn push_reset(&'_ mut self, data: &[u8]) -> Result<&'_ mut [u8], Overflow>;
}

/// Basically postcard's cobs accumulator, but without the deser part
pub struct CobsAccumulator<S: Storage> {
    buf: S,
    in_overflow: bool,
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

    SuccessInput {
        /// Decoded data.
        data: &'input [u8],

        /// Remaining data left in the buffer after deserializing.
        remaining: &'input [u8],
    },

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
        CobsAccumulator {
            buf: s,
            in_overflow: false,
        }
    }

    /// Appends data to the internal buffer and attempts to deserialize the accumulated data into
    /// `T`.
    ///
    /// This differs from feed, as it allows the `T` to reference data within the internal buffer, but
    /// mutably borrows the accumulator for the lifetime of the deserialization.
    /// If `T` does not require the reference, the borrow of `self` ends at the end of the function.
    pub fn feed_raw<'me, 'input>(
        &'me mut self,
        input: &'input mut [u8],
    ) -> FeedResult<'input, 'me> {
        if input.is_empty() {
            return FeedResult::Consumed;
        }

        let zero_pos = input.iter().position(|&i| i == 0);

        if let Some(n) = zero_pos {
            // Yes! We have an end of message here.
            // Add one to include the zero in the "take" portion
            // of the buffer, rather than in "release".
            let (take, release) = input.split_at_mut(n + 1);

            // If we got a zero, this frees us from the overflow condition
            if self.in_overflow {
                self.in_overflow = false;
                return FeedResult::OverFull(release);
            }

            // If there's no data in the buffer, then we don't need to copy it in
            if self.buf.is_empty() {
                match cobs::decode_in_place(take) {
                    Ok(ct) => FeedResult::SuccessInput {
                        data: &take[..ct],
                        remaining: release,
                    },
                    Err(_) => FeedResult::DeserError(release),
                }
            } else {
                // Does it fit?
                match self.buf.push_reset(take) {
                    Ok(used) => match cobs::decode_in_place(used) {
                        Ok(ct) => FeedResult::Success {
                            data: &used[..ct],
                            remaining: release,
                        },
                        Err(_) => FeedResult::DeserError(release),
                    },
                    Err(Overflow) => {
                        self.in_overflow = true;
                        FeedResult::OverFull(release)
                    }
                }
            }
        } else {
            // No zero, we're still overflowing
            if self.in_overflow {
                return FeedResult::OverFull(&[]);
            }

            // Does it fit?
            match self.buf.push(input) {
                Ok(()) => FeedResult::Consumed,
                Err(Overflow) => {
                    // If there's NO zero in this input, and we JUST entered the overflow
                    // state, then we're going to consume the entire input, no point in
                    // giving partial data back to the caller.
                    self.in_overflow = true;
                    FeedResult::OverFull(&[])
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
    fn push(&mut self, data: &[u8]) -> Result<(), Overflow> {
        let old_idx = self.idx;
        let new_end = old_idx + data.len();
        if let Some(sli) = self.data.get_mut(old_idx..new_end) {
            sli.copy_from_slice(data);
            self.idx = self.data.len().min(new_end);
            Ok(())
        } else {
            self.idx = 0;
            Err(Overflow)
        }
    }

    #[inline]
    fn push_reset(&'_ mut self, data: &[u8]) -> Result<&'_ mut [u8], Overflow> {
        let old_idx = self.idx;
        let new_end = old_idx + data.len();
        let res = if let Some(sli) = self.data.get_mut(old_idx..new_end) {
            sli.copy_from_slice(data);
            Ok(sli)
        } else {
            Err(Overflow)
        };
        self.idx = 0;
        res
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.idx == 0
    }
}

#[cfg(any(feature = "std", test))]
pub use use_std::BoxSliceStorage;

#[cfg(any(feature = "std", test))]
mod use_std {
    use super::{Overflow, Storage};

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
        fn push(&mut self, data: &[u8]) -> Result<(), Overflow> {
            let old_idx = self.idx;
            let new_end = old_idx + data.len();
            if let Some(sli) = self.data.get_mut(old_idx..new_end) {
                sli.copy_from_slice(data);
                self.idx = self.data.len().min(new_end);
                Ok(())
            } else {
                self.idx = 0;
                Err(Overflow)
            }
        }

        #[inline]
        fn push_reset(&'_ mut self, data: &[u8]) -> Result<&'_ mut [u8], Overflow> {
            let old_idx = self.idx;
            let new_end = old_idx + data.len();
            let res = if let Some(sli) = self.data.get_mut(old_idx..new_end) {
                sli.copy_from_slice(data);
                Ok(sli)
            } else {
                Err(Overflow)
            };
            self.idx = 0;
            res
        }

        #[inline]
        fn is_empty(&self) -> bool {
            self.idx == 0
        }
    }
}
