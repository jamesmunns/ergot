//! CAN link layer: ID encoding, fragmentation, reassembly and the sink.
//!
//! One implementation serves classic CAN (8-byte frames) and CAN FD (up to
//! 64-byte frames). The design is written up in
//! `notes/2026-09-28-can-transport.md`; the short version:
//!
//! * The 29-bit extended ID carries everything the link needs:
//!   `prio 2 | dst_node 8 | src_node 8 | end 2 | tid 3 | idx 6`.
//! * The payload carries the ergot frame and nothing else. A message spans
//!   up to [`MAX_FRAMES`] frames: `idx` numbers them from 0 and `end` marks
//!   the last one. A frame's data length is its DLC, except for a CAN FD last
//!   frame that had to be padded to the next DLC: `end` says so, and the data
//!   length sits in its last byte — free, because the padding is at least one
//!   byte.
//! * `tid` numbers a sender's messages per traffic class. A sender never
//!   interleaves two messages of one class, so the receiver assembles at most
//!   one message per (sender, class) and recognises the frames CAN itself
//!   retransmits by `(tid, idx)`.
//! * The segment addressing (`dst_node`, `src_node`) comes from the
//!   [`LinkMeta`] the port hands the sink; the broadcast `dst_node` is
//!   [`BROADCAST_NODE_ID`].
//! * Nothing is acknowledged or retransmitted: at-most-once, as everywhere
//!   else in ergot.

// TODO(header compression): on classic CAN a message is one frame only if
// the whole ergot frame fits 8 bytes, and the header alone takes ~7 (two
// varint addresses of 3-5 bytes each plus the meta byte). Much of it repeats
// the CAN ID: an address on this segment is (the segment's net, the node in
// the ID), so only its port is news. Prototyped 2026-09-29, not adopted:
//
// * A: a "derived from the ID" flag per address (S, D) in the two class bits
//   of the meta byte — the class is already the ID's prio — with meta moved
//   in front of the addresses so the receiver sees the flags first. Costs no
//   ID bits and keeps 8-bit node ids; the link always re-encodes the header.
// * B: S and D in the ID, by narrowing node ids to 7 bits (1..=126, 127 =
//   broadcast). The ergot frame goes untouched unless compressed, but the
//   node range leaks into the core: bus claims on CAN must stay <= 126.
//
// Both put the same bytes on the wire: 2 bytes saved per derivable address
// (3 once net ids reach 32), ~0.5-0.9 KiB of flash, and the receiver rebuilds
// the header (8 bytes of headroom per reassembly slot, a small stack buffer
// for single frames). That turns two frames into one only for small unicast
// messages to a known port: topics and wildcard (port 0) requests carry the
// 9-13 byte any/all appendix, which dominates, and so do the topic-based
// control planes this transport is built for. Revisit — preferring A — once
// hot traffic goes to fixed, known ports (a core API for binding one) or
// topic keys get shorter on the wire. Adopting it changes the payload
// format, so every node on a bus must move at once.

use core::cmp::Reverse;

use bbqueue::{prod_cons::framed::FramedProducer, traits::bbqhdl::BbqHandle};
use postcard::{
    Serializer,
    ser_flavors::{Flavor, Slice},
};
use serde::Serialize;

pub use crate::interface_manager::edge_port::BROADCAST_NODE_ID;
use crate::{
    FrameKind, Header, ProtocolError, TrafficClass,
    interface_manager::{InterfaceSink, LinkDst, LinkMeta},
    time::{Duration, Instant},
    wire_frames::{self, MAX_HDR_ENCODED_SIZE, encode_frame_hdr},
};

/// Largest classic CAN payload.
pub const CLASSIC_MAX_PAYLOAD: usize = 8;
/// Largest CAN FD payload.
pub const FD_MAX_PAYLOAD: usize = 64;
/// Most frames one message may span: `idx` is 6 bits and never wraps.
pub const MAX_FRAMES: usize = 64;
/// Bytes of identifier in front of the payload in [`CanFrame::to_bytes`].
pub const ENCODED_ID_LEN: usize = 4;

/// Whether a CAN frame can carry exactly `len` bytes: 0..=8, or one of the
/// CAN FD lengths above 8.
pub const fn is_frame_len(len: usize) -> bool {
    matches!(len, 0..=8 | 12 | 16 | 20 | 24 | 32 | 48 | 64)
}

/// Whether `max_payload` is usable for a link: a frame length of at least 8
/// (8 for classic CAN, typically 64 for CAN FD). Every frame but the last is
/// full, so it has to be a length a frame carries exactly.
pub const fn is_max_payload(max_payload: usize) -> bool {
    max_payload >= CLASSIC_MAX_PAYLOAD && is_frame_len(max_payload)
}

/// The frame length a CAN FD controller pads `len` data bytes to.
const fn padded_len(len: usize) -> usize {
    match len {
        0..=8 => len,
        9..=12 => 12,
        13..=16 => 16,
        17..=20 => 20,
        21..=24 => 24,
        25..=32 => 32,
        33..=48 => 48,
        _ => 64,
    }
}

/// Largest encoded ergot frame one message can carry at `max_payload`.
/// 512 bytes on classic CAN, 4096 on CAN FD.
pub const fn max_message_len(max_payload: usize) -> usize {
    MAX_FRAMES * max_payload
}

