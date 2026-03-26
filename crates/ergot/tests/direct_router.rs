//! Tests for DirectRouter profile.
//!
//! Mirrors the structure of no_std_router.rs but for the std DirectRouter.

#![cfg(feature = "tokio-std")]

use ergot::interface_manager::{
    Interface, InterfaceSendError, InterfaceSink, InterfaceState, Profile, SeedAssignmentError,
    SeedRefreshError, profiles::direct_router::DirectRouter,
};
use ergot::{Address, AnyAllAppendix, FrameKind, Header, HeaderSeq, Key, ProtocolError};
use serde::Serialize;
use std::sync::{Arc, Mutex};

// --- Mock sink that records sends ---

#[derive(Clone)]
struct RecordingSink {
    log: Arc<Mutex<Vec<String>>>,
    label: &'static str,
}

impl RecordingSink {
    fn new(label: &'static str, log: Arc<Mutex<Vec<String>>>) -> Self {
        Self { log, label }
    }
}

impl InterfaceSink for RecordingSink {
    fn send_ty<T: Serialize>(&mut self, hdr: &HeaderSeq, _body: &T) -> Result<(), ()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:send_ty:{}", self.label, hdr.dst));
        Ok(())
    }
    fn send_raw(&mut self, hdr: &HeaderSeq, _body: &[u8]) -> Result<(), ()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:send_raw:{}", self.label, hdr.dst));
        Ok(())
    }
    fn send_err(&mut self, hdr: &HeaderSeq, _err: ProtocolError) -> Result<(), ()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:send_err:{}", self.label, hdr.dst));
        Ok(())
    }
}

struct MockInterface;
impl Interface for MockInterface {
    type Sink = RecordingSink;
}

fn make_hdr(src_net: u16, dst_net: u16, dst_node: u8, dst_port: u8) -> Header {
    Header {
        src: Address {
            network_id: src_net,
            node_id: 0,
            port_id: 0,
        },
        dst: Address {
            network_id: dst_net,
            node_id: dst_node,
            port_id: dst_port,
        },
        any_all: None,
        seq_no: None,
        kind: FrameKind::ENDPOINT_REQ,
        ttl: 16,
    }
}

fn make_broadcast_hdr() -> Header {
    Header {
        src: Address {
            network_id: 0,
            node_id: 0,
            port_id: 0,
        },
        dst: Address {
            network_id: 0,
            node_id: 0,
            port_id: 255,
        },
        any_all: Some(AnyAllAppendix {
            key: Key(*b"TESTTEST"),
            nash: None,
        }),
        seq_no: None,
        kind: FrameKind::TOPIC_MSG,
        ttl: 16,
    }
}

// --- Registration tests ---

#[test]
fn register_and_deregister() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    let id0 = router
        .register_interface(RecordingSink::new("usb", log.clone()))
        .unwrap();
    let id1 = router
        .register_interface(RecordingSink::new("uart", log.clone()))
        .unwrap();

    // Both are Active with sequential net_ids
    assert!(matches!(
        router.interface_state(id0),
        Some(InterfaceState::Active { net_id: 1, .. })
    ));
    assert!(matches!(
        router.interface_state(id1),
        Some(InterfaceState::Active { net_id: 2, .. })
    ));

    // Deregister first
    router.deregister_interface(id0).unwrap();
    assert_eq!(router.interface_state(id0), None);

    // Second still works
    assert!(matches!(
        router.interface_state(id1),
        Some(InterfaceState::Active { net_id: 2, .. })
    ));
}

// --- Routing tests ---

#[test]
fn send_unicast_routes_to_correct_interface() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    // net_id=1
    router
        .register_interface(RecordingSink::new("usb", log.clone()))
        .unwrap();
    // net_id=2
    router
        .register_interface(RecordingSink::new("uart", log.clone()))
        .unwrap();

    // Send to net_id=2, node=2 (EDGE), port=5
    let hdr = make_hdr(0, 2, 2, 5);
    router.send(&hdr, &42u32).unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].starts_with("uart:send_ty:"));
}

#[test]
fn send_unicast_destination_local() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    // net_id=1
    router
        .register_interface(RecordingSink::new("usb", log.clone()))
        .unwrap();

    // Send to net_id=1, node=1 (CENTRAL = us)
    let hdr = make_hdr(0, 1, 1, 5);
    let result = router.send(&hdr, &42u32);
    assert_eq!(result, Err(InterfaceSendError::DestinationLocal));
}

#[test]
fn send_unicast_no_route() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    router
        .register_interface(RecordingSink::new("usb", log.clone()))
        .unwrap();

    // Send to net_id=99 — doesn't exist
    let hdr = make_hdr(0, 99, 2, 5);
    let result = router.send(&hdr, &42u32);
    assert_eq!(result, Err(InterfaceSendError::NoRouteToDest));
}

#[test]
fn send_broadcast_goes_to_all() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    router
        .register_interface(RecordingSink::new("usb", log.clone()))
        .unwrap();
    router
        .register_interface(RecordingSink::new("uart", log.clone()))
        .unwrap();
    router
        .register_interface(RecordingSink::new("radio", log.clone()))
        .unwrap();

    let hdr = make_broadcast_hdr();
    router.send(&hdr, &42u32).unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 3);

    let labels: Vec<&str> = entries
        .iter()
        .map(|e| e.split(':').next().unwrap())
        .collect();
    assert!(labels.contains(&"usb"));
    assert!(labels.contains(&"uart"));
    assert!(labels.contains(&"radio"));
}

