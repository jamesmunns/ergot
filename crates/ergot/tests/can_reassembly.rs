//! The CAN reassembler on its own: frames in, messages out. What it makes of
//! arbitrary frames, of repeats, of lost frames and of a sender that serves
//! several destinations.

#![cfg(feature = "tokio-std")]

use ergot::interface_manager::utils::can::{
    CanFrame, CanId, DropReason, Fragmenter, FrameEnd, Push, Reassembler, ReassemblyTimeouts,
    TidCounters, max_message_len,
};
use ergot::time::Instant;

const NORMAL: u8 = 1;
const BULK: u8 = 2;

fn at(ms: u64) -> Instant {
    Instant::from_millis(ms)
}

fn frames(msg: &[u8], max: usize, prio: u8, dst: u8, src: u8, tid: u8) -> Vec<CanFrame> {
    Fragmenter::new(msg, max, prio, dst, src, tid)
        .unwrap()
        .collect()
}

fn msg(len: usize, salt: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(salt))
        .collect()
}

/// xorshift64*
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }
}

/// Feed timed frames to a reassembler as the worker does, and collect what
/// it hands out and what it drops.
fn run<const K: usize, const MTU: usize>(
    r: &mut Reassembler<K, MTU>,
    script: &[(u64, CanFrame)],
) -> (Vec<Vec<u8>>, Vec<DropReason>) {
    let mut out = Vec::new();
    let mut drops = Vec::new();
    for (t, f) in script {
        r.expire(at(*t));
        match r.push(f, at(*t)) {
            Push::Single(d) => out.push(d.to_vec()),
            Push::Complete(i) => out.push(r.finish(i).to_vec()),
            Push::Pending => {}
            Push::Dropped(why) => drops.push(why),
        }
    }
    (out, drops)
}

/// Only the frames a node's filter passes: its own and everyone's.
fn heard_by(node: u8, script: &[(u64, CanFrame)]) -> Vec<(u64, CanFrame)> {
    script
        .iter()
        .filter(|(_, f)| f.id.dst_node() == node || f.id.dst_node() == 255)
        .copied()
        .collect()
}

// ---------------------------------------------------------------------------
// Arbitrary input.
// ---------------------------------------------------------------------------

/// Random frames, many of them continuing the one before from their sender:
/// nothing panics, no message outgrows the buffer, no more than `K` assemble.
fn fuzz<const K: usize, const MTU: usize>(seed: u64, iters: usize) {
    let mut rng = Rng(seed);
    let mut r = Reassembler::<K, MTU>::new(ReassemblyTimeouts::DEFAULT);
    let mut now = 0u64;
    // What would continue the last frame of each (src, prio): dst, tid, idx.
    let mut next = [[(0u8, 0u8, 0u8); 4]; 4];
    for _ in 0..iters {
        now += match rng.below(10) {
            0 => rng.below(600),
            1 => rng.below(30),
            _ => 0,
        };
        let src = rng.below(4) as u8;
        let prio = rng.below(4) as u8;
        let (ndst, ntid, nidx) = next[src as usize][prio as usize];
        let follow = rng.chance(3, 4);
        let dst = if follow { ndst } else { rng.below(3) as u8 };
        let tid = if follow { ntid } else { rng.below(8) as u8 };
        let idx = if follow { nidx } else { rng.below(64) as u8 };
        let end_bits = match rng.below(20) {
            0 => 3u32,
            1 => 2,
            2 | 3 => 1,
            _ => 0,
        };
        let raw = ((prio as u32) << 27)
            | ((dst as u32) << 19)
            | ((src as u32) << 11)
            | (end_bits << 9)
            | ((tid as u32) << 6)
            | idx as u32;
        let id = CanId::from_raw(raw).unwrap();
        let len = match rng.below(4) {
            0 => rng.below(65) as usize,
            1 => 64,
            2 => 8,
            _ => rng.below(9) as usize,
        };
        let mut data = [0u8; 64];
        for b in data.iter_mut() {
            *b = rng.next() as u8;
        }
        let f = CanFrame::new(id, &data[..len]).unwrap();
        match r.push(&f, at(now)) {
            Push::Single(d) => assert!(d.len() <= MTU),
            Push::Complete(i) => assert!(r.finish(i).len() <= MTU),
            _ => {}
        }
        assert!(r.active() <= K);
        next[src as usize][prio as usize] = (dst, tid, (idx + 1) & 0x3F);
        if rng.chance(1, 50) {
            r.expire(at(now));
        }
    }
}

