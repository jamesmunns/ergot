//! Regression test for the COBS stream sink's write-grant sizing.
#![cfg(feature = "std")]
// A grant-sizing correctness test, not a soundness test. The std bbqueue it builds
// trips Miri's leak checker (as with the other std-queue tests in this suite), so
// exclude it from Miri.
#![cfg(not(miri))]

use ergot::interface_manager::InterfaceSink;
use ergot::interface_manager::utils::cobs_stream::Sink;
use ergot::interface_manager::utils::std::new_std_queue;
use ergot::{Address, FrameKind, HeaderSeq};

/// A worst-case frame exactly at the MTU must be accepted by the COBS stream sink.
///
/// postcard's `Cobs` serialization flavor appends a `0x00` frame delimiter, but the
/// sink used to request a grant of only `cobs::max_encoding_length(mtu)` bytes,
/// which accounts for the COBS run overhead but NOT that trailing delimiter. A
/// fully zero-free frame of exactly `mtu` bytes hits COBS's worst case: the encoded
/// data uses the full `max_encoding_length` and the delimiter needs one more byte,
/// so the grant overflows and the send fails with a spurious "buffer full".
///
/// This frame is constructed to be entirely zero-free (any single zero byte would
/// shrink the COBS overhead by a byte and hide the off-by-one):
///   src/dst = 0xFFFF_FFFF -> varint `FF FF FF FF 0F` (5 bytes each)
///   seq_no  = 0xFFFF      -> varint `FF FF 03`        (3 bytes)
///   kind    = TOPIC_MSG (3), ttl = 0x40              (1 byte each)
/// => 15-byte header; body `[0xFF; 5]` => a 20-byte frame, so MTU = 20.
#[test]
fn cobs_sink_accepts_worst_case_mtu_frame() {
    let hdr = HeaderSeq {
        src: Address::from_word(0xFFFF_FFFF),
        dst: Address::from_word(0xFFFF_FFFF),
        any_all: None,
        seq_no: 0xFFFF,
        kind: FrameKind::TOPIC_MSG,
        ttl: 0x40,
    };
    let body: [u8; 5] = [0xFF; 5];
    const MTU: u16 = 20;

    let q = new_std_queue(256);
    let mut sink = Sink::new_from_handle(q, MTU);

    assert_eq!(
        sink.send_ty(&hdr, &body),
        Ok(()),
        "worst-case frame at the MTU must fit the write grant"
    );
}
