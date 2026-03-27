//! MTU discovery tests: wire format round-trip and router PacketTooBig e2e.

#![cfg(any(feature = "std", feature = "nostd-seed-router"))]

use ergot::interface_manager::{
    Interface, InterfaceSendError, InterfaceSink, Profile, profiles::router::Router,
};
use ergot::wire_frames::{de_frame, encode_frame_err};
use ergot::{Address, FrameKind, HeaderSeq, ProtocolError};
use rand_core::RngCore;
use serde::Serialize;
use std::sync::{Arc, Mutex};

// --- Mock RNG ---

struct MockRng(u64);

impl RngCore for MockRng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(1);
        self.0
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for b in dest.iter_mut() {
            *b = self.next_u64() as u8;
        }
    }
}

// --- Mock sink with configurable MTU ---

#[derive(Clone)]
struct MtuSink {
    mtu: u16,
    log: Arc<Mutex<Vec<SinkEvent>>>,
    label: &'static str,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
enum SinkEvent {
    SendTy {
        label: &'static str,
        dst: Address,
    },
    SendRaw {
        label: &'static str,
        dst: Address,
    },
    SendErr {
        label: &'static str,
        dst: Address,
        err: ProtocolError,
    },
}

impl MtuSink {
    fn new(label: &'static str, mtu: u16, log: Arc<Mutex<Vec<SinkEvent>>>) -> Self {
        Self { mtu, log, label }
    }
}

impl InterfaceSink for MtuSink {
    fn mtu(&self) -> u16 {
        self.mtu
    }
    fn send_ty<T: Serialize>(&mut self, hdr: &HeaderSeq, _body: &T) -> Result<(), ()> {
        self.log.lock().unwrap().push(SinkEvent::SendTy {
            label: self.label,
            dst: hdr.dst,
        });
        Ok(())
    }
    fn send_raw(&mut self, hdr: &HeaderSeq, _body: &[u8]) -> Result<(), ()> {
        self.log.lock().unwrap().push(SinkEvent::SendRaw {
            label: self.label,
            dst: hdr.dst,
        });
        Ok(())
    }
    fn send_err(&mut self, hdr: &HeaderSeq, err: ProtocolError) -> Result<(), ()> {
        self.log.lock().unwrap().push(SinkEvent::SendErr {
            label: self.label,
            dst: hdr.dst,
            err,
        });
        Ok(())
    }
}

struct MockInterface;
impl Interface for MockInterface {
    type Sink = MtuSink;
}

// --- Wire format round-trip tests ---

#[test]
fn error_frame_round_trip_simple() {
    // Encode a simple error (no payload) and decode it
    let hdr = HeaderSeq {
        src: Address {
            network_id: 1,
            node_id: 1,
            port_id: 10,
        },
        dst: Address {
            network_id: 2,
            node_id: 2,
            port_id: 20,
        },
        any_all: None,
        seq_no: 42,
        kind: FrameKind::PROTOCOL_ERROR,
        ttl: 8,
    };

    let flav = postcard::ser_flavors::StdVec::new();
    let encoded = encode_frame_err(flav, &hdr, ProtocolError::IseNoRouteToDest).unwrap();

    let frame = de_frame(&encoded).expect("should decode");
    assert_eq!(frame.hdr.src, hdr.src);
    assert_eq!(frame.hdr.dst, hdr.dst);
    assert_eq!(frame.hdr.seq_no, 42);
    assert_eq!(frame.hdr.kind, FrameKind::PROTOCOL_ERROR);
    assert_eq!(frame.body, Err(ProtocolError::IseNoRouteToDest));
}

#[test]
fn error_frame_round_trip_packet_too_big() {
    // Encode a PacketTooBig error WITH the MTU payload and decode it
    let hdr = HeaderSeq {
        src: Address {
            network_id: 1,
            node_id: 1,
            port_id: 10,
        },
        dst: Address {
            network_id: 2,
            node_id: 2,
            port_id: 20,
        },
        any_all: None,
        seq_no: 99,
        kind: FrameKind::PROTOCOL_ERROR,
        ttl: 8,
    };

    let err = ProtocolError::IsePacketTooBig { mtu: 512 };
    let flav = postcard::ser_flavors::StdVec::new();
    let encoded = encode_frame_err(flav, &hdr, err).unwrap();

    let frame = de_frame(&encoded).expect("should decode");
    assert_eq!(frame.hdr.seq_no, 99);
    assert_eq!(frame.body, Err(ProtocolError::IsePacketTooBig { mtu: 512 }));
}

#[test]
fn error_frame_round_trip_max_mtu() {
    // Ensure maximum u16 MTU value survives the round-trip
    let hdr = HeaderSeq {
        src: Address {
            network_id: 1,
            node_id: 1,
            port_id: 10,
        },
        dst: Address {
            network_id: 1,
            node_id: 2,
            port_id: 20,
        },
        any_all: None,
        seq_no: 0,
        kind: FrameKind::PROTOCOL_ERROR,
        ttl: 1,
    };

    let err = ProtocolError::IsePacketTooBig { mtu: u16::MAX };
    let flav = postcard::ser_flavors::StdVec::new();
    let encoded = encode_frame_err(flav, &hdr, err).unwrap();

    let frame = de_frame(&encoded).expect("should decode");
    assert_eq!(
        frame.body,
        Err(ProtocolError::IsePacketTooBig { mtu: u16::MAX })
    );
}

// --- Router PacketTooBig e2e test ---

#[test]
fn router_send_raw_packet_too_big() {
    // Set up a router with two interfaces: one with large MTU, one with small MTU.
    // Send a raw frame from the large-MTU interface destined for the small-MTU one.
    // The router should return PacketTooBig with the small interface's MTU.
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: Router<MockInterface, MockRng, 4, 8> = Router::new(MockRng(0));

    // Interface 0: large MTU (source)
    let _id_large = router
        .register_interface(MtuSink::new("large", 2048, log.clone()))
        .unwrap();
    // Interface 1: small MTU (destination) — 64 bytes
    let _id_small = router
        .register_interface(MtuSink::new("small", 64, log.clone()))
        .unwrap();

    // Build a raw frame that's larger than the small interface's MTU.
    // Interface 0 has net_id=1, interface 1 has net_id=2.
    // Source is on net_id=1, destination is on net_id=2.
    let hdr = HeaderSeq {
        src: Address {
            network_id: 1,
            node_id: 2,
            port_id: 10,
        },
        dst: Address {
            network_id: 2,
            node_id: 2,
            port_id: 20,
        },
        any_all: None,
        seq_no: 1,
        kind: FrameKind::ENDPOINT_REQ,
        ttl: 16,
    };

    // 100 bytes of payload — well over the 64 byte MTU
    let big_payload = [0xABu8; 100];

    let result = router.send_raw(&hdr, &big_payload, _id_large);
    assert_eq!(result, Err(InterfaceSendError::PacketTooBig { mtu: 64 }));
}

#[test]
fn router_send_raw_within_mtu_succeeds() {
    // Verify that sending a frame within MTU works fine
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: Router<MockInterface, MockRng, 4, 8> = Router::new(MockRng(0));

    let id0 = router
        .register_interface(MtuSink::new("src", 2048, log.clone()))
        .unwrap();
    let _id1 = router
        .register_interface(MtuSink::new("dst", 2048, log.clone()))
        .unwrap();

    let hdr = HeaderSeq {
        src: Address {
            network_id: 1,
            node_id: 2,
            port_id: 10,
        },
        dst: Address {
            network_id: 2,
            node_id: 2,
            port_id: 20,
        },
        any_all: None,
        seq_no: 1,
        kind: FrameKind::ENDPOINT_REQ,
        ttl: 16,
    };

    let small_payload = [0xABu8; 10];
    let result = router.send_raw(&hdr, &small_payload, id0);
    assert!(result.is_ok());

    let events = log.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        SinkEvent::SendRaw { label: "dst", .. }
    ));
}