#[test]
#[cfg_attr(miri, ignore = "hundreds of thousands of frames")]
fn arbitrary_frames_never_panic_or_outgrow_the_buffer() {
    let n = 200_000;
    fuzz::<0, 0>(1, n);
    fuzz::<1, 0>(2, n);
    fuzz::<1, 1>(3, n);
    fuzz::<2, 7>(4, n);
    fuzz::<2, 8>(5, n);
    fuzz::<2, 64>(6, n);
    fuzz::<4, 256>(7, n);
    fuzz::<3, 4096>(8, n);
    fuzz::<2, 70000>(9, n);
}

/// A peer that never ends its message: 64 frames of `More` are taken, the
/// next one is not frame 65, and the slot does not outlive its timeout.
#[test]
fn a_message_that_never_ends_is_bounded() {
    let mut r = Reassembler::<1, 4096>::new(ReassemblyTimeouts::DEFAULT);
    for idx in 0..64u8 {
        let id = CanId::new(NORMAL, 1, 9, FrameEnd::More, 3, idx);
        let f = CanFrame::new(id, &[0xEE; 64]).unwrap();
        assert_eq!(r.push(&f, at(idx as u64)), Push::Pending, "idx {idx}");
    }
    let id = CanId::new(NORMAL, 1, 9, FrameEnd::Last, 3, 63);
    let f = CanFrame::new(id, &[0xEE; 64]).unwrap();
    assert_eq!(r.push(&f, at(64)), Push::Dropped(DropReason::Duplicate));
    assert_eq!(r.active(), 1);
    r.expire(at(64 + 101));
    assert_eq!(r.active(), 0);
}

/// `len` and `data` of a frame are public, so nothing keeps `len` within the
/// data. Such a frame is cut to its data.
#[test]
fn a_frame_with_a_length_beyond_its_data_is_cut_to_it() {
    let f = CanFrame {
        id: CanId::new(NORMAL, 1, 2, FrameEnd::Last, 0, 0),
        len: 65,
        data: [7; 64],
    };
    assert_eq!(f.payload(), &[7; 64][..]);
    let mut r = Reassembler::<1, 64>::new(ReassemblyTimeouts::DEFAULT);
    assert_eq!(r.push(&f, at(0)), Push::Single(&[7; 64][..]));
}

#[test]
#[cfg_attr(miri, ignore = "hundreds of thousands of frames")]
fn every_length_round_trips_for_every_fd_max_payload() {
    for max in [12usize, 16, 20, 24, 32, 48] {
        for len in 0..=max_message_len(max) {
            let m = msg(len, 3);
            let fs = frames(&m, max, NORMAL, 0x11, 0x22, 3);
            assert_eq!(fs.len(), len.div_ceil(max).max(1));
            let mut r = Reassembler::<2, 4096>::new(ReassemblyTimeouts::DEFAULT);
            let mut out = Vec::new();
            for (t, f) in fs.iter().enumerate() {
                assert!(f.len as usize <= max, "frame longer than max_payload");
                match r.push(f, at(t as u64)) {
                    Push::Single(d) => out.push(d.to_vec()),
                    Push::Complete(i) => out.push(r.finish(i).to_vec()),
                    Push::Pending => {}
                    Push::Dropped(why) => panic!("len {len} max {max}: {why:?}"),
                }
            }
            assert_eq!(out, vec![m], "len {len} max {max}");
        }
    }
}

#[test]
fn eviction_takes_the_oldest_slot_of_the_lowest_class() {
    let start = |prio: u8, src: u8| {
        let id = CanId::new(prio, 1, src, FrameEnd::More, 0, 0);
        CanFrame::new(id, &[src; 8]).unwrap()
    };
    let cont = |prio: u8, src: u8| {
        let id = CanId::new(prio, 1, src, FrameEnd::Last, 0, 1);
        CanFrame::new(id, &[src; 8]).unwrap()
    };
    let mut r = Reassembler::<3, 64>::new(ReassemblyTimeouts::DEFAULT);
    assert_eq!(r.push(&start(1, 10), at(0)), Push::Pending); // Normal, oldest
    assert_eq!(r.push(&start(2, 20), at(1)), Push::Pending); // Bulk, older
    assert_eq!(r.push(&start(2, 21), at(2)), Push::Pending); // Bulk, newer
    // Control arrives: it evicts src 20 (Bulk, older) and keeps 10 and 21.
    assert_eq!(r.push(&start(0, 30), at(3)), Push::Pending);
    assert!(matches!(r.push(&cont(1, 10), at(4)), Push::Complete(_)));
    assert_eq!(
        r.push(&cont(2, 20), at(4)),
        Push::Dropped(DropReason::NoMessage)
    );
    assert!(matches!(r.push(&cont(2, 21), at(4)), Push::Complete(_)));
    // A class does not evict itself.
    let mut r = Reassembler::<1, 64>::new(ReassemblyTimeouts::DEFAULT);
    assert_eq!(r.push(&start(3, 10), at(0)), Push::Pending);
    assert_eq!(
        r.push(&start(3, 11), at(1)),
        Push::Dropped(DropReason::NoSlot)
    );
    // Bulk evicts Background.
    assert_eq!(r.push(&start(2, 12), at(2)), Push::Pending);
}

