#![cfg_attr(not(any(test, feature = "std")), no_std)]

use core::ops::DerefMut;

/// The call to `push_reset` failed due to overflow
struct Overflow;

/// Basically postcard's cobs accumulator, but without the deser part
pub struct CobsAccumulator<B: DerefMut<Target = [u8]>> {
    buf: B,
    idx: usize,
    in_overflow: bool,
}

/// The result of feeding the accumulator.
pub enum FeedResult<'input, 'buf> {
    /// Consumed all data, still pending.
    Consumed,

    /// Buffer was filled. Contains remaining section of input, if any.
    OverFull(&'input mut [u8]),

    /// Reached end of chunk, but cobs decode failed. Contains remaining
    /// section of input, if any.
    DecodeError(&'input mut [u8]),

    /// We decoded a message successfully. The data is currently
    /// stored in our storage buffer.
    Success {
        /// Decoded data.
        data: &'buf [u8],

        /// Remaining data left in the buffer after deserializing.
        remaining: &'input mut [u8],
    },

    /// We decoded a message successfully. The data is currently
    /// stored in the passed-in input buffer
    SuccessInput {
        /// Decoded data.
        data: &'input [u8],

        /// Remaining data left in the buffer after deserializing.
        remaining: &'input mut [u8],
    },
}

#[cfg(any(feature = "std", test))]
impl CobsAccumulator<Box<[u8]>> {
    pub fn new_boxslice(len: usize) -> Self {
        Self::new(vec![0u8; len].into_boxed_slice())
    }
}

impl<B: DerefMut<Target = [u8]>> CobsAccumulator<B> {
    /// Create a new accumulator.
    pub fn new(b: B) -> Self {
        CobsAccumulator {
            buf: b,
            idx: 0,
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
        // No input? No work!
        if input.is_empty() {
            return FeedResult::Consumed;
        }

        // Can we find any zeroes in the whole input?
        let zero_pos = input.iter().position(|&i| i == 0);
        let Some(n) = zero_pos else {
            // No zero in this entire input.
            //
            // Are we currently overflowing?
            if self.in_overflow {
                // Yes: overflowing, and no zero to rescue us. Consume the whole
                // input, remain in overflow.
                return FeedResult::OverFull(&mut []);
            }

            // Not overflowing, Does the input fit?
            return match self.push(input) {
                // We ate the whole input, and no zero, so we're done here.
                Ok(()) => FeedResult::Consumed,
                // If there's NO zero in this input, and we JUST entered the overflow
                // state, then we're going to consume the entire input, no point in
                // giving partial data back to the caller.
                Err(Overflow) => {
                    self.in_overflow = true;
                    FeedResult::OverFull(&mut [])
                }
            };
        };

        // Yes! We have an end of message here.
        // Add one to include the zero in the "take" portion
        // of the buffer, rather than in "release".
        let (take, release) = input.split_at_mut(n + 1);

        // If we got a zero, this frees us from the overflow condition,
        // don't attempt to decode, we've already lost some part of this
        // message.
        if self.in_overflow {
            self.in_overflow = false;
            return FeedResult::OverFull(release);
        }

        // If there's no data in the buffer, then we don't need to copy it in,
        // just decode directly in the input buffer without doing an extra
        // memcpy
        if self.idx == 0 {
            return match cobs::decode_in_place(take) {
                Ok(ct) => FeedResult::SuccessInput {
                    data: &take[..ct],
                    remaining: release,
                },
                Err(_) => FeedResult::DecodeError(release),
            };
        }

        // Does it fit? This will give us a view of the buffer, but reset the
        // count, so the next call will see an empty buffer.
        let Ok(used) = self.push_reset(take) else {
            // If we overflowed, tell the caller. DON'T mark ourselves as
            // in-overflow, because we DID get a zero, which clears the
            // state, we just lost the current message. We are ready to
            // start again with `release` on the next call.
            return FeedResult::OverFull(release);
        };

        // Finally: attempt to de-cobs the contents of our storage buffer.
        match cobs::decode_in_place(used) {
            // It worked! Tell the caller it went great
            Ok(ct) => FeedResult::Success {
                data: &used[..ct],
                remaining: release,
            },
            // It did NOT work, tell the caller
            Err(_) => FeedResult::DecodeError(release),
        }
    }

    #[inline]
    fn push(&mut self, data: &[u8]) -> Result<(), Overflow> {
        let old_idx = self.idx;
        let new_end = old_idx + data.len();
        if let Some(sli) = self.buf.get_mut(old_idx..new_end) {
            sli.copy_from_slice(data);
            self.idx = self.buf.len().min(new_end);
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
        let res = if let Some(sli) = self.buf.get_mut(old_idx..new_end) {
            sli.copy_from_slice(data);
            Ok(sli)
        } else {
            Err(Overflow)
        };
        self.idx = 0;
        res
    }
}
