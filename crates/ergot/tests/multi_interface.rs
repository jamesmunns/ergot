//! Tests for the multi_interface! macro

use ergot::interface_manager::{Interface, InterfaceSink, LinkDst, LinkMeta};
use ergot::multi_interface;
use ergot::{Header, ProtocolError};
use serde::Serialize;
use std::sync::atomic::{AtomicU8, Ordering};

// --- Mock sinks and interfaces for testing ---

static LAST_SINK: AtomicU8 = AtomicU8::new(0);
/// `src_node` of the last `LinkMeta` a mock sink saw.
static LAST_LINK_SRC: AtomicU8 = AtomicU8::new(0);

fn record(sink: u8, link: &LinkMeta) -> Result<(), ()> {
    LAST_SINK.store(sink, Ordering::SeqCst);
    LAST_LINK_SRC.store(link.src_node, Ordering::SeqCst);
    Ok(())
}

struct MockSinkA;

impl InterfaceSink for MockSinkA {
    fn mtu(&self) -> u16 {
        2048
    }
    fn send_ty<T: Serialize>(&mut self, link: &LinkMeta, _: &Header, _: &T) -> Result<(), ()> {
        record(1, link)
    }
    fn send_raw(&mut self, link: &LinkMeta, _: &Header, _: &[u8]) -> Result<(), ()> {
        record(1, link)
    }
    fn send_err(&mut self, link: &LinkMeta, _: &Header, _: ProtocolError) -> Result<(), ()> {
        record(1, link)
    }
}

struct MockSinkB;

impl InterfaceSink for MockSinkB {
    fn mtu(&self) -> u16 {
        2048
    }
    fn send_ty<T: Serialize>(&mut self, link: &LinkMeta, _: &Header, _: &T) -> Result<(), ()> {
        record(2, link)
    }
    fn send_raw(&mut self, link: &LinkMeta, _: &Header, _: &[u8]) -> Result<(), ()> {
        record(2, link)
    }
    fn send_err(&mut self, link: &LinkMeta, _: &Header, _: ProtocolError) -> Result<(), ()> {
        record(2, link)
    }
}

struct MockSinkC;

impl InterfaceSink for MockSinkC {
    fn mtu(&self) -> u16 {
        2048
    }
    fn send_ty<T: Serialize>(&mut self, link: &LinkMeta, _: &Header, _: &T) -> Result<(), ()> {
        record(3, link)
    }
    fn send_raw(&mut self, link: &LinkMeta, _: &Header, _: &[u8]) -> Result<(), ()> {
        record(3, link)
    }
    fn send_err(&mut self, link: &LinkMeta, _: &Header, _: ProtocolError) -> Result<(), ()> {
        record(3, link)
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

fn make_dummy_hdr() -> Header {
    Header {
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
        kind: ergot::FrameKind::ENDPOINT_REQ,
        class: ergot::TrafficClass::Normal,
        ttl: 15,
    }
}

fn link(src_node: u8) -> LinkMeta {
    LinkMeta {
        src_node,
        dst: LinkDst::Broadcast,
    }
}

#[test]
fn multi_interface_dispatches_to_correct_sink() {
    let hdr = make_dummy_hdr();

    let mut sink_a: TestSink = TestSink::A(MockSinkA);
    LAST_SINK.store(0, Ordering::SeqCst);
    sink_a.send_ty(&link(10), &hdr, &42u32).unwrap();
    assert_eq!(LAST_SINK.load(Ordering::SeqCst), 1);
    assert_eq!(LAST_LINK_SRC.load(Ordering::SeqCst), 10);

    let mut sink_b: TestSink = TestSink::B(MockSinkB);
    LAST_SINK.store(0, Ordering::SeqCst);
    sink_b.send_raw(&link(20), &hdr, &[1, 2, 3]).unwrap();
    assert_eq!(LAST_SINK.load(Ordering::SeqCst), 2);
    assert_eq!(LAST_LINK_SRC.load(Ordering::SeqCst), 20);

    let mut sink_c: TestSink = TestSink::C(MockSinkC);
    LAST_SINK.store(0, Ordering::SeqCst);
    sink_c
        .send_err(&link(30), &hdr, ProtocolError::Reserved)
        .unwrap();
    assert_eq!(LAST_SINK.load(Ordering::SeqCst), 3);
    assert_eq!(LAST_LINK_SRC.load(Ordering::SeqCst), 30);
}

#[test]
fn multi_interface_struct_implements_interface() {
    // Verify the generated struct implements Interface with correct Sink type
    fn assert_interface<I: Interface<Sink = TestSink>>() {}
    assert_interface::<TestInterface>();
}