// ---------------------------------------------------------------------------
// Repeats: CAN sends a frame again when its transmitter saw the end of it
// disturbed, though the receivers had taken it.
// ---------------------------------------------------------------------------

/// Repeats of a start, of a middle frame, of a last frame and of a single
/// frame, each behind frames of another sender and of another class.
#[test]
fn repeats_behind_other_traffic_are_still_recognised() {
    let m = msg(20, 1);
    let bulk = frames(&m, 8, BULK, 1, 7, 5);
    // Another sender, same class and tid.
    let other = frames(&msg(20, 2), 8, BULK, 1, 8, 5);
    // The same sender, Control.
    let control = frames(&msg(12, 3), 8, 0, 1, 7, 5);
    let single = frames(&msg(4, 4), 8, BULK, 1, 7, 6);
    let script: Vec<(u64, CanFrame)> = [
        bulk[0], other[0], control[0], bulk[0], //
        bulk[1], other[1], control[1], control[1], bulk[1], //
        other[2], bulk[2], other[2], bulk[2], //
        single[0], single[0],
    ]
    .into_iter()
    .enumerate()
    .map(|(t, f)| (t as u64, f))
    .collect();
    let mut r = Reassembler::<4, 64>::new(ReassemblyTimeouts::DEFAULT);
    let (out, _) = run(&mut r, &script);
    assert_eq!(out, vec![msg(12, 3), msg(20, 2), m, msg(4, 4)]);
}

/// 40 messages half a millisecond apart: the tid wraps five times inside the
/// repeat window, and no message is taken for a repeat of the one eight
/// before it.
#[test]
fn a_wrapping_tid_drops_nothing() {
    for len in [5, 20] {
        let mut script = Vec::new();
        let mut want = Vec::new();
        for n in 0..40u8 {
            let m = msg(len, n);
            for f in frames(&m, 8, NORMAL, 1, 7, n & 7) {
                script.push((n as u64 / 2, f));
            }
            want.push(m);
        }
        let mut r = Reassembler::<2, 64>::new(ReassemblyTimeouts::DEFAULT);
        assert_eq!(run(&mut r, &script).0, want, "{len}-byte messages");
    }
}

