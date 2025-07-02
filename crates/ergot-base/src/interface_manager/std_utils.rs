use postcard::ser_flavors::{Cobs, StdVec};

use crate::{Address, FrameKind, HeaderSeq, ProtocolError};

use super::wire_frames::{self, CommonHeader};

pub(crate) struct OwnedFrame {
    pub(crate) hdr: HeaderSeq,
    pub(crate) body: Result<Vec<u8>, ProtocolError>,
}

#[derive(Debug, PartialEq)]
pub enum ReceiverError {
    SocketClosed,
}

pub(crate) fn ser_frame(frame: OwnedFrame) -> Vec<u8> {
    let dst_any = [0, 255].contains(&frame.hdr.dst.port_id);
    let is_err = frame.hdr.kind == FrameKind::PROTOCOL_ERROR;
    let chdr = CommonHeader {
        src: frame.hdr.src.as_u32(),
        dst: frame.hdr.dst.as_u32(),
        seq_no: frame.hdr.seq_no,
        kind: frame.hdr.kind.0,
        ttl: frame.hdr.ttl,
    };

    let out = Cobs::try_new(StdVec::new()).unwrap();

    let key = if dst_any {
        let k = frame.hdr.key.as_ref();
        assert!(k.is_some());
        k
    } else {
        None
    };

    match frame.body {
        Ok(body) => {
            assert!(!is_err);


            wire_frames::encode_frame_raw(out, &chdr, key, body.as_slice())
        }
        Err(perr) => {
            assert!(is_err && !dst_any);
            wire_frames::encode_frame_err(out, &chdr, perr)
        }
    }
    .unwrap()
}

pub(crate) fn de_frame(remain: &[u8]) -> Option<OwnedFrame> {
    let res = wire_frames::decode_frame_partial(remain)?;

    let key;
    let body = match res.tail {
        wire_frames::PartialDecodeTail::Specific(body) => {
            key = None;
            Ok(body.to_vec())
        }
        wire_frames::PartialDecodeTail::AnyAll { key: skey, body } => {
            key = Some(skey);
            Ok(body.to_vec())
        }
        wire_frames::PartialDecodeTail::Err(protocol_error) => {
            key = None;
            Err(protocol_error)
        }
    };

    let CommonHeader {
        src,
        dst,
        seq_no,
        kind,
        ttl,
    } = res.hdr;

    Some(OwnedFrame {
        hdr: HeaderSeq {
            src: Address::from_word(src),
            dst: Address::from_word(dst),
            seq_no,
            key,
            kind: FrameKind(kind),
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