/// Where a frame sits in its message.
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameEnd {
    /// More frames follow.
    More = 0,
    /// The last frame; its data length is the frame length.
    Last = 1,
    /// The last frame, padded to the next CAN FD length; its last byte is the
    /// data length.
    LastPadded = 2,
}

impl FrameEnd {
    const fn from_bits(bits: u8) -> Option<Self> {
        match bits & 0b11 {
            0 => Some(Self::More),
            1 => Some(Self::Last),
            2 => Some(Self::LastPadded),
            _ => None,
        }
    }
}

/// Arbitration priority for a traffic class: its two bits, so `Control`
/// (0) wins.
pub const fn class_prio(class: TrafficClass) -> u8 {
    class.to_bits()
}

/// A 29-bit extended CAN identifier in the ergot layout.
///
/// ```text
///  28 27│26       19│18       11│10  9│8   6│5      0
///  prio │  dst_node │  src_node │ end │ tid │  idx
/// ```
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanId(u32);

impl CanId {
    const PRIO_SHIFT: u32 = 27;
    const DST_SHIFT: u32 = 19;
    const SRC_SHIFT: u32 = 11;
    const END_SHIFT: u32 = 9;
    const TID_SHIFT: u32 = 6;
    /// Largest `tid` (3 bits); the counter wraps past it.
    pub const MAX_TID: u8 = 0x7;
    /// Largest `idx` (6 bits).
    pub const MAX_IDX: u8 = 0x3F;
    /// Largest valid extended identifier.
    pub const MAX_RAW: u32 = 0x1FFF_FFFF;

    pub const fn new(
        prio: u8,
        dst_node: u8,
        src_node: u8,
        end: FrameEnd,
        tid: u8,
        idx: u8,
    ) -> Self {
        Self(
            (((prio & 0b11) as u32) << Self::PRIO_SHIFT)
                | ((dst_node as u32) << Self::DST_SHIFT)
                | ((src_node as u32) << Self::SRC_SHIFT)
                | (((end as u8) as u32) << Self::END_SHIFT)
                | (((tid & Self::MAX_TID) as u32) << Self::TID_SHIFT)
                | (idx & Self::MAX_IDX) as u32,
        )
    }

    /// Wrap a raw identifier. Returns `None` if it does not fit 29 bits.
    pub const fn from_raw(raw: u32) -> Option<Self> {
        if raw > Self::MAX_RAW {
            None
        } else {
            Some(Self(raw))
        }
    }

    pub const fn raw(self) -> u32 {
        self.0
    }

    pub const fn prio(self) -> u8 {
        ((self.0 >> Self::PRIO_SHIFT) & 0b11) as u8
    }

    pub const fn dst_node(self) -> u8 {
        (self.0 >> Self::DST_SHIFT) as u8
    }

    pub const fn src_node(self) -> u8 {
        (self.0 >> Self::SRC_SHIFT) as u8
    }

    /// `None` for the reserved value.
    pub const fn end(self) -> Option<FrameEnd> {
        FrameEnd::from_bits((self.0 >> Self::END_SHIFT) as u8)
    }

    pub const fn tid(self) -> u8 {
        (self.0 >> Self::TID_SHIFT) as u8 & Self::MAX_TID
    }

    pub const fn idx(self) -> u8 {
        self.0 as u8 & Self::MAX_IDX
    }
}

/// One CAN frame: identifier plus up to 64 payload bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanFrame {
    pub id: CanId,
    pub len: u8,
    pub data: [u8; FD_MAX_PAYLOAD],
}

impl CanFrame {
    pub fn new(id: CanId, payload: &[u8]) -> Option<Self> {
        if payload.len() > FD_MAX_PAYLOAD {
            return None;
        }
        let mut data = [0u8; FD_MAX_PAYLOAD];
        data[..payload.len()].copy_from_slice(payload);
        Some(Self {
            id,
            len: payload.len() as u8,
            data,
        })
    }

    /// The whole payload, padding included.
    pub fn payload(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }

    /// The message bytes this frame carries: the payload, or for a padded
    /// last frame the length its last byte gives. `None` for a reserved `end`
    /// or a length byte that does not fit the frame.
    pub fn message_data(&self) -> Option<&[u8]> {
        let payload = self.payload();
        match self.id.end()? {
            FrameEnd::More | FrameEnd::Last => Some(payload),
            FrameEnd::LastPadded => {
                let (&len, _) = payload.split_last()?;
                payload
                    .get(..len as usize)
                    .filter(|d| d.len() < payload.len())
            }
        }
    }

    /// Serialize as `[id BE u32][payload]`, e.g. to carry frames over a byte
    /// channel (bus simulations, host tools).
    pub fn to_bytes(&self, out: &mut [u8]) -> Option<usize> {
        let n = ENCODED_ID_LEN + self.len as usize;
        if out.len() < n {
            return None;
        }
        out[..4].copy_from_slice(&self.id.raw().to_be_bytes());
        out[4..n].copy_from_slice(self.payload());
        Some(n)
    }

    /// Inverse of [`Self::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < ENCODED_ID_LEN || bytes.len() > ENCODED_ID_LEN + FD_MAX_PAYLOAD {
            return None;
        }
        let id = CanId::from_raw(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))?;
        Self::new(id, &bytes[4..])
    }
}

