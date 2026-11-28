//! Cobs Stream
//!
//! The "Cobs Stream" is one flavor of interface sinks. It is intended for serial-like
//! interfaces that require framing in software.

use bbqueue::{prod_cons::stream::StreamProducer, traits::bbqhdl::BbqHandle};
use postcard::{Serializer, ser_flavors::Flavor};
use serde::Serialize;

use crate::{
    FrameKind, Header, ProtocolError,
    interface_manager::{InterfaceSink, LinkMeta},
    wire_frames::{self, MAX_HDR_ENCODED_SIZE, encode_frame_hdr},
};

pub struct Sink<Q>
where
    Q: BbqHandle,
{
    pub(crate) mtu: u16,
    pub(crate) prod: StreamProducer<Q>,
}

#[allow(clippy::result_unit_err)] // todo
impl<Q> Sink<Q>
where
    Q: BbqHandle,
{
    pub fn new_from_handle(q: Q, mtu: u16) -> Self {
        Self {
            mtu,
            prod: q.stream_producer(),
        }
    }

    pub const fn new(prod: StreamProducer<Q>, mtu: u16) -> Self {
        Self { mtu, prod }
    }
}

#[allow(clippy::result_unit_err)] // todo
impl<Q> InterfaceSink for Sink<Q>
where
    Q: BbqHandle,
{
    fn mtu(&self) -> u16 {
        self.mtu
    }

    fn send_ty<T: Serialize>(
        &mut self,
        _link: &LinkMeta,
        hdr: &Header,
        body: &T,
    ) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR;

        if is_err {
            // todo: use a different interface for this
            return Err(());
        }

        let mtu = usize::from(self.mtu);
        let mut wgr = self.prod.grant_exact(max_encoded_len(mtu)).map_err(drop)?;

        let ser = CobsRuns::try_new(&mut wgr, mtu).map_err(drop)?;
        let used = wire_frames::encode_frame_ty(ser, hdr, body).map_err(drop)?;
        let len = used.len();
        wgr.commit(len);

        Ok(())
    }

    fn send_raw(&mut self, _link: &LinkMeta, hdr: &Header, body: &[u8]) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR;

        if is_err {
            // todo: use a different interface for this
            return Err(());
        }
        // The header is `MAX_HDR_ENCODED_SIZE` bytes at most.
        let max_len = max_encoded_len(MAX_HDR_ENCODED_SIZE + body.len());
        let mut wgr = self.prod.grant_exact(max_len).map_err(drop)?;

        let mut ser = Serializer {
            output: CobsRuns::try_new(&mut wgr, usize::from(self.mtu)).map_err(drop)?,
        };
        encode_frame_hdr(&mut ser, hdr).map_err(drop)?;
        ser.output.try_extend(body).map_err(drop)?;
        let fin = ser.output.finalize().map_err(drop)?;
        let len = fin.len();
        wgr.commit(len);

        Ok(())
    }

    fn send_err(&mut self, _link: &LinkMeta, hdr: &Header, err: ProtocolError) -> Result<(), ()> {
        let is_err = hdr.kind == FrameKind::PROTOCOL_ERROR;

        // note: here it SHOULD be an err!
        if !is_err {
            // todo: use a different interface for this
            return Err(());
        }

        let mtu = usize::from(self.mtu);
        let mut wgr = self.prod.grant_exact(max_encoded_len(mtu)).map_err(drop)?;

        let ser = CobsRuns::try_new(&mut wgr, mtu).map_err(drop)?;
        let used = wire_frames::encode_frame_err(ser, hdr, err).map_err(drop)?;
        let len = used.len();
        wgr.commit(len);

        Ok(())
    }
}

