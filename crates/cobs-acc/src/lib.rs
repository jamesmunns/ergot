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
#[derive(Debug)]
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

        let res = if let Some(sli) = self.buf.get_mut(..new_end) {
            sli[old_idx..].copy_from_slice(data);
            Ok(sli)
        } else {
            Err(Overflow)
        };
        self.idx = 0;
        res
    }

    #[doc(hidden)]
    #[cfg(test)]
    pub fn contents(&self) -> Option<&[u8]> {
        if self.in_overflow {
            None
        } else {
            Some(&self.buf[..self.idx])
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod test {
    use crate::{CobsAccumulator, FeedResult};

    #[test]
    fn smoke() {
        let mut acc = CobsAccumulator::new_boxslice(16);
        let mut input = vec![];
        for i in 0..6 {
            input.push(0);
            input.push(i);
        }
        let mut inenc = cobs::encode_vec(&input);
        inenc.push(0);
        assert_eq!(inenc.len(), 14);
        let inenc = inenc;

        // No matter the stride of the input, we get the expected data out
        for chsz in 1..inenc.len() {
            let mut inenc = inenc.clone();
            let mut got_data = None;
            let mut fed = 0;
            for ch in inenc.chunks_mut(chsz) {
                fed += ch.len();
                match acc.feed_raw(ch) {
                    FeedResult::Consumed => {}
                    FeedResult::Success { data, remaining } => {
                        assert!(remaining.is_empty());
                        got_data = Some(data.to_vec());
                        break;
                    }
                    _ => panic!(),
                }
            }
            assert_eq!(fed, 14);
            let got = got_data.unwrap();
            assert_eq!(got, input);
        }

        // Do it again, but we might have two messages
        let mut twoenc = vec![];
        twoenc.extend_from_slice(&inenc);
        twoenc.extend_from_slice(&inenc);
        let twoenc = twoenc;

        for chsz in 1..twoenc.len() {
            let mut twoenc = twoenc.clone();
            let mut got_data = 0;
            let mut fed = 0;
            for mut ch in twoenc.chunks_mut(chsz) {
                fed += ch.len();
                'feed: loop {
                    match acc.feed_raw(ch) {
                        FeedResult::Consumed => break 'feed,
                        FeedResult::Success { data, remaining } => {
                            assert_eq!(data, &input);
                            got_data += 1;
                            ch = remaining;
                        }
                        FeedResult::SuccessInput { data, remaining } => {
                            assert_eq!(data, &input);
                            got_data += 1;
                            ch = remaining;
                        }
                        e => panic!("{e:?}"),
                    }
                }
            }
            assert_eq!(fed, 28);
            assert_eq!(got_data, 2);
        }
    }

    #[test]
    fn decode_err() {
        let mut acc = CobsAccumulator::new_boxslice(16);
        let mut input = vec![];
        for i in 0..6 {
            input.push(0);
            input.push(i);
        }
        let mut inenc = cobs::encode_vec(&input);
        let mut badenc = inenc.clone();
        inenc.push(0);
        badenc.push(4);
        badenc.push(0);
        assert_eq!(inenc.len(), 14);
        assert_eq!(badenc.len(), 15);
        let inenc = inenc;
        let badenc = badenc;

        // Set up good bad good as the pattern, ensure we get both
        // goods and don't get the bad.

        let mut sandwich = vec![];
        sandwich.extend_from_slice(&inenc);
        sandwich.extend_from_slice(&badenc);
        sandwich.extend_from_slice(&inenc);
        let sandwich = sandwich;

        for chsz in 1..sandwich.len() {
            let mut sandwich = sandwich.clone();
            let mut got_data = 0;
            let mut bad_data = 0;
            let mut fed = 0;
            for mut ch in sandwich.chunks_mut(chsz) {
                fed += ch.len();
                'feed: loop {
                    match acc.feed_raw(ch) {
                        FeedResult::Consumed => break 'feed,
                        FeedResult::Success { data, remaining } => {
                            assert_eq!(data, &input);
                            got_data += 1;
                            ch = remaining;
                        }
                        FeedResult::SuccessInput { data, remaining } => {
                            assert_eq!(data, &input);
                            got_data += 1;
                            ch = remaining;
                        }
                        FeedResult::DecodeError(remaining) => {
                            bad_data += 1;
                            ch = remaining;
                        }
                        e => panic!("{e:?}"),
                    }
                }
            }
            assert_eq!(fed, inenc.len() * 2 + badenc.len());
            assert_eq!(got_data, 2);
            assert_eq!(bad_data, 1);
        }
    }

    #[test]
    fn overflow_err() {
        let mut acc = CobsAccumulator::new_boxslice(16);
        let mut input = vec![];
        for i in 0..6 {
            input.push(0);
            input.push(i);
        }
        let mut inenc = cobs::encode_vec(&input);
        inenc.push(0);

        let mut biginput = vec![];
        for i in 0..25 {
            biginput.push(0);
            biginput.push(i);
        }
        let mut bigenc = cobs::encode_vec(&biginput);
        bigenc.push(0);

        assert_eq!(inenc.len(), 14);
        assert_eq!(bigenc.len(), 52);
        let inenc = inenc;
        let bigenc = bigenc;

        // Set up good bad good as the pattern, ensure we get both
        // goods and don't get the bad.

        let mut sandwich = vec![];
        sandwich.extend_from_slice(&inenc);
        sandwich.extend_from_slice(&bigenc);
        sandwich.extend_from_slice(&inenc);
        let sandwich = sandwich;

        // NOTE: we cap the max chunk size here to the size of the
        // storage buffer, because in cases where we get the ENTIRE message
        // in input, we don't actually overflow!
        for chsz in 1..16 {
            let mut sandwich = sandwich.clone();
            let mut got_data = 0;
            let mut fed = 0;
            for mut ch in sandwich.chunks_mut(chsz) {
                fed += ch.len();
                println!("CH: {ch:?}");
                'feed: loop {
                    println!("{:?} <- {ch:?}", acc.contents());
                    match acc.feed_raw(ch) {
                        FeedResult::Consumed => break 'feed,
                        FeedResult::Success { data, remaining } => {
                            assert_eq!(data, &input);
                            got_data += 1;
                            ch = remaining;
                        }
                        FeedResult::SuccessInput { data, remaining } => {
                            assert_eq!(data, &input);
                            got_data += 1;
                            ch = remaining;
                        }
                        FeedResult::OverFull(remaining) => {
                            ch = remaining;
                        }
                        e => panic!("{e:?}"),
                    }
                }
            }
            assert_eq!(fed, inenc.len() * 2 + bigenc.len());
            assert_eq!(got_data, 2);
        }
    }

    #[test]
    fn permute_256() {
        let mut acc = CobsAccumulator::new_boxslice(16);

        // 00: good message
        let mut input = vec![];
        for i in 0..6 {
            input.push(0);
            input.push(i);
        }
        let mut inenc = cobs::encode_vec(&input);
        inenc.push(0);
        let inenc = inenc;

        // 01: bad decode
        let mut binput = vec![];
        for i in 0..6 {
            binput.push(0);
            binput.push(i);
        }
        let mut badenc = cobs::encode_vec(&binput);
        badenc.push(4);
        badenc.push(0);
        let badenc = badenc;

        // 10: empty good
        let empenc = vec![0u8];

        // 11: overflow
        let mut biginput = vec![];
        for i in 0..25 {
            biginput.push(0);
            biginput.push(i);
        }
        let mut bigenc = cobs::encode_vec(&biginput);
        bigenc.push(0);
        let bigenc = bigenc;

        // Use an 8 bit integer to come up with 256 test cases,
        // each with a stream made up of 4 of the 4 above scenarios
        for mut scenario_byte in 0u8..=255u8 {
            let mut input_stream = vec![];
            let mut good_emptys = 0;
            let mut good_data = 0;
            let mut bad_dec = 0;
            for _ in 0..4 {
                let scen = scenario_byte & 0b11;
                scenario_byte >>= 2;
                match scen {
                    0b00 => {
                        input_stream.extend_from_slice(&inenc);
                        good_data += 1;
                    }
                    0b01 => {
                        input_stream.extend_from_slice(&badenc);
                        bad_dec += 1;
                    }
                    0b10 => {
                        input_stream.extend_from_slice(&empenc);
                        good_emptys += 1;
                    }
                    _ => {
                        input_stream.extend_from_slice(&bigenc);
                    }
                }
            }

            // NOTE: we cap the max chunk size here to one less than the
            // overflow case, because in cases where we get the ENTIRE message
            // in input, we don't actually overflow!
            for chsz in 1..bigenc.len() {
                let mut input_stream = input_stream.clone();
                let mut got_data = 0;
                let mut got_empty = 0;
                let mut got_bads = 0;
                let mut fed = 0;
                for mut ch in input_stream.chunks_mut(chsz) {
                    fed += ch.len();
                    println!("CH: {ch:?}");
                    'feed: loop {
                        println!("{:?} <- {ch:?}", acc.contents());
                        match acc.feed_raw(ch) {
                            FeedResult::Consumed => break 'feed,
                            FeedResult::Success { data, remaining } => {
                                if data.is_empty() {
                                    got_empty += 1;
                                } else {
                                    assert_eq!(data, &input);
                                    got_data += 1;
                                }
                                ch = remaining;
                            }
                            FeedResult::SuccessInput { data, remaining } => {
                                if data.is_empty() {
                                    got_empty += 1;
                                } else {
                                    assert_eq!(data, &input);
                                    got_data += 1;
                                }
                                ch = remaining;
                            }
                            FeedResult::DecodeError(remaining) => {
                                got_bads += 1;
                                ch = remaining;
                            }
                            FeedResult::OverFull(remaining) => {
                                ch = remaining;
                            }
                        }
                    }
                }
                assert_eq!(fed, input_stream.len());
                assert_eq!(got_data, good_data);
                assert_eq!(got_bads, bad_dec);
                assert_eq!(got_empty, good_emptys);
            }
        }
    }
}
