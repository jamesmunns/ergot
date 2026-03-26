//! Unit tests for Router in bridge mode (with upstream).
//!
//! Uses RecordingSink to verify routing decisions without real transports.

#![cfg(feature = "tokio-std")]

use ergot::interface_manager::{
    Interface, InterfaceSendError, InterfaceSink, InterfaceState, Profile,
    profiles::router::{Router, UPSTREAM_IDENT},
};
use ergot::{Address, AnyAllAppendix, FrameKind, Header, HeaderSeq, Key, ProtocolError};
use rand::SeedableRng;
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

type TestRouter = Router<MockInterface, rand::rngs::StdRng, 4, 8>;

fn make_bridge(log: &Arc<Mutex<Vec<String>>>) -> TestRouter {
    Router::new_bridge(
        rand::rngs::StdRng::from_seed([0u8; 32]),
        RecordingSink::new("upstream", log.clone()),
    )
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

// --- Bridge construction ---

#[test]
fn bridge_has_upstream() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let router = make_bridge(&log);
    assert!(router.has_upstream());
}

#[test]
fn root_has_no_upstream() {
    let router: TestRouter = Router::new(rand::rngs::StdRng::from_seed([0u8; 32]));
    assert!(!router.has_upstream());
}

#[test]
fn bridge_upstream_starts_down() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router = make_bridge(&log);
    let state = router.interface_state(UPSTREAM_IDENT);
    assert!(matches!(state, Some(InterfaceState::Down)));
}

#[test]
fn bridge_set_upstream_state() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router = make_bridge(&log);

    // Transition to Inactive then Active
    router
        .set_interface_state(UPSTREAM_IDENT, InterfaceState::Inactive)
        .unwrap();
    assert!(matches!(
        router.interface_state(UPSTREAM_IDENT),
        Some(InterfaceState::Inactive)
    ));

    router
        .set_interface_state(
            UPSTREAM_IDENT,
            InterfaceState::Active {
                net_id: 5,
                node_id: 2,
            },
        )
        .unwrap();
    assert!(matches!(
        router.interface_state(UPSTREAM_IDENT),
        Some(InterfaceState::Active { net_id: 5, .. })
    ));
}

// --- Unicast routing ---

#[test]
fn bridge_routes_known_downstream() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router = make_bridge(&log);

    // Activate upstream so sends work
    router
        .set_interface_state(
            UPSTREAM_IDENT,
            InterfaceState::Active {
                net_id: 10,
                node_id: 2,
            },
        )
        .unwrap();

    // Register downstream with net_id=1
    router
        .register_interface(RecordingSink::new("down0", log.clone()))
        .unwrap();

    // Send to net_id=1, node=2 — should go to downstream, NOT upstream
    let hdr = make_hdr(0, 1, 2, 5);
    router.send(&hdr, &42u32).unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].starts_with("down0:"),
        "expected downstream, got: {}",
        entries[0]
    );
}

#[test]
fn bridge_forwards_unknown_to_upstream() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router = make_bridge(&log);

    // Activate upstream
    router
        .set_interface_state(
            UPSTREAM_IDENT,
            InterfaceState::Active {
                net_id: 10,
                node_id: 2,
            },
        )
        .unwrap();

    // Register downstream
    router
        .register_interface(RecordingSink::new("down0", log.clone()))
        .unwrap();

    // Send to net_id=99 — unknown downstream, should go upstream
    let hdr = make_hdr(0, 99, 2, 5);
    router.send(&hdr, &42u32).unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].starts_with("upstream:"),
        "expected upstream, got: {}",
        entries[0]
    );
}

#[test]
fn root_router_no_upstream_fallback() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router: TestRouter = Router::new(rand::rngs::StdRng::from_seed([0u8; 32]));

    router
        .register_interface(RecordingSink::new("down0", log.clone()))
        .unwrap();

    // Send to unknown net_id — should fail (no upstream)
    let hdr = make_hdr(0, 99, 2, 5);
    let result = router.send(&hdr, &42u32);
    assert_eq!(result, Err(InterfaceSendError::NoRouteToDest));
}

// --- Broadcast ---