/// The most bytes a frame of `frame_len` bytes takes on the stream: a code
/// byte before the frame, one more after every 254 bytes of it without a
/// zero, and the delimiter that ends it. The sink reserves this much to queue
/// a frame, and the other end needs as much room to receive one of its MTU.
///
/// When `frame_len` is a multiple of 254 this is a byte more than
/// `cobs::max_encoding_length(frame_len)` and the delimiter. The encoder
/// begins a block after a full one even at the end of the frame, as
/// postcard's does and the encoder of `cobs` does not.
pub const fn max_encoded_len(frame_len: usize) -> usize {
    frame_len + frame_len / (FULL_BLOCK - 1) + 2
}

/// A postcard flavor that COBS-encodes into a slice and ends the frame with
/// its delimiter.
///
/// It writes the bytes postcard's `Cobs<Slice>` writes. That flavor encodes a
/// byte at a time; this one copies each run of non-zero bytes at once in
/// [`Flavor::try_extend`], which is how byte bodies and raw frames arrive.
///
/// It also counts the frame. How many bytes a frame takes once encoded
/// depends on where its zeros are, so room for the longest encoding of a
/// frame of the MTU also holds some longer frames.
struct CobsRuns<'a> {
    out: &'a mut [u8],
    /// Where the code byte of the block being filled is.
    code: usize,
    /// Where the next byte goes.
    at: usize,
    /// How many more bytes the frame may have.
    left: usize,
}

/// The value of a code byte whose block is full: 254 bytes follow it.
const FULL_BLOCK: usize = 0xFF;

impl<'a> CobsRuns<'a> {
    /// Encodes a frame of at most `max_frame_len` bytes into `out`. Fails
    /// when `out` has no room for the first code byte.
    fn try_new(out: &'a mut [u8], max_frame_len: usize) -> postcard::Result<Self> {
        let mut this = Self {
            out,
            code: 0,
            at: 0,
            left: max_frame_len,
        };
        this.put(0)?;
        Ok(this)
    }

    /// Counts `len` more bytes of the frame. Fails when that makes it longer
    /// than it may be.
    fn count(&mut self, len: usize) -> postcard::Result<()> {
        self.left = self
            .left
            .checked_sub(len)
            .ok_or(postcard::Error::SerializeBufferFull)?;
        Ok(())
    }

    fn put(&mut self, byte: u8) -> postcard::Result<()> {
        *self
            .out
            .get_mut(self.at)
            .ok_or(postcard::Error::SerializeBufferFull)? = byte;
        self.at += 1;
        Ok(())
    }

    /// Ends the block being filled, at a zero or because it is full, and
    /// leaves room for the code byte of the next.
    fn next_block(&mut self) -> postcard::Result<()> {
        self.out[self.code] = (self.at - self.code) as u8;
        self.code = self.at;
        self.put(0)
    }
}

impl<'a> Flavor for CobsRuns<'a> {
    type Output = &'a mut [u8];

    #[inline]
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.count(1)?;
        if byte == 0 {
            return self.next_block();
        }
        self.put(byte)?;
        if self.at - self.code == FULL_BLOCK {
            self.next_block()?;
        }
        Ok(())
    }

    fn try_extend(&mut self, mut data: &[u8]) -> postcard::Result<()> {
        self.count(data.len())?;
        while !data.is_empty() {
            // A block is never left full, so it takes at least one byte.
            let limit = data.len().min(FULL_BLOCK - (self.at - self.code));
            let run = data[..limit]
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(limit);
            self.out
                .get_mut(self.at..self.at + run)
                .ok_or(postcard::Error::SerializeBufferFull)?
                .copy_from_slice(&data[..run]);
            self.at += run;
            if run < limit {
                // The zero is not stored: the next code byte stands for it.
                self.next_block()?;
                data = &data[run + 1..];
            } else {
                if self.at - self.code == FULL_BLOCK {
                    self.next_block()?;
                }
                data = &data[run..];
            }
        }
        Ok(())
    }

    fn finalize(mut self) -> postcard::Result<Self::Output> {
        self.out[self.code] = (self.at - self.code) as u8;
        self.put(0)?;
        Ok(&mut self.out[..self.at])
    }
}