#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentError {
    /// Longer than [`max_message_len`].
    TooLarge,
    /// The `emit` callback of [`fragment`] refused a frame. Frames already
    /// emitted are not taken back.
    Refused,
    /// `max_payload` is not a usable frame length ([`is_max_payload`]).
    BadMaxPayload,
}

/// Splits one encoded ergot frame into CAN frames, in transmission order.
///
/// Every frame but the last carries `max_payload` bytes. The last carries the
/// rest; on CAN FD, if that is not a frame length, it is padded to the next
/// one and marked [`FrameEnd::LastPadded`]. Every frame this yields has a
/// length a controller sends as is. Yielding one frame at a time lets the
/// transmitter await each hand-over to the controller (and serve other
/// traffic in between) without buffering the whole message.
pub struct Fragmenter<'a> {
    frame: &'a [u8],
    max_payload: usize,
    prio: u8,
    dst_node: u8,
    src_node: u8,
    tid: u8,
    /// Bytes of `frame` already emitted.
    off: usize,
    /// Index of the next frame.
    idx: u8,
    done: bool,
}

impl<'a> Fragmenter<'a> {
    pub fn new(
        frame: &'a [u8],
        max_payload: usize,
        prio: u8,
        dst_node: u8,
        src_node: u8,
        tid: u8,
    ) -> Result<Self, FragmentError> {
        if !is_max_payload(max_payload) {
            return Err(FragmentError::BadMaxPayload);
        }
        if frame.len() > max_message_len(max_payload) {
            return Err(FragmentError::TooLarge);
        }
        Ok(Self {
            frame,
            max_payload,
            prio,
            dst_node,
            src_node,
            tid,
            off: 0,
            idx: 0,
            done: false,
        })
    }
}

impl Iterator for Fragmenter<'_> {
    type Item = CanFrame;

    fn next(&mut self) -> Option<CanFrame> {
        if self.done {
            return None;
        }
        let frame: &[u8] = self.frame;
        let rest = &frame[self.off..];
        let mut buf = [0u8; FD_MAX_PAYLOAD];
        let (end, payload): (FrameEnd, &[u8]) = if rest.len() > self.max_payload {
            let chunk = &rest[..self.max_payload];
            self.off += chunk.len();
            (FrameEnd::More, chunk)
        } else if is_frame_len(rest.len()) {
            self.done = true;
            (FrameEnd::Last, rest)
        } else {
            // CAN FD only: not a frame length, so pad to the next one and put
            // the data length in the last byte, which the padding frees.
            self.done = true;
            let padded = padded_len(rest.len());
            buf[..rest.len()].copy_from_slice(rest);
            buf[padded - 1] = rest.len() as u8;
            (FrameEnd::LastPadded, &buf[..padded])
        };
        let id = CanId::new(
            self.prio,
            self.dst_node,
            self.src_node,
            end,
            self.tid,
            self.idx,
        );
        self.idx += 1;
        CanFrame::new(id, payload)
    }
}

/// Split one encoded ergot frame into CAN frames, calling `emit` for each in
/// transmission order. See [`Fragmenter`].
pub fn fragment(
    frame: &[u8],
    max_payload: usize,
    prio: u8,
    dst_node: u8,
    src_node: u8,
    tid: u8,
    mut emit: impl FnMut(&CanFrame) -> Result<(), ()>,
) -> Result<(), FragmentError> {
    for f in Fragmenter::new(frame, max_payload, prio, dst_node, src_node, tid)? {
        emit(&f).map_err(|()| FragmentError::Refused)?;
    }
    Ok(())
}

/// Per-class limits on how long a partial message may sit without progress,
/// and how long a message start is remembered to recognise its repeat.
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReassemblyTimeouts {
    pub control_ms: u64,
    pub normal_ms: u64,
    pub bulk_ms: u64,
    /// How long after a message start an identical start (same sender,
    /// class and `tid`) counts as CAN's retransmission of it. A repeat comes
    /// right behind the original, but the sender's `tid` wraps after 8
    /// messages of a class, so a long window would take a new message whose
    /// 7 predecessors were all lost for a repeat.
    pub duplicate_ms: u64,
}

impl ReassemblyTimeouts {
    pub const DEFAULT: Self = Self {
        control_ms: 20,
        normal_ms: 100,
        bulk_ms: 500,
        duplicate_ms: 20,
    };

    const fn for_prio(&self, prio: u8) -> Duration {
        Duration::from_millis(match prio {
            0 => self.control_ms,
            1 => self.normal_ms,
            _ => self.bulk_ms,
        })
    }

    const fn duplicate(&self) -> Duration {
        Duration::from_millis(self.duplicate_ms)
    }
}

impl Default for ReassemblyTimeouts {
    fn default() -> Self {
        Self::DEFAULT
    }
}

struct Slot<const MTU: usize> {
    active: bool,
    src: u8,
    prio: u8,
    tid: u8,
    next_idx: u8,
    got: u16,
    /// When the message last made progress.
    last: Instant,
    buf: [u8; MTU],
}

impl<const MTU: usize> Slot<MTU> {
    const EMPTY: Self = Self {
        active: false,
        src: 0,
        prio: 0,
        tid: 0,
        next_idx: 0,
        got: 0,
        last: Instant::from_millis(0),
        buf: [0; MTU],
    };
}