#[test]
fn send_raw_routing_loop() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    let id_usb = router
        .register_interface(RecordingSink::new("usb", log.clone()))
        .unwrap();

    // Raw packet from USB destined to net_id=1 (same interface)
    let hdr = HeaderSeq {
        src: Address {
            network_id: 1,
            node_id: 2,
            port_id: 3,
        },
        dst: Address {
            network_id: 1,
            node_id: 2,
            port_id: 5,
        },
        any_all: None,
        seq_no: 100,
        kind: FrameKind::ENDPOINT_REQ,
        ttl: 16,
    };

    let result = router.send_raw(&hdr, &[1, 2, 3], id_usb);
    assert_eq!(result, Err(InterfaceSendError::RoutingLoop));
}

#[test]
fn send_raw_forwards_to_other_interface() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    let id_usb = router
        .register_interface(RecordingSink::new("usb", log.clone()))
        .unwrap();
    // net_id=2
    router
        .register_interface(RecordingSink::new("uart", log.clone()))
        .unwrap();

    let hdr = HeaderSeq {
        src: Address {
            network_id: 1,
            node_id: 2,
            port_id: 3,
        },
        dst: Address {
            network_id: 2,
            node_id: 2,
            port_id: 5,
        },
        any_all: None,
        seq_no: 100,
        kind: FrameKind::ENDPOINT_REQ,
        ttl: 16,
    };

    router.send_raw(&hdr, &[1, 2, 3], id_usb).unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].starts_with("uart:send_raw:"));
}

// --- Seed router tests ---

#[test]
fn seed_assign_success() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    router
        .register_interface(RecordingSink::new("uart", log.clone()))
        .unwrap();

    let assignment = router.request_seed_net_assign(1).unwrap();
    assert_eq!(assignment.net_id, 2);
    assert_eq!(assignment.expires_seconds, 30);
    assert_ne!(assignment.refresh_token, [0; 8]);
}

#[test]
fn seed_assign_unknown_source() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    router
        .register_interface(RecordingSink::new("uart", log.clone()))
        .unwrap();

    let result = router.request_seed_net_assign(99);
    assert_eq!(result, Err(SeedAssignmentError::UnknownSource));
}

#[test]
fn seed_refresh_success() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    router
        .register_interface(RecordingSink::new("uart", log.clone()))
        .unwrap();

    let assignment = router.request_seed_net_assign(1).unwrap();

    // Initial lease is 30s < MIN_SEED_REFRESH (62s) → refresh allowed immediately
    let refreshed = router
        .refresh_seed_net_assignment(1, assignment.net_id, assignment.refresh_token)
        .unwrap();

    assert_eq!(refreshed.net_id, assignment.net_id);
    assert_eq!(refreshed.expires_seconds, 120);
}

#[test]
fn seed_refresh_then_too_soon() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    router
        .register_interface(RecordingSink::new("uart", log.clone()))
        .unwrap();

    let assignment = router.request_seed_net_assign(1).unwrap();
    let refreshed = router
        .refresh_seed_net_assignment(1, assignment.net_id, assignment.refresh_token)
        .unwrap();

    // Second refresh immediately: remaining ~120s > 62s → TooSoon
    let result = router.refresh_seed_net_assignment(1, refreshed.net_id, refreshed.refresh_token);
    assert_eq!(result, Err(SeedRefreshError::TooSoon));
}

#[test]
fn seed_refresh_bad_token() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    router
        .register_interface(RecordingSink::new("uart", log.clone()))
        .unwrap();

    let assignment = router.request_seed_net_assign(1).unwrap();

    let result = router.refresh_seed_net_assignment(1, assignment.net_id, [0xFF; 8]);
    assert_eq!(result, Err(SeedRefreshError::BadRequest));
}

#[test]
fn deregister_cleans_routes() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    let id_uart = router
        .register_interface(RecordingSink::new("uart", log.clone()))
        .unwrap();

    // Seed route through uart
    let assignment = router.request_seed_net_assign(1).unwrap();
    let seed_net = assignment.net_id;

    // Verify routing works
    let hdr = make_hdr(0, seed_net, 2, 5);
    router.send(&hdr, &42u32).unwrap();
    assert_eq!(log.lock().unwrap().len(), 1);

    // Deregister — should tombstone all routes
    router.deregister_interface(id_uart).unwrap();

    log.lock().unwrap().clear();
    let hdr = make_hdr(0, seed_net, 2, 5);
    let result = router.send(&hdr, &42u32);
    assert_eq!(result, Err(InterfaceSendError::NoRouteToDest));
}

#[test]
fn get_nets_returns_active_interfaces() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: DirectRouter<MockInterface> = DirectRouter::new_std();

    router
        .register_interface(RecordingSink::new("a", log.clone()))
        .unwrap();
    router
        .register_interface(RecordingSink::new("b", log.clone()))
        .unwrap();

    let mut nets = router.get_nets();
    nets.sort();
    assert_eq!(nets, vec![1, 2]);
}
