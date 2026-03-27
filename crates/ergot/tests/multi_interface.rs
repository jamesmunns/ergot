//! Tests for the multi_interface! macro

use ergot::interface_manager::{Interface, InterfaceSink};
use ergot::multi_interface;
use ergot::{HeaderSeq, ProtocolError};
use serde::Serialize;
use std::sync::atomic::{AtomicU8, Ordering};

// --- Mock sinks and interfaces for testing ---

static LAST_SINK: AtomicU8 = AtomicU8::new(0);

struct MockSinkA;

impl InterfaceSink for MockSinkA {
    fn mtu(&self) -> u16 {
        2048
    }
    fn send_ty<T: Serialize>(&mut self, _hdr: &HeaderSeq, _body: &T) -> Result<(), ()> {
        LAST_SINK.store(1, Ordering::SeqCst);
        Ok(())
    }
    fn send_raw(&mut self, _hdr: &HeaderSeq, _body: &[u8]) -> Result<(), ()> {
        LAST_SINK.store(1, Ordering::SeqCst);
        Ok(())
    }
    fn send_err(&mut self, _hdr: &HeaderSeq, _err: ProtocolError) -> Result<(), ()> {
        LAST_SINK.store(1, Ordering::SeqCst);
        Ok(())
    }
}

struct MockSinkB;

impl InterfaceSink for MockSinkB {
    fn mtu(&self) -> u16 {
        2048
    }
    fn send_ty<T: Serialize>(&mut self, _hdr: &HeaderSeq, _body: &T) -> Result<(), ()> {
        LAST_SINK.store(2, Ordering::SeqCst);
        Ok(())
    }
    fn send_raw(&mut self, _hdr: &HeaderSeq, _body: &[u8]) -> Result<(), ()> {
        LAST_SINK.store(2, Ordering::SeqCst);
        Ok(())
    }
    fn send_err(&mut self, _hdr: &HeaderSeq, _err: ProtocolError) -> Result<(), ()> {
        LAST_SINK.store(2, Ordering::SeqCst);
        Ok(())
    }
}

struct MockSinkC;

impl InterfaceSink for MockSinkC {
    fn mtu(&self) -> u16 {
        2048
    }
    fn send_ty<T: Serialize>(&mut self, _hdr: &HeaderSeq, _body: &T) -> Result<(), ()> {
        LAST_SINK.store(3, Ordering::SeqCst);
        Ok(())
    }
    fn send_raw(&mut self, _hdr: &HeaderSeq, _body: &[u8]) -> Result<(), ()> {
        LAST_SINK.store(3, Ordering::SeqCst);
        Ok(())
    }
    fn send_err(&mut self, _hdr: &HeaderSeq, _err: ProtocolError) -> Result<(), ()> {
        LAST_SINK.store(3, Ordering::SeqCst);
        Ok(())
    }
}

struct IfaceA;
impl Interface for IfaceA {
    type Sink = MockSinkA;
}

struct IfaceB;
impl Interface for IfaceB {
    type Sink = MockSinkB;
}

struct IfaceC;
impl Interface for IfaceC {
    type Sink = MockSinkC;
}

// --- Generate combined interface ---

multi_interface! {
    pub enum TestSink for TestInterface {
        A(IfaceA),
        B(IfaceB),
        C(IfaceC),
    }
}

fn make_dummy_hdr() -> HeaderSeq {
    HeaderSeq {
        src: ergot::Address {
            network_id: 1,
            node_id: 1,
            port_id: 1,
        },
        dst: ergot::Address {
            network_id: 2,
            node_id: 2,
            port_id: 2,
        },
        any_all: None,
        seq_no: 0,
        kind: ergot::FrameKind::ENDPOINT_REQ,
        ttl: 16,
    }
}

#[test]
fn multi_interface_dispatches_to_correct_sink() {
    let hdr = make_dummy_hdr();

    let mut sink_a: TestSink = TestSink::A(MockSinkA);
    LAST_SINK.store(0, Ordering::SeqCst);
    sink_a.send_ty(&hdr, &42u32).unwrap();
    assert_eq!(LAST_SINK.load(Ordering::SeqCst), 1);

    let mut sink_b: TestSink = TestSink::B(MockSinkB);
    LAST_SINK.store(0, Ordering::SeqCst);
    sink_b.send_raw(&hdr, &[1, 2, 3]).unwrap();
    assert_eq!(LAST_SINK.load(Ordering::SeqCst), 2);

    let mut sink_c: TestSink = TestSink::C(MockSinkC);
    LAST_SINK.store(0, Ordering::SeqCst);
    sink_c.send_err(&hdr, ProtocolError::Reserved).unwrap();
    assert_eq!(LAST_SINK.load(Ordering::SeqCst), 3);
}

#[test]
fn multi_interface_struct_implements_interface() {
    // Verify the generated struct implements Interface with correct Sink type
    fn assert_interface<I: Interface<Sink = TestSink>>() {}
    assert_interface::<TestInterface>();
}