/// The latest message start seen from one (sender, class).
#[derive(Clone, Copy)]
struct Start {
    valid: bool,
    src: u8,
    prio: u8,
    tid: u8,
    at: Instant,
}

impl Start {
    const EMPTY: Self = Self {
        valid: false,
        src: 0,
        prio: 0,
        tid: 0,
        at: Instant::from_millis(0),
    };
}

/// How many (sender, class) pairs the repeat filter remembers. A pair pushed
/// out only loses repeat detection for its next start.
const STARTS: usize = 8;

/// Result of feeding one frame to the [`Reassembler`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Push<'f> {
    /// A single-frame message, complete in the frame itself.
    Single(&'f [u8]),
    /// A message is complete in the given slot; take it with
    /// [`Reassembler::finish`].
    Complete(usize),
    /// More frames needed.
    Pending,
    /// The frame was not used.
    Dropped(DropReason),
}

#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// A repeat of a frame already taken (CAN retransmits a frame whose last
    /// EOF bit a transmitter saw disturbed, after receivers accepted it).
    Duplicate,
    /// A continuation with no message assembling from its (sender, class):
    /// its start was lost or went to another node.
    NoMessage,
    /// A continuation of a later message than the one assembling: the
    /// sender moved on, so the one assembling is dropped too.
    Superseded,
    /// A continuation index skipped ahead: frames were lost.
    IndexGap,
    /// The message outgrows the reassembly buffer.
    TooLarge,
    /// No free slot and nothing lower-priority to evict.
    NoSlot,
    /// The message had made no progress for longer than its class timeout
    /// when this frame arrived.
    Expired,
    /// A reserved `end`, or a padded frame whose length byte does not fit.
    Malformed,
}

/// Reassembles messages, `K` at a time, into `MTU`-byte buffers, and filters
/// out frames CAN delivered twice.
pub struct Reassembler<const K: usize, const MTU: usize> {
    slots: [Slot<MTU>; K],
    starts: [Start; STARTS],
    timeouts: ReassemblyTimeouts,
}

impl<const K: usize, const MTU: usize> Reassembler<K, MTU> {
    pub const fn new(timeouts: ReassemblyTimeouts) -> Self {
        Self {
            slots: [Slot::<MTU>::EMPTY; K],
            starts: [Start::EMPTY; STARTS],
            timeouts,
        }
    }

    /// Feed one frame, received at `now`. The arrival times drive the progress
    /// timeouts and the repeat window.
    pub fn push<'f>(&mut self, frame: &'f CanFrame, now: Instant) -> Push<'f> {
        let id = frame.id;
        let (Some(end), Some(data)) = (id.end(), frame.message_data()) else {
            return Push::Dropped(DropReason::Malformed);
        };
        let (src, prio, tid) = (id.src_node(), id.prio(), id.tid());

        if id.idx() == 0 {
            if self.repeats_start(src, prio, tid, now) {
                return Push::Dropped(DropReason::Duplicate);
            }
            self.note_start(src, prio, tid, now);
            // A sender never interleaves two messages of one class: whatever
            // was still assembling from it is dead.
            if let Some(i) = self.find(src, prio) {
                self.slots[i].active = false;
            }
            if data.len() > MTU {
                return Push::Dropped(DropReason::TooLarge);
            }
            if end != FrameEnd::More {
                return Push::Single(data);
            }
            // Reap at arrival: the worker's last `expire()` may be older than
            // a slot that went stale since.
            self.expire(now);
            let Some(i) = self.slot_for_start(prio) else {
                return Push::Dropped(DropReason::NoSlot);
            };
            let s = &mut self.slots[i];
            s.active = true;
            s.src = src;
            s.prio = prio;
            s.tid = tid;
            s.next_idx = 1;
            s.got = data.len() as u16;
            s.last = now;
            s.buf[..data.len()].copy_from_slice(data);
            return Push::Pending;
        }

        let Some(i) = self.find(src, prio) else {
            return Push::Dropped(DropReason::NoMessage);
        };
        let timeout = self.timeouts.for_prio(prio);
        let s = &mut self.slots[i];
        if s.tid != tid {
            s.active = false;
            return Push::Dropped(DropReason::Superseded);
        }
        // Checked at arrival, not only in `expire()`: the worker may have been
        // parked in `recv()` past the timeout, and a late frame must not
        // revive a message that should be dead.
        if now.saturating_duration_since(s.last) > timeout {
            s.active = false;
            return Push::Dropped(DropReason::Expired);
        }
        if id.idx() + 1 == s.next_idx {
            return Push::Dropped(DropReason::Duplicate);
        }
        if id.idx() != s.next_idx {
            s.active = false;
            return Push::Dropped(DropReason::IndexGap);
        }
        let (got, end_at) = (s.got as usize, s.got as usize + data.len());
        if end_at > MTU {
            s.active = false;
            return Push::Dropped(DropReason::TooLarge);
        }
        s.buf[got..end_at].copy_from_slice(data);
        s.got = end_at as u16;
        s.next_idx += 1;
        s.last = now;
        if end == FrameEnd::More {
            Push::Pending
        } else {
            Push::Complete(i)
        }
    }

    /// Take the completed message out of `slot` and free it.
    pub fn finish(&mut self, slot: usize) -> &[u8] {
        let s = &mut self.slots[slot];
        s.active = false;
        &s.buf[..s.got as usize]
    }

    /// Drop every message that has made no progress within its class
    /// timeout.
    pub fn expire(&mut self, now: Instant) {
        for s in self.slots.iter_mut() {
            if s.active && now.saturating_duration_since(s.last) > self.timeouts.for_prio(s.prio) {
                s.active = false;
            }
        }
    }

    /// Number of messages currently assembling.
    pub fn active(&self) -> usize {
        self.slots.iter().filter(|s| s.active).count()
    }

    fn find(&self, src: u8, prio: u8) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.active && s.src == src && s.prio == prio)
    }

    /// A free slot, else the lowest-class, oldest message if the newcomer
    /// outranks it.
    fn slot_for_start(&self, prio: u8) -> Option<usize> {
        if let Some(i) = self.slots.iter().position(|s| !s.active) {
            return Some(i);
        }
        let (i, victim) = self
            .slots
            .iter()
            .enumerate()
            .max_by_key(|(_, s)| (s.prio, Reverse(s.last)))?;
        (victim.prio > prio).then_some(i)
    }

    fn repeats_start(&self, src: u8, prio: u8, tid: u8, now: Instant) -> bool {
        self.starts.iter().any(|s| {
            s.valid
                && s.src == src
                && s.prio == prio
                && s.tid == tid
                && now.saturating_duration_since(s.at) <= self.timeouts.duplicate()
        })
    }

    fn note_start(&mut self, src: u8, prio: u8, tid: u8, now: Instant) {
        let i = self
            .starts
            .iter()
            .position(|s| s.valid && s.src == src && s.prio == prio)
            .or_else(|| self.starts.iter().position(|s| !s.valid))
            .unwrap_or_else(|| {
                // Full: replace the pair heard from longest ago.
                (0..STARTS).min_by_key(|&i| self.starts[i].at).unwrap_or(0)
            });
        self.starts[i] = Start {
            valid: true,
            src,
            prio,
            tid,
            at: now,
        };
    }
}