#[test]
fn bridge_broadcast_includes_upstream() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router = make_bridge(&log);

    router
        .set_interface_state(
            UPSTREAM_IDENT,
            InterfaceState::Active {
                net_id: 10,
                node_id: 2,
            },
        )
        .unwrap();

    router
        .register_interface(RecordingSink::new("down0", log.clone()))
        .unwrap();
    router
        .register_interface(RecordingSink::new("down1", log.clone()))
        .unwrap();

    let hdr = make_broadcast_hdr();
    router.send(&hdr, &42u32).unwrap();

    let entries = log.lock().unwrap();
    // Should go to down0 + down1 + upstream = 3
    assert_eq!(entries.len(), 3, "entries: {:?}", *entries);

    let labels: Vec<&str> = entries
        .iter()
        .map(|e| e.split(':').next().unwrap())
        .collect();
    assert!(labels.contains(&"down0"));
    assert!(labels.contains(&"down1"));
    assert!(labels.contains(&"upstream"));
}

#[test]
fn bridge_broadcast_from_upstream_skips_upstream() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router = make_bridge(&log);

    router
        .set_interface_state(
            UPSTREAM_IDENT,
            InterfaceState::Active {
                net_id: 10,
                node_id: 2,
            },
        )
        .unwrap();

    router
        .register_interface(RecordingSink::new("down0", log.clone()))
        .unwrap();
    router
        .register_interface(RecordingSink::new("down1", log.clone()))
        .unwrap();

    // Raw broadcast from upstream (source=UPSTREAM_IDENT)
    let hdr = HeaderSeq {
        src: Address {
            network_id: 10,
            node_id: 1,
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
        seq_no: 0,
        kind: FrameKind::TOPIC_MSG,
        ttl: 16,
    };

    router.send_raw(&hdr, &[1, 2, 3], UPSTREAM_IDENT).unwrap();

    let entries = log.lock().unwrap();
    // Should go to down0 + down1 only, NOT back to upstream
    assert_eq!(entries.len(), 2, "entries: {:?}", *entries);

    let labels: Vec<&str> = entries
        .iter()
        .map(|e| e.split(':').next().unwrap())
        .collect();
    assert!(labels.contains(&"down0"));
    assert!(labels.contains(&"down1"));
    assert!(!labels.contains(&"upstream"));
}

// --- Routing loop ---

#[test]
fn bridge_raw_from_upstream_no_loop() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router = make_bridge(&log);

    router
        .set_interface_state(
            UPSTREAM_IDENT,
            InterfaceState::Active {
                net_id: 10,
                node_id: 2,
            },
        )
        .unwrap();

    // Raw unicast from upstream to unknown net_id — should NOT go back upstream
    let hdr = HeaderSeq {
        src: Address {
            network_id: 10,
            node_id: 1,
            port_id: 0,
        },
        dst: Address {
            network_id: 99,
            node_id: 2,
            port_id: 5,
        },
        any_all: None,
        seq_no: 0,
        kind: FrameKind::ENDPOINT_REQ,
        ttl: 16,
    };

    let result = router.send_raw(&hdr, &[1, 2, 3], UPSTREAM_IDENT);
    assert_eq!(result, Err(InterfaceSendError::RoutingLoop));
}

// --- Downstream to downstream stays local ---

#[test]
fn bridge_downstream_to_downstream_no_upstream() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router = make_bridge(&log);

    router
        .set_interface_state(
            UPSTREAM_IDENT,
            InterfaceState::Active {
                net_id: 10,
                node_id: 2,
            },
        )
        .unwrap();

    let id0 = router
        .register_interface(RecordingSink::new("down0", log.clone()))
        .unwrap();
    // net_id=2
    router
        .register_interface(RecordingSink::new("down1", log.clone()))
        .unwrap();

    // Raw packet from down0 destined to net_id=2 (down1)
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

    router.send_raw(&hdr, &[1, 2, 3], id0).unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].starts_with("down1:"),
        "should route to down1, not upstream. got: {}",
        entries[0]
    );
}

// --- send_err through upstream ---

#[test]
fn bridge_send_err_forwards_upstream() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut router = make_bridge(&log);

    router
        .set_interface_state(
            UPSTREAM_IDENT,
            InterfaceState::Active {
                net_id: 10,
                node_id: 2,
            },
        )
        .unwrap();

    router
        .register_interface(RecordingSink::new("down0", log.clone()))
        .unwrap();

    // Error to unknown net_id — should go upstream
    let hdr = Header {
        src: Address {
            network_id: 0,
            node_id: 0,
            port_id: 1,
        },
        dst: Address {
            network_id: 99,
            node_id: 2,
            port_id: 5,
        },
        any_all: None,
        seq_no: None,
        kind: FrameKind::PROTOCOL_ERROR,
        ttl: 16,
    };

    router
        .send_err(&hdr, ProtocolError::NSSE_NO_ROUTE, None)
        .unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].starts_with("upstream:"));
}
