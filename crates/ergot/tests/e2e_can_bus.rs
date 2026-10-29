//! E2E test: the CAN transport on a simulated shared bus.
//!
//! Topology (one classic-CAN segment, 8-byte frames):
//! ```text
//!           ┌──────── CAN bus (net_id=N) ────────┐
//!     Root Router                          Edge A, Edge B
//!       (N.1)                             (N.30)  (N.50)
//! ```
//!
//! The edges claim node ids through the bus address claim, then the router
//! exchanges endpoint traffic that does not fit a single 8-byte frame (so it
//! is fragmented and reassembled through the real sink/worker pair) and
//! broadcasts a fragmented topic that every edge must reassemble.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

mod common;

use std::{pin::pin, time::Duration};

use bbqueue::traits::bbqhdl::BbqHandle;
use common::{Bus, BusReceiver, BusSender};
use ergot::{
    Address, endpoint,
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::can::CanInterface,
        profiles::{
            direct_edge::{DirectEdge, EdgeFrameProcessor},
            router::{Router, RouterFrameProcessor},
        },
        transports::can::{CanConfig, CanRx, CanRxTxWorker, CanTx},
        utils::{
            can::{CanFrame, ENCODED_ID_LEN, FD_MAX_PAYLOAD, Sink},
            std::{StdQueue, new_std_queue},
        },
    },
    net_stack::ArcNetStack,
    topic,
    well_known::{AddressClaimRequest, ErgotAddressClaimEndpoint},
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};
use tokio::time::{sleep, timeout};

const MTU: usize = 256;
const CLASSIC: u8 = 8;

type CanIf = CanInterface<StdQueue, MTU>;
type BusRouterStack =
    ArcNetStack<CriticalSectionRawMutex, Router<CanIf, rand::rngs::StdRng, 4, 4, 16>>;
type BusEdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<CanIf>>;

/// 48 bytes of body: several classic frames per message in each direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Schema)]
pub struct Big {
    words: [u32; 12],
}

endpoint!(BigEndpoint, Big, Big, "test/can/big");
topic!(BigTopic, Big, "test/can/big-topic");

// ---- A CAN link over the shared Bus mock ----

struct MemCanTx(BusSender);
struct MemCanRx(BusReceiver);

fn mem_link(bus: &Bus) -> (MemCanRx, MemCanTx) {
    let (tx, rx) = bus.tap().split();
    (MemCanRx(rx), MemCanTx(tx))
}

impl CanTx for MemCanTx {
    type Error = ();

    fn max_payload(&self) -> u8 {
        CLASSIC
    }

    async fn send(&mut self, frame: &CanFrame) -> Result<(), ()> {
        assert!(
            frame.len as usize <= CLASSIC as usize,
            "classic frame exceeded 8 bytes"
        );
        let mut buf = [0u8; ENCODED_ID_LEN + FD_MAX_PAYLOAD];
        let n = frame.to_bytes(&mut buf).ok_or(())?;
        self.0.send(&buf[..n]);
        Ok(())
    }
}

impl CanRx for MemCanRx {
    type Error = ();

    async fn recv(&mut self) -> Result<CanFrame, ()> {
        loop {
            let bytes = self.0.recv().await.ok_or(())?;
            if let Some(f) = CanFrame::from_bytes(&bytes) {
                return Ok(f);
            }
        }
    }
}

struct Queues {
    control: StdQueue,
    bulk: StdQueue,
}

fn queues() -> Queues {
    Queues {
        control: new_std_queue(4096),
        bulk: new_std_queue(4096),
    }
}

fn make_edge(q: &Queues) -> BusEdgeStack {
    BusEdgeStack::new_with_profile(DirectEdge::new_target(Sink::new(
        q.control.clone(),
        q.bulk.clone(),
        CLASSIC,
    )))
}

fn spawn_router_worker(bus: &Bus, stack: &BusRouterStack, ident: u8, net_id: u16, q: &Queues) {
    let stack = stack.clone();
    let (rx, tx) = mem_link(bus);
    let (ctl, blk) = (
        BbqHandle::framed_consumer(&q.control),
        BbqHandle::framed_consumer(&q.bulk),
    );
    let initial = stack
        .manage_profile(|im| im.interface_state(ident))
        .expect("registered interface has a state");
    tokio::spawn(async move {
        let mut worker = CanRxTxWorker::<_, _, _, StdQueue, _, 4, MTU>::new(
            stack,
            rx,
            tx,
            RouterFrameProcessor::new(net_id),
            ident,
            ctl,
            blk,
            CanConfig::new(0x10),
        );
        let _ = worker.run(initial).await;
    });
}

fn spawn_edge_worker(bus: &Bus, stack: &BusEdgeStack, candidate: u8, q: &Queues) {
    let stack = stack.clone();
    let (rx, tx) = mem_link(bus);
    let (ctl, blk) = (
        BbqHandle::framed_consumer(&q.control),
        BbqHandle::framed_consumer(&q.bulk),
    );
    tokio::spawn(async move {
        let mut worker = CanRxTxWorker::<_, _, _, StdQueue, _, 4, MTU>::new(
            stack,
            rx,
            tx,
            EdgeFrameProcessor::new(),
            (),
            ctl,
            blk,
            CanConfig::new(candidate),
        );
        // Link-local with the device-chosen candidate node id until the claim
        // completes.
        let _ = worker.run(InterfaceState::link_local(candidate)).await;
    });
}