/// A sender restarts in the middle of a message and begins another one. The
/// new message is delivered and the old one is not, whatever `tid` the new
/// life starts with and however soon.
#[test]
fn a_restarted_sender_is_heard_again() {
    let old = msg(30, 0x10);
    let new = msg(30, 0x80);
    let before = frames(&old, 8, NORMAL, 1, 7, 3);
    for (new_tid, gap_ms) in [(6u8, 5u64), (3, 50), (3, 10)] {
        let mut script = vec![(0, before[0]), (0, before[1])];
        for f in frames(&new, 8, NORMAL, 1, 7, new_tid) {
            script.push((gap_ms, f));
        }
        let mut r = Reassembler::<2, 64>::new(ReassemblyTimeouts::DEFAULT);
        let (out, drops) = run(&mut r, &script);
        assert_eq!(
            out,
            vec![new.clone()],
            "tid {new_tid} after {gap_ms} ms: drops {drops:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// A sender that serves several destinations, and what one of them hears.
// ---------------------------------------------------------------------------

/// One sender, one class, several destinations in turn, at several cycle
/// times: a destination gets every one of its messages, however many go to
/// the others between two of them.
#[test]
#[cfg_attr(miri, ignore = "hundreds of thousands of frames")]
fn messages_to_several_nodes_keep_their_own_numbers() {
    const ME: u8 = 41;
    for nodes in [2u8, 4, 7, 8, 9, 16] {
        for cycle_ms in [5u64, 10, 20, 21] {
            for len in [6, 20] {
                let mut tids = TidCounters::new(0);
                let mut script = Vec::new();
                let mut mine = Vec::new();
                for cycle in 0..40u64 {
                    for k in 0..nodes {
                        let dst = ME + k;
                        let m = msg(len, (cycle as u8).wrapping_mul(16).wrapping_add(k));
                        if dst == ME {
                            mine.push(m.clone());
                        }
                        let tid = tids.next(NORMAL, dst);
                        for f in frames(&m, 8, NORMAL, dst, 1, tid) {
                            script.push((cycle * cycle_ms, f));
                        }
                    }
                }
                let mut r = Reassembler::<4, 256>::new(ReassemblyTimeouts::DEFAULT);
                let (out, drops) = run(&mut r, &heard_by(ME, &script));
                assert_eq!(
                    out, mine,
                    "{nodes} destinations, {cycle_ms} ms cycle, {len} bytes: drops {drops:?}"
                );
            }
        }
    }
}

/// The frames of a message to another node do not continue a message to
/// this one, even pushed unfiltered and with the `tid` and `idx` that fit.
#[test]
fn another_destinations_frames_do_not_continue_a_message() {
    const ME: u8 = 41;
    const OTHER: u8 = 42;
    let a = msg(20, 0xA0);
    let b = msg(20, 0xB0);
    let mut script = vec![(0u64, frames(&a, 8, NORMAL, ME, 1, 1)[0])];
    for f in frames(&b, 8, NORMAL, OTHER, 1, 1) {
        script.push((10, f));
    }
    let mut r = Reassembler::<4, 256>::new(ReassemblyTimeouts::DEFAULT);
    let (out, _) = run(&mut r, &script);
    assert_eq!(out, vec![b], "only the message that arrived whole");
}

/// The tail of a message is lost, and the next start this node sees carries
/// the same `tid`, inside the repeat window. It is another frame than the
/// start before, so it begins a new message instead of passing for a repeat
/// and leaving its own tail to complete the old one.
#[test]
fn a_new_start_with_the_number_of_a_stale_message_replaces_it() {
    const ME: u8 = 41;
    let a = msg(20, 0xA0);
    let b = msg(20, 0xB0);
    let first = frames(&a, 8, NORMAL, ME, 1, 1);
    let second = frames(&b, 8, NORMAL, ME, 1, 1);
    let script = vec![
        (0, first[0]),
        (10, second[0]),
        (10, second[1]),
        (10, second[2]),
    ];
    let mut r = Reassembler::<4, 256>::new(ReassemblyTimeouts::DEFAULT);
    let (out, drops) = run(&mut r, &script);
    assert_eq!(out, vec![b], "drops {drops:?}");
}

// ---------------------------------------------------------------------------
// Frames lost in a burst, as when a controller's FIFO overruns.
// ---------------------------------------------------------------------------

/// A receiver that goes deaf for 5 to 40 ms now and then, on a stream of
/// multi-frame messages. Hearing again in the middle of a message whose
/// `tid` and `idx` happen to fit, it would complete the message it was
/// assembling before with that tail: one outage in twenty did. Told of the
/// loss (`reset`), it hands out nothing that was not sent.
#[test]
#[cfg_attr(miri, ignore = "hundreds of thousands of frames")]
fn after_a_reset_no_later_frame_completes_an_earlier_message() {
    let mut rng = Rng(0xC0FFEE);
    // An 8-byte extended frame at 1 Mbit/s.
    let frame_us = 130u64;
    for (prio, nframes) in [(NORMAL, 4usize), (BULK, 4), (BULK, 2)] {
        let mut r = Reassembler::<4, 256>::new(ReassemblyTimeouts::DEFAULT);
        let mut tids = TidCounters::new(0);
        let mut now_us = 0u64;
        let mut sent: Vec<Vec<u8>> = Vec::new();
        let mut deaf_until = 0u64;
        let mut deaf = false;
        let (mut outages, mut delivered) = (0u32, 0u32);
        for n in 0..200_000u32 {
            let len = (nframes - 1) * 8 + 1 + rng.below(8) as usize;
            let mut m = msg(len, n as u8);
            m[0] = (n >> 8) as u8;
            m[1] = (n >> 16) as u8;
            let tid = tids.next(prio, 41);
            let fs = frames(&m, 8, prio, 41, 1, tid);
            assert_eq!(fs.len(), nframes);
            sent.push(m);
            if sent.len() > 64 {
                sent.remove(0);
            }
            for f in fs {
                now_us += frame_us;
                if now_us >= deaf_until && rng.chance(1, 2000) {
                    deaf_until = now_us + 5_000 + rng.below(35_000);
                    outages += 1;
                }
                if now_us < deaf_until {
                    deaf = true;
                    continue;
                }
                if deaf {
                    // The adapter reports the overrun before this frame.
                    r.reset();
                    deaf = false;
                }
                let t = at(now_us / 1000);
                r.expire(t);
                let got = match r.push(&f, t) {
                    Push::Single(d) => Some(d.to_vec()),
                    Push::Complete(i) => Some(r.finish(i).to_vec()),
                    _ => None,
                };
                if let Some(d) = got {
                    delivered += 1;
                    assert!(
                        sent.contains(&d),
                        "class {prio}, {nframes} frames a message: a message nobody sent \
                         was delivered after {outages} outages"
                    );
                }
            }
        }
        assert!(outages > 100 && delivered > 150_000);
    }
}