/// Bytes of link-layer header in front of each ergot frame in the outgoing
/// queues: `[prio, dst_node, src_node]`.
pub const QUEUED_LINK_HEADER: usize = 3;

/// One entry of the outgoing queues: a complete ergot frame and the link
/// addressing its CAN frames will carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedFrame<'a> {
    pub prio: u8,
    pub dst_node: u8,
    pub src_node: u8,
    /// The encoded ergot frame (header and body).
    pub frame: &'a [u8],
}

impl<'a> QueuedFrame<'a> {
    /// Split a queue entry written by [`Sink`].
    pub fn parse(entry: &'a [u8]) -> Option<Self> {
        let (&[prio, dst_node, src_node], frame) = entry.split_first_chunk()?;
        Some(Self {
            prio,
            dst_node,
            src_node,
            frame,
        })
    }
}

/// The CAN sink: serializes each ergot frame, as one entry, onto one of two
/// outgoing queues (Control, and everything else). The worker fragments it
/// when it transmits.
///
/// Queueing whole frames means a send either queues the complete message or
/// nothing: a full queue never leaves a truncated message on the bus. Like
/// the framed stream sink, a send reserves room for the largest frame first,
/// so each queue needs `MTU + 5` contiguous free bytes (link header and bbqueue
/// framing) to accept any send.
///
/// `MTU` is the largest ergot frame this link carries — i.e. the receiver's
/// reassembly buffer size — and what [`InterfaceSink::mtu`] reports.
pub struct Sink<Q, const MTU: usize>
where
    Q: BbqHandle,
{
    max_payload: u8,
    control: FramedProducer<Q, u16>,
    bulk: FramedProducer<Q, u16>,
}

impl<Q, const MTU: usize> Sink<Q, MTU>
where
    Q: BbqHandle,
{
    /// `control` and `bulk` are two separate frame queues; the TX worker
    /// always drains `control` first. `max_payload` is 8 for classic CAN, a
    /// CAN FD frame length (typically 64) otherwise, and must match the link
    /// the worker drives.
    ///
    /// Panics if `max_payload` is not usable ([`is_max_payload`]).
    pub fn new(control: Q, bulk: Q, max_payload: u8) -> Self {
        assert!(
            is_max_payload(max_payload as usize),
            "CAN max_payload must be 8 or a CAN FD frame length"
        );
        Self {
            max_payload,
            control: control.framed_producer(),
            bulk: bulk.framed_producer(),
        }
    }

    pub fn max_payload(&self) -> u8 {
        self.max_payload
    }

    /// Queue one ergot frame, encoded by `encode` into at most `max_len`
    /// bytes, behind its link header. Nothing is queued on any failure.
    fn enqueue(
        &mut self,
        link: &LinkMeta,
        hdr: &Header,
        max_len: usize,
        encode: impl FnOnce(&mut [u8]) -> Result<usize, ()>,
    ) -> Result<(), ()> {
        let prod = if hdr.class == TrafficClass::Control {
            &self.control
        } else {
            &self.bulk
        };
        let grant_len = u16::try_from(QUEUED_LINK_HEADER + max_len).map_err(drop)?;
        let mut wgr = prod.grant(grant_len).map_err(drop)?;
        wgr[0] = class_prio(hdr.class);
        wgr[1] = match link.dst {
            LinkDst::Node(node) => node,
            LinkDst::Broadcast => BROADCAST_NODE_ID,
        };
        wgr[2] = link.src_node;
        let used = encode(&mut wgr[QUEUED_LINK_HEADER..])?;
        if used > self.mtu() as usize {
            // Dropping the grant uncommitted queues nothing.
            return Err(());
        }
        wgr.commit((QUEUED_LINK_HEADER + used) as u16);
        Ok(())
    }
}