fn spawn_big_server(stack: &BusEdgeStack, tag: u32) {
    let stack = stack.clone();
    tokio::spawn(async move {
        let server = stack
            .endpoints()
            .bounded_server::<BigEndpoint, 4>(Some("big"));
        let server = pin!(server);
        let mut hdl = server.attach();
        loop {
            let _ = hdl
                .serve(|req: &Big| {
                    let mut words = req.words;
                    for w in words.iter_mut() {
                        *w ^= tag;
                    }
                    async move { Big { words } }
                })
                .await;
        }
    });
}

async fn claim(edge: &BusEdgeStack, candidate: u8, nonce: u64) -> u16 {
    let link_local_router = Address {
        network_id: 0,
        node_id: 1,
        port_id: 0,
    };
    let mut last = None;
    for _ in 0..20 {
        let res = timeout(
            Duration::from_millis(500),
            edge.endpoints().request::<ErgotAddressClaimEndpoint>(
                link_local_router,
                &AddressClaimRequest {
                    candidate_node_id: candidate,
                    nonce,
                },
                None,
            ),
        )
        .await;
        match res {
            Ok(Ok(Ok(granted))) => {
                assert_eq!(granted.assignment.node_id, candidate);
                edge.manage_profile(|im| {
                    im.set_interface_state(
                        (),
                        InterfaceState::Active {
                            net_id: granted.assignment.net_id,
                            node_id: granted.assignment.node_id,
                        },
                    )
                })
                .unwrap();
                return granted.assignment.net_id;
            }
            other => {
                last = Some(format!("{other:?}"));
                sleep(Duration::from_millis(50)).await;
            }
        }
    }
    panic!("claim for node {candidate} never granted: {last:?}");
}

async fn big_with_retry(stack: &BusRouterStack, addr: Address, req: &Big) -> Big {
    for _ in 0..30 {
        let res = timeout(
            Duration::from_millis(500),
            stack
                .endpoints()
                .request::<BigEndpoint>(addr, req, Some("big")),
        )
        .await;
        match res {
            Ok(Ok(v)) => return v,
            _ => sleep(Duration::from_millis(100)).await,
        }
    }
    panic!("fragmented request to {addr:?} failed after retries");
}

#[tokio::test]
async fn fragmented_endpoint_and_topic_traffic_over_classic_can() {
    let _ = env_logger::builder().is_test(true).try_init();
    let bus = Bus::new();

    // ---- Root router (bus arbiter) ----
    let rq = queues();
    let router: BusRouterStack = BusRouterStack::new_with_profile(Router::new_std());
    let router_ident = router
        .manage_profile(|im| {
            im.register_interface(Sink::new(rq.control.clone(), rq.bulk.clone(), CLASSIC))
        })
        .unwrap();
    let net_id = router
        .manage_profile(|im| im.net_id_of(router_ident))
        .unwrap();
    spawn_router_worker(&bus, &router, router_ident, net_id, &rq);
    tokio::spawn({
        let s = router.clone();
        async move { s.services().address_claim_handler::<4>().await }
    });

    // ---- Edges ----
    let aq = queues();
    let edge_a = make_edge(&aq);
    spawn_edge_worker(&bus, &edge_a, 30, &aq);
    spawn_big_server(&edge_a, 0xA0A0_A0A0);

    let bq = queues();
    let edge_b = make_edge(&bq);
    spawn_edge_worker(&bus, &edge_b, 50, &bq);
    spawn_big_server(&edge_b, 0x0B0B_0B0B);

    sleep(Duration::from_millis(50)).await;

    // Claims come from the link-local candidates. On classic CAN they are
    // fragmented too: the wildcard destination port carries the 13-byte
    // any/all appendix, so a claim request is ~27 bytes.
    assert_eq!(claim(&edge_a, 30, 0xAAAA).await, net_id);
    assert_eq!(claim(&edge_b, 50, 0xBBBB).await, net_id);

    // ---- Fragmented request/response to each edge ----
    let req = Big {
        words: core::array::from_fn(|i| 0x1000_0000 + i as u32 * 0x0101_0101),
    };
    let to = |node| Address {
        network_id: net_id,
        node_id: node,
        port_id: 0,
    };
    let ra = big_with_retry(&router, to(30), &req).await;
    assert!(
        ra.words
            .iter()
            .zip(req.words)
            .all(|(r, q)| *r == q ^ 0xA0A0_A0A0)
    );
    let rb = big_with_retry(&router, to(50), &req).await;
    assert!(
        rb.words
            .iter()
            .zip(req.words)
            .all(|(r, q)| *r == q ^ 0x0B0B_0B0B)
    );

    // ---- Fragmented broadcast topic reassembled by both edges ----
    let mut rx_a = pin!(edge_a.topics().bounded_receiver::<BigTopic, 4>(None));
    let mut rx_a = rx_a.as_mut().subscribe();
    let mut rx_b = pin!(edge_b.topics().bounded_receiver::<BigTopic, 4>(None));
    let mut rx_b = rx_b.as_mut().subscribe();

    let msg = Big {
        words: core::array::from_fn(|i| 0xBEEF_0000 + i as u32),
    };
    // Best-effort: repeat a few times, receivers dedupe by content.
    for _ in 0..3 {
        router.topics().broadcast::<BigTopic>(&msg, None).unwrap();
        sleep(Duration::from_millis(20)).await;
    }
    let got_a = timeout(Duration::from_secs(2), rx_a.recv())
        .await
        .expect("edge A never received the fragmented topic");
    let got_b = timeout(Duration::from_secs(2), rx_b.recv())
        .await
        .expect("edge B never received the fragmented topic");
    assert_eq!(got_a.t, msg);
    assert_eq!(got_b.t, msg);
}
