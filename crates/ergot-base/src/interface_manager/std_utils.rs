use crate::{Address, FrameKind, HeaderSeq, Key};

pub(crate) struct OwnedFrame {
    pub(crate) hdr: HeaderSeq,
    pub(crate) body: Vec<u8>,
}

#[derive(Debug, PartialEq)]
pub enum ReceiverError {
    SocketClosed,
}

pub(crate) fn ser_frame(frame: OwnedFrame) -> Vec<u8> {
    let dst_any = frame.hdr.dst.port_id == 0;
    let src = frame.hdr.src.as_u32();
    let dst = frame.hdr.dst.as_u32();
    let seq = frame.hdr.seq_no;

    let mut out = vec![];
    // TODO: This is bad and does a ton of allocs. yolo
    //
    out.extend_from_slice(&postcard::to_stdvec(&src).unwrap());
    out.extend_from_slice(&postcard::to_stdvec(&dst).unwrap());
    out.push(frame.hdr.kind.0);
    out.push(frame.hdr.ttl);
    if dst_any {
        let key = frame.hdr.key.unwrap();
        out.extend_from_slice(&key.0);
    }

    out.extend_from_slice(&postcard::to_stdvec(&seq).unwrap());
    out.extend_from_slice(&frame.body);
    let mut out = cobs::encode_vec(&out);
    out.push(0);
    out
}

pub(crate) fn de_frame(remain: &[u8]) -> Option<OwnedFrame> {
    let (src_word, remain) = postcard::take_from_bytes::<u32>(remain).ok()?;
    let src = Address::from_word(src_word);
    let (dst_word, remain) = postcard::take_from_bytes::<u32>(remain).ok()?;
    let dst = Address::from_word(dst_word);
    let (kind, remain) = remain.split_first()?;
    let kind = FrameKind(*kind);
    let (ttl, remain) = remain.split_first()?;
    let ttl = *ttl;
    let (key, remain) = if dst.port_id == 0 {
        if remain.len() < 8 {
            return None;
        }
        let (keyb, remain) = remain.split_at(8);
        let mut buf = [0u8; 8];
        buf.copy_from_slice(keyb);
        (Some(Key(buf)), remain)
    } else {
        (None, remain)
    };

    let (seq, remain) = postcard::take_from_bytes::<u16>(remain).ok()?;
    let body = remain.to_vec();

    Some(OwnedFrame {
        hdr: HeaderSeq {
            src,
            dst,
            seq_no: seq,
            key,
            kind,
            ttl,
        },
        body,
    })
}

pub(crate) mod acc {
    //! Basically postcard's cobs accumulator, but without the deser part

    pub struct CobsAccumulator {
        buf: Box<[u8]>,
        idx: usize,
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

    impl CobsAccumulator {
        /// Create a new accumulator.
        pub fn new(sz: usize) -> Self {
            CobsAccumulator {
                buf: vec![0u8; sz].into_boxed_slice(),
                idx: 0,
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
            let max_len = self.buf.len();

            if let Some(n) = zero_pos {
                // Yes! We have an end of message here.
                // Add one to include the zero in the "take" portion
                // of the buffer, rather than in "release".
                let (take, release) = input.split_at(n + 1);

                // TODO(AJM): We could special case when idx == 0 to avoid copying
                // into the dest buffer if there's a whole packet in the input

                // Does it fit?
                if (self.idx + take.len()) <= max_len {
                    // Aw yiss - add to array
                    self.extend_unchecked(take);

                    let retval = match cobs::decode_in_place(&mut self.buf[..self.idx]) {
                        Ok(ct) => FeedResult::Success {
                            data: &self.buf[..ct],
                            remaining: release,
                        },
                        Err(_) => FeedResult::DeserError(release),
                    };
                    self.idx = 0;
                    retval
                } else {
                    self.idx = 0;
                    FeedResult::OverFull(release)
                }
            } else {
                // Does it fit?
                if (self.idx + input.len()) > max_len {
                    // nope
                    let new_start = max_len - self.idx;
                    self.idx = 0;
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
            let new_end = self.idx + input.len();
            self.buf[self.idx..new_end].copy_from_slice(input);
            self.idx = new_end;
        }
    }
}
