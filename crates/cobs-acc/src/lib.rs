#![cfg_attr(not(any(test, feature = "std")), no_std)]

pub trait Storage {
    /// Get a mutable view of the entire storage
    fn storage_mut(&mut self) -> &mut [u8];
    /// Get the current index
    fn idx(&self) -> usize;
    /// Set the current index, clamped to the max size
    fn set_idx(&mut self, idx: usize);
    /// The max size of the storage
    ///
    /// This value must NEVER change.
    fn capacity(&self) -> usize;
    /// Get a slice `..idx`, then reset the index to zero
    fn slice_reset(&mut self, idx: usize) -> &[u8];
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
        CobsAccumulator {
            buf: s,
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
        input: &'input [u8],
    ) -> FeedResult<'input, 'me> {
        if input.is_empty() {
            return FeedResult::Consumed;
        }

        let zero_pos = input.iter().position(|&i| i == 0);
        let max_len = self.buf.capacity();

        if let Some(n) = zero_pos {
            // Yes! We have an end of message here.
            // Add one to include the zero in the "take" portion
            // of the buffer, rather than in "release".
            let (take, release) = input.split_at(n + 1);

            // TODO(AJM): We could special case when idx == 0 to avoid copying
            // into the dest buffer if there's a whole packet in the input

            // Does it fit?
            let old_idx = self.buf.idx();
            if (old_idx + take.len()) <= max_len {
                // Aw yiss - add to array
                self.extend_unchecked(take);

                let retval = match cobs::decode_in_place(&mut self.buf.storage_mut()[..old_idx]) {
                    Ok(ct) => FeedResult::Success {
                        data: self.buf.slice_reset(ct),
                        remaining: release,
                    },
                    Err(_) => FeedResult::DeserError(release),
                };
                retval
            } else {
                self.buf.set_idx(0);
                FeedResult::OverFull(release)
            }
        } else {
            // Does it fit?
            if (self.buf.idx() + input.len()) > max_len {
                // nope
                let new_start = max_len - self.buf.idx();
                self.buf.set_idx(0);
                FeedResult::OverFull(&input[new_start..])
            } else {
                // yup!
                self.extend_unchecked(input);
                FeedResult::Consumed
            }
        }
    }

    /// Extend the internal buffer with the given input.
    ///
    /// # Panics
    ///
    /// Will panic if the input does not fit in the internal buffer.
    fn extend_unchecked(&mut self, input: &[u8]) {
        let old_idx = self.buf.idx();
        let new_end = old_idx + input.len();
        self.buf.storage_mut()[old_idx..new_end].copy_from_slice(input);
        self.buf.set_idx(new_end);
    }
}

pub struct SliceStorage<'a> {
    data: &'a mut [u8],
    idx: usize,
}

impl<'a> SliceStorage<'a> {
    pub fn new(sli: &'a mut [u8]) -> Self {
        Self {
            data: sli,
            idx: 0,
        }
    }
}


impl<'a> Storage for SliceStorage<'a> {
    #[inline]
    fn storage_mut(&mut self) -> &mut [u8] {
        self.data
    }

    #[inline]
    fn idx(&self) -> usize {
        self.idx
    }

    #[inline]
    fn set_idx(&mut self, idx: usize) {
        self.idx = self.data.len().min(idx)
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.data.len()
    }

    #[inline]
    fn slice_reset(&mut self, idx: usize) -> &[u8] {
        let sli = &self.data[..idx];
        self.idx = 0;
        sli
    }
}

#[cfg(any(feature = "std", test))]
pub use use_std::BoxSliceStorage;

#[cfg(any(feature = "std", test))]
mod use_std {
    use super::Storage;

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
        fn storage_mut(&mut self) -> &mut [u8] {
            &mut self.data
        }

        #[inline]
        fn idx(&self) -> usize {
            self.idx
        }

        #[inline]
        fn set_idx(&mut self, idx: usize) {
            self.idx = self.data.len().min(idx)
        }

        #[inline]
        fn capacity(&self) -> usize {
            self.data.len()
        }

        #[inline]
        fn slice_reset(&mut self, idx: usize) -> &[u8] {
            let sli = &self.data[..idx];
            self.idx = 0;
            sli
        }
    }
}