impl<Q, const MTU: usize> InterfaceSink for Sink<Q, MTU>
where
    Q: BbqHandle,
{
    /// The smaller of the reassembly buffer and what fragmentation can carry,
    /// so an oversized send is refused up front as `PacketTooBig` rather than
    /// failing at transmit time.
    fn mtu(&self) -> u16 {
        MTU.min(max_message_len(self.max_payload as usize)) as u16
    }

    fn send_ty<T: Serialize>(&mut self, link: &LinkMeta, hdr: &Header, body: &T) -> Result<(), ()> {
        if hdr.kind == FrameKind::PROTOCOL_ERROR {
            return Err(());
        }
        let mtu = self.mtu() as usize;
        self.enqueue(link, hdr, mtu, |buf| {
            Ok(wire_frames::encode_frame_ty(Slice::new(buf), hdr, body)
                .map_err(drop)?
                .len())
        })
    }

    fn send_raw(&mut self, link: &LinkMeta, hdr: &Header, body: &[u8]) -> Result<(), ()> {
        if hdr.kind == FrameKind::PROTOCOL_ERROR {
            return Err(());
        }
        self.enqueue(link, hdr, MAX_HDR_ENCODED_SIZE + body.len(), |buf| {
            let mut ser = Serializer {
                output: Slice::new(buf),
            };
            encode_frame_hdr(&mut ser, hdr).map_err(drop)?;
            ser.output.try_extend(body).map_err(drop)?;
            Ok(ser.output.finalize().map_err(drop)?.len())
        })
    }

    fn send_err(&mut self, link: &LinkMeta, hdr: &Header, err: ProtocolError) -> Result<(), ()> {
        if hdr.kind != FrameKind::PROTOCOL_ERROR {
            return Err(());
        }
        let mtu = self.mtu() as usize;
        self.enqueue(link, hdr, mtu, |buf| {
            Ok(wire_frames::encode_frame_err(Slice::new(buf), hdr, err)
                .map_err(drop)?
                .len())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASSIC: usize = CLASSIC_MAX_PAYLOAD;
    const FD: usize = FD_MAX_PAYLOAD;
    /// A Normal-class sender (prio 1).
    const NORMAL: u8 = 1;

    fn frames(msg: &[u8], max_payload: usize, prio: u8, tid: u8) -> Vec<CanFrame> {
        Fragmenter::new(msg, max_payload, prio, 0x11, 0x22, tid)
            .unwrap()
            .collect()
    }

    fn msg(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + 3) as u8).collect()
    }

    /// The instant `ms` milliseconds in.
    fn at(ms: u64) -> Instant {
        Instant::from_millis(ms)
    }

    /// Feed `frames` one ms apart; return every message delivered.
    fn deliver<const K: usize, const MTU: usize>(
        r: &mut Reassembler<K, MTU>,
        frames: &[CanFrame],
    ) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for (t, f) in frames.iter().enumerate() {
            match r.push(f, at(t as u64)) {
                Push::Single(d) => out.push(d.to_vec()),
                Push::Complete(i) => out.push(r.finish(i).to_vec()),
                _ => {}
            }
        }
        out
    }

    fn reassembler<const MTU: usize>() -> Reassembler<2, MTU> {
        Reassembler::new(ReassemblyTimeouts::DEFAULT)
    }

    #[test]
    fn id_round_trips_every_field() {
        for (prio, dst, src, end, tid, idx) in [
            (0u8, 0x01u8, 0x02u8, FrameEnd::Last, 0u8, 0u8),
            (3, 0xFF, 0xFE, FrameEnd::More, 7, 63),
            (2, 0x10, 0x7F, FrameEnd::LastPadded, 5, 17),
        ] {
            let id = CanId::new(prio, dst, src, end, tid, idx);
            assert!(id.raw() <= CanId::MAX_RAW);
            assert_eq!(
                (
                    id.prio(),
                    id.dst_node(),
                    id.src_node(),
                    id.end(),
                    id.tid(),
                    id.idx()
                ),
                (prio, dst, src, Some(end), tid, idx)
            );
            assert_eq!(CanId::from_raw(id.raw()), Some(id));
        }
        assert_eq!(CanId::from_raw(CanId::MAX_RAW + 1), None);
        // The reserved `end` value.
        assert_eq!(CanId::from_raw(0b11 << 9).unwrap().end(), None);
    }

    #[test]
    fn only_frame_lengths_of_at_least_8_are_usable_max_payloads() {
        for n in 0..=FD + 1 {
            let usable = matches!(n, 8 | 12 | 16 | 20 | 24 | 32 | 48 | 64);
            assert_eq!(is_max_payload(n), usable, "{n}");
            if !usable {
                assert_eq!(
                    Fragmenter::new(&[1], n, 0, 1, 2, 0).err(),
                    Some(FragmentError::BadMaxPayload)
                );
            }
        }
    }

    /// Every length up to the limit, classic and FD: every frame has a length
    /// a controller sends as is, indices count from 0, only the last frame is
    /// marked last, and the message comes back whole.
    #[test]
    fn every_length_round_trips_classic_and_fd() {
        for max in [CLASSIC, FD] {
            for len in 0..=max_message_len(max) {
                let m = msg(len);
                let fs = frames(&m, max, NORMAL, 3);
                assert_eq!(fs.len(), len.div_ceil(max).max(1), "len {len} max {max}");
                for (i, f) in fs.iter().enumerate() {
                    assert!(is_frame_len(f.len as usize), "len {len} max {max}");
                    assert_eq!(f.id.idx() as usize, i);
                    assert_eq!(f.id.end() == Some(FrameEnd::More), i + 1 < fs.len());
                }
                let mut r = reassembler::<4096>();
                assert_eq!(deliver(&mut r, &fs), vec![m], "len {len} max {max}");
                assert_eq!(r.active(), 0);
            }
        }
    }

    #[test]
    fn classic_frames_carry_no_link_overhead() {
        // 8 bytes: one frame; 16 bytes: exactly two full frames.
        assert_eq!(frames(&msg(8), CLASSIC, NORMAL, 0).len(), 1);
        let fs = frames(&msg(16), CLASSIC, NORMAL, 0);
        assert_eq!(fs.len(), 2);
        assert!(fs.iter().all(|f| f.len == 8));
    }

    #[test]
    fn fd_pads_a_last_frame_and_keeps_its_length_in_the_spare_byte() {
        // 21 bytes: padded to 24, the length in byte 23.
        let fs = frames(&msg(21), FD, NORMAL, 0);
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].id.end(), Some(FrameEnd::LastPadded));
        assert_eq!(fs[0].len, 24);
        assert_eq!(fs[0].payload()[23], 21);
        // 24 bytes: exact, no length byte.
        let fs = frames(&msg(24), FD, NORMAL, 0);
        assert_eq!((fs[0].id.end(), fs[0].len), (Some(FrameEnd::Last), 24));
        // 64 + 21: a full frame, then a padded one.
        let fs = frames(&msg(85), FD, NORMAL, 0);
        assert_eq!(fs.len(), 2);
        assert_eq!(fs[1].id.end(), Some(FrameEnd::LastPadded));
    }

    #[test]
    fn a_padded_length_byte_that_does_not_fit_is_malformed() {
        let mut f = frames(&msg(21), FD, NORMAL, 0)[0];
        f.data[23] = 24;
        let mut r = reassembler::<256>();
        assert_eq!(r.push(&f, at(0)), Push::Dropped(DropReason::Malformed));
    }

    #[test]
    fn fragment_limit_is_max_message_len() {
        assert_eq!(max_message_len(CLASSIC), 512);
        assert_eq!(max_message_len(FD), 4096);
        for max in [CLASSIC, FD] {
            let limit = max_message_len(max);
            let m = msg(limit + 1);
            let fs = frames(&m[..limit], max, NORMAL, 0);
            assert_eq!(fs.len(), MAX_FRAMES);
            assert_eq!(fs.last().unwrap().id.idx() as usize, MAX_FRAMES - 1);
            assert_eq!(
                Fragmenter::new(&m, max, NORMAL, 1, 2, 0).err(),
                Some(FragmentError::TooLarge)
            );
        }
    }

    #[test]
    fn every_repeated_frame_is_delivered_once() {
        // Each frame of a 3-frame message and of a single-frame message sent
        // twice in a row, as CAN's automatic retransmission does.
        let multi = frames(&msg(20), CLASSIC, NORMAL, 1);
        let single = frames(&msg(5), CLASSIC, NORMAL, 2);
        let doubled: Vec<CanFrame> = multi
            .iter()
            .chain(single.iter())
            .flat_map(|f| [*f, *f])
            .collect();
        let mut r = reassembler::<64>();
        assert_eq!(deliver(&mut r, &doubled), vec![msg(20), msg(5)]);
    }

    #[test]
    fn a_repeated_start_outside_the_window_is_a_new_message() {
        let f = frames(&msg(5), CLASSIC, NORMAL, 4)[0];
        let mut r = reassembler::<64>();
        assert!(matches!(r.push(&f, at(0)), Push::Single(_)));
        assert_eq!(r.push(&f, at(1)), Push::Dropped(DropReason::Duplicate));
        let later = ReassemblyTimeouts::DEFAULT.duplicate_ms + 1;
        assert!(matches!(r.push(&f, at(later)), Push::Single(_)));
    }

    #[test]
    fn lost_tail_of_a_and_lost_start_of_b_delivers_nothing() {
        // A and B have the same length, so their indices line up — only the
        // tid tells B's tail from the rest of A.
        let a = frames(&[0xAA; 20], CLASSIC, NORMAL, 1);
        let b = frames(&[0xBB; 20], CLASSIC, NORMAL, 2);
        let mut r = reassembler::<64>();
        assert_eq!(r.push(&a[0], at(0)), Push::Pending);
        assert_eq!(r.push(&b[1], at(1)), Push::Dropped(DropReason::Superseded));
        assert_eq!(r.push(&b[2], at(2)), Push::Dropped(DropReason::NoMessage));
        assert_eq!(r.active(), 0);
    }

    #[test]
    fn a_new_start_retires_the_message_assembling_from_that_sender() {
        let a = frames(&msg(20), CLASSIC, NORMAL, 1);
        let b = frames(&msg(20), CLASSIC, NORMAL, 2);
        let mut r = reassembler::<64>();
        let mut script = vec![a[0]]; // A's tail is lost
        script.extend_from_slice(&b);
        assert_eq!(deliver(&mut r, &script), vec![msg(20)]);
        assert_eq!(r.active(), 0);
    }

    #[test]
    fn control_interleaves_into_bulk_from_the_same_sender() {
        let bulk = frames(&msg(60), CLASSIC, 2, 7);
        let ctl = frames(&msg(20), CLASSIC, 0, 7);
        let mut order: Vec<CanFrame> = bulk[..2].to_vec();
        order.extend_from_slice(&ctl);
        order.extend_from_slice(&bulk[2..]);
        let mut r = reassembler::<64>();
        assert_eq!(deliver(&mut r, &order), vec![msg(20), msg(60)]);
    }

    #[test]
    fn an_index_gap_drops_the_message() {
        let mut fs = frames(&msg(40), CLASSIC, NORMAL, 3);
        fs.remove(2);
        let mut r = reassembler::<64>();
        let outcomes: Vec<Push> = fs
            .iter()
            .enumerate()
            .map(|(t, f)| r.push(f, at(t as u64)))
            .collect();
        assert!(outcomes.contains(&Push::Dropped(DropReason::IndexGap)));
        assert!(!outcomes.iter().any(|p| matches!(p, Push::Complete(_))));
        assert_eq!(r.active(), 0);
    }

    #[test]
    fn a_stalled_message_expires_by_class() {
        let fs = frames(&msg(40), CLASSIC, NORMAL, 3);
        let mut r = reassembler::<64>();
        assert_eq!(r.push(&fs[0], at(0)), Push::Pending);
        r.expire(at(ReassemblyTimeouts::DEFAULT.normal_ms));
        assert_eq!(r.active(), 1, "not yet");
        r.expire(at(ReassemblyTimeouts::DEFAULT.normal_ms + 1));
        assert_eq!(r.active(), 0);
    }

    #[test]
    fn a_late_frame_does_not_revive_an_expired_message() {
        let fs = frames(&msg(10), CLASSIC, NORMAL, 1);
        let mut r = reassembler::<64>();
        assert_eq!(r.push(&fs[0], at(0)), Push::Pending);
        // Nobody called expire() in between: arrival time alone decides.
        let late = ReassemblyTimeouts::DEFAULT.normal_ms + 1;
        assert_eq!(r.push(&fs[1], at(late)), Push::Dropped(DropReason::Expired));
        assert_eq!(r.active(), 0);
    }

    #[test]
    fn full_slots_evict_bulk_for_control_but_not_the_reverse() {
        let mut r = Reassembler::<1, 64>::new(ReassemblyTimeouts::DEFAULT);
        let bulk = Fragmenter::new(&msg(40), CLASSIC, 2, 1, 0x30, 0)
            .unwrap()
            .next()
            .unwrap();
        let ctl = Fragmenter::new(&msg(40), CLASSIC, 0, 1, 0x31, 0)
            .unwrap()
            .next()
            .unwrap();
        let bulk2 = Fragmenter::new(&msg(40), CLASSIC, 2, 1, 0x32, 0)
            .unwrap()
            .next()
            .unwrap();
        assert_eq!(r.push(&bulk, at(0)), Push::Pending);
        assert_eq!(r.push(&ctl, at(1)), Push::Pending, "control evicts bulk");
        assert_eq!(r.push(&bulk2, at(2)), Push::Dropped(DropReason::NoSlot));
    }

    #[test]
    fn a_message_larger_than_the_buffer_is_dropped() {
        let fs = frames(&msg(40), CLASSIC, NORMAL, 0);
        let mut r = reassembler::<32>();
        assert!(deliver(&mut r, &fs).is_empty());
        assert_eq!(r.active(), 0);
    }

    #[cfg(feature = "std")]
    #[test]
    fn sink_mtu_is_capped_by_the_fragment_limit() {
        use crate::interface_manager::utils::std::new_std_queue;
        let q = || new_std_queue(64);
        let classic = Sink::<_, 4096>::new(q(), q(), CLASSIC as u8);
        assert_eq!(classic.mtu(), 512);
        let fd = Sink::<_, 4096>::new(q(), q(), FD as u8);
        assert_eq!(fd.mtu(), 4096);
    }

    #[test]
    fn frame_bytes_round_trip() {
        let f = CanFrame::new(CanId::new(1, 2, 3, FrameEnd::More, 4, 9), &[1, 2, 3, 4, 5]).unwrap();
        let mut buf = [0u8; ENCODED_ID_LEN + FD_MAX_PAYLOAD];
        let n = f.to_bytes(&mut buf).unwrap();
        assert_eq!(n, ENCODED_ID_LEN + 5);
        assert_eq!(CanFrame::from_bytes(&buf[..n]), Some(f));
    }
}