#[cfg(test)]
mod tests {
    use postcard::ser_flavors::{Cobs, Flavor, Slice};

    use super::{CobsRuns, max_encoded_len};

    /// One step of filling a flavor: bytes pushed singly or given at once.
    #[derive(Clone, Copy)]
    enum Step<'a> {
        Push(&'a [u8]),
        Extend(&'a [u8]),
    }

    fn fill<F: Flavor>(mut flavor: F, steps: &[Step<'_>]) -> postcard::Result<F::Output> {
        for step in steps {
            match *step {
                Step::Push(bytes) => bytes.iter().try_for_each(|&byte| flavor.try_push(byte))?,
                Step::Extend(bytes) => flavor.try_extend(bytes)?,
            }
        }
        flavor.finalize()
    }

    /// The frame, or `None` when it does not fit in `room` bytes.
    fn runs(steps: &[Step<'_>], room: usize) -> Option<Vec<u8>> {
        limited(steps, room, usize::MAX)
    }

    /// The frame, or `None` when it does not fit in `room` bytes or is
    /// longer than `max_frame_len` before it is encoded.
    fn limited(steps: &[Step<'_>], room: usize, max_frame_len: usize) -> Option<Vec<u8>> {
        let mut out = vec![0xAA; room];
        let frame =
            CobsRuns::try_new(&mut out, max_frame_len).and_then(|flavor| fill(flavor, steps));
        frame.ok().map(|frame| frame.to_vec())
    }

    /// What postcard's flavor does with the same steps.
    fn reference(steps: &[Step<'_>], room: usize) -> Option<Vec<u8>> {
        let mut out = vec![0x55; room];
        let frame = Cobs::try_new(Slice::new(&mut out)).and_then(|flavor| fill(flavor, steps));
        frame.ok().map(|frame| frame.to_vec())
    }

    /// Checks the frame against postcard's with room to spare, with exactly
    /// enough room, and with every smaller room up to 300 bytes short.
    fn check(steps: &[Step<'_>]) {
        let len: usize = steps
            .iter()
            .map(|step| match step {
                Step::Push(bytes) | Step::Extend(bytes) => bytes.len(),
            })
            .sum();
        let expected = reference(steps, len + len / 254 + 3).expect("the reference fits");
        assert_eq!(runs(steps, len + len / 254 + 3).as_ref(), Some(&expected));
        assert_eq!(runs(steps, expected.len()).as_ref(), Some(&expected));
        for room in expected.len().saturating_sub(300)..expected.len() {
            assert_eq!(runs(steps, room), None, "{len} bytes in {room}");
            assert_eq!(reference(steps, room), None);
        }
    }

    /// Bytes that are zero about once in `zero_every`; never, when it is 0.
    fn bytes(len: usize, zero_every: u32, seed: &mut u32) -> Vec<u8> {
        (0..len)
            .map(|_| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let value = *seed >> 16;
                if zero_every != 0 && value % zero_every == 0 {
                    0
                } else {
                    (value >> 8) as u8 | 1
                }
            })
            .collect()
    }

    #[test]
    #[cfg_attr(miri, ignore = "thousands of frames")]
    fn runs_without_zeros_of_every_length_around_a_block() {
        let data = [0x11; 1100];
        for len in (0..=260)
            .chain(500..=520)
            .chain(760..=770)
            .chain([1016, 1017, 1100])
        {
            check(&[Step::Extend(&data[..len])]);
            check(&[Step::Push(&data[..len])]);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "thousands of frames")]
    fn a_zero_at_every_place_around_a_block() {
        for len in [1, 2, 253, 254, 255, 256, 508, 509, 510] {
            for zero in 0..len {
                let mut data = vec![0x22; len];
                data[zero] = 0;
                check(&[Step::Extend(&data)]);
            }
        }
        check(&[Step::Extend(&[0; 600])]);
        check(&[Step::Extend(&[])]);
        check(&[]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "thousands of frames")]
    fn a_header_pushed_and_a_body_extended() {
        let mut seed = 7;
        for header in [0, 1, 5, 17, 253, 254, 255] {
            for body in [0, 1, 200, 254, 255, 600, 1830] {
                for zero_every in [0, 2, 16, 256] {
                    let head = bytes(header, zero_every, &mut seed);
                    let tail = bytes(body, zero_every, &mut seed);
                    check(&[Step::Push(&head), Step::Extend(&tail)]);
                }
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "thousands of frames")]
    fn pieces_of_any_size_in_any_order() {
        let mut seed = 99;
        for round in 0..100 {
            let zero_every = [0, 2, 8, 64, 256][round % 5];
            let data = bytes(3000, zero_every, &mut seed);
            let mut steps = Vec::new();
            let mut rest = &data[..];
            while !rest.is_empty() {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let take = ((seed >> 16) as usize % 700).min(rest.len());
                let (piece, after) = rest.split_at(take);
                steps.push(if seed & 0x100 == 0 {
                    Step::Extend(piece)
                } else {
                    Step::Push(piece)
                });
                rest = after;
            }
            check(&steps);
        }
    }

    #[test]
    fn a_frame_decodes_to_what_went_in() {
        let mut seed = 3;
        let data = bytes(1500, 16, &mut seed);
        let frame = runs(&[Step::Extend(&data)], 1600).expect("the frame fits");
        assert_eq!(frame.iter().filter(|&&byte| byte == 0).count(), 1);
        let mut decoded = vec![0; data.len()];
        let len = cobs::decode(&frame[..frame.len() - 1], &mut decoded).expect("valid COBS");
        assert_eq!(&decoded[..len], &data[..]);
    }

    /// A frame without a zero takes `max_encoded_len` to the byte, and one
    /// with zeros no more.
    #[test]
    #[cfg_attr(miri, ignore = "thousands of frames")]
    fn the_longest_encoding_is_of_a_frame_without_a_zero() {
        let data = [0x33; 1100];
        for len in 0..=data.len() {
            let steps = [Step::Extend(&data[..len])];
            let room = max_encoded_len(len);
            let frame = runs(&steps, room).map(|frame| frame.len());
            assert_eq!(frame, Some(room), "{len} bytes");
            assert_eq!(runs(&steps, room - 1), None, "{len} bytes");
        }
        let mut seed = 5;
        for len in [1, 253, 254, 255, 508, 509, 1016, 1100] {
            for zero_every in [2, 16, 256] {
                let data = bytes(len, zero_every, &mut seed);
                let room = max_encoded_len(len);
                assert!(runs(&[Step::Extend(&data)], room).is_some(), "{len} bytes");
            }
        }
    }

    /// A frame longer than it may be fails by its own length, with room to
    /// spare and however its bytes arrive; one no longer does not.
    #[test]
    fn a_frame_longer_than_it_may_be_fails() {
        let most: &[usize] = if cfg!(miri) {
            &[0, 2, 254]
        } else {
            &[0, 1, 2, 253, 254, 255, 512]
        };
        for &max in most {
            for len in max.saturating_sub(2)..=max + 2 {
                for fill in [0x00, 0x44] {
                    let data = vec![fill; len];
                    let (head, tail) = data.split_at(len / 2);
                    for steps in [
                        [Step::Push(&data), Step::Push(&[])],
                        [Step::Extend(&data), Step::Extend(&[])],
                        [Step::Push(head), Step::Extend(tail)],
                        [Step::Extend(head), Step::Push(tail)],
                    ] {
                        let frame = limited(&steps, max_encoded_len(len) + 8, max);
                        assert_eq!(
                            frame.is_some(),
                            len <= max,
                            "{len} bytes of {fill:#04x}, of at most {max}"
                        );
                    }
                }
            }
        }
    }
}
