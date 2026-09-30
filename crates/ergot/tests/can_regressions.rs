//! Regressions for the CAN transport, each reproduced through the real sink
//! and worker with a scripted link: the defects found in review of the
//! earlier implementations, and the behaviour that replaced them.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use std::{
    collections::VecDeque,
    future::pending,
    pin::pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bbqueue::traits::bbqhdl::BbqHandle;
use ergot::{
    Address, AnyAllAppendix, FrameKind, Header, Key, TrafficClass,
    interface_manager::{
        FrameProcessor, InterfaceSendError, InterfaceState, Profile,
        interface_impls::can::CanInterface,
        profiles::{
            direct_edge::{BROADCAST_NODE_ID, CENTRAL_NODE_ID, DirectEdge},
            router::Router,
        },
        transports::can::{CanConfig, CanError, CanErrorKind, CanRx, CanRxTxWorker, CanTx},
        utils::{
            can::{CanFrame, FrameEnd, QueuedFrame, Sink, fragment, is_frame_len},
            std::{StdQueue, new_std_queue},
        },
    },
    multi_interface,
    net_stack::{ArcNetStack, NetStackHandle},
    time::{Instant, sleep_until},
};
use maitake_sync::WaitQueue;
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::{sync::Notify, time::timeout};

const CLASSIC: u8 = 8;
const FD: u8 = 64;
/// `prio` of a Normal-class message.
const NORMAL: u8 = 1;
type CanIf = CanInterface<StdQueue, 256>;
type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<CanIf>>;

// ---- Scripted link and processor ----
//
// The tests run on tokio's paused time, which is also the worker's clock
// (the `tokio-std` time backend): waiting for a scripted arrival time is
// instant, and the worker sees it pass.

#[derive(Debug)]
enum TestError {
    Fatal,
    Transient,
}

impl CanError for TestError {
    fn kind(&self) -> CanErrorKind {
        match self {
            TestError::Fatal => CanErrorKind::Stopped,
            TestError::Transient => CanErrorKind::Bus,
        }
    }
}

/// What the scripted receiver does once its script runs out.
enum End {
    /// Fail fatally, ending the worker.
    Fatal,
    /// Wait forever, leaving the worker to transmit.
    Idle,
    /// Report a transient error on every call, without ever waiting.
    TransientForever(Arc<AtomicUsize>),
}

/// Replays scripted frames (or errors), each at its arrival time in
/// milliseconds from when the script was made, after an optional extra
/// delay per call.
struct ScriptedRx {
    script: VecDeque<(u64, Result<CanFrame, TestError>)>,
    start: Instant,
    end: End,
    delay: Duration,
}

impl ScriptedRx {
    fn frames(script: Vec<(u64, CanFrame)>) -> Self {
        Self::script(
            script.into_iter().map(|(t, f)| (t, Ok(f))).collect(),
            End::Fatal,
        )
    }

    fn idle() -> Self {
        Self::script(VecDeque::new(), End::Idle)
    }

    fn script(script: VecDeque<(u64, Result<CanFrame, TestError>)>, end: End) -> Self {
        Self {
            script,
            start: Instant::now(),
            end,
            delay: Duration::ZERO,
        }
    }
}

impl CanRx for ScriptedRx {
    type Error = TestError;

    async fn recv(&mut self) -> Result<CanFrame, TestError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        match self.script.pop_front() {
            Some((at, res)) => {
                sleep_until(self.start + Duration::from_millis(at)).await;
                res
            }
            None => match &self.end {
                End::Fatal => Err(TestError::Fatal),
                End::Idle => pending().await,
                End::TransientForever(count) => {
                    // A worker that never yields here would hang the test
                    // (its timeout cannot fire); fail it instead.
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    assert!(n < 100_000, "the receive loop spins without yielding");
                    Err(TestError::Transient)
                }
            },
        }
    }
}

/// Records every frame it is handed. Optionally never accepts frames for one
/// destination node, and optionally parks on the first frame until a gate
/// opens.
#[derive(Clone)]
struct RecordingTx {
    max_payload: u8,
    sent: Arc<Mutex<Vec<CanFrame>>>,
    stuck_dst: Option<u8>,
    gate: Option<Arc<Notify>>,
}

impl Default for RecordingTx {
    fn default() -> Self {
        Self {
            max_payload: CLASSIC,
            sent: Arc::default(),
            stuck_dst: None,
            gate: None,
        }
    }
}

impl RecordingTx {
    fn sent(&self) -> Vec<CanFrame> {
        self.sent.lock().unwrap().clone()
    }
}

impl CanTx for RecordingTx {
    type Error = TestError;

    fn max_payload(&self) -> u8 {
        self.max_payload
    }

    async fn send(&mut self, frame: &CanFrame) -> Result<(), TestError> {
        assert!(
            is_frame_len(frame.len as usize) && frame.len <= self.max_payload,
            "a frame the controller cannot send as is: {} bytes",
            frame.len
        );
        if self.stuck_dst == Some(frame.id.dst_node()) {
            pending::<()>().await;
        }
        self.sent.lock().unwrap().push(*frame);
        if let Some(gate) = self.gate.take() {
            gate.notified().await;
        }
        Ok(())
    }
}

/// Counts complete frames handed to the processor.
struct Count(Arc<AtomicUsize>);

impl<N: NetStackHandle> FrameProcessor<N> for Count {
    fn process_frame(
        &mut self,
        _data: &[u8],
        _nsh: &N,
        _ident: <N::Profile as Profile>::InterfaceIdent,
    ) -> bool {
        self.0.fetch_add(1, Ordering::SeqCst);
        false
    }
    fn reset(&mut self) {}
}

// ---- A node: stack, queues, worker ----

struct Node {
    stack: EdgeStack,
    control: StdQueue,
    bulk: StdQueue,
    max_payload: u8,
}

impl Node {
    /// A classic-CAN edge at `(net_id, node_id)` with `queue_len`-byte
    /// outgoing queues.
    fn new(net_id: u16, node_id: u8, queue_len: usize) -> Self {
        Self::with_payload(net_id, node_id, queue_len, CLASSIC)
    }

    fn with_payload(net_id: u16, node_id: u8, queue_len: usize, max_payload: u8) -> Self {
        let control = new_std_queue(queue_len);
        let bulk = new_std_queue(queue_len);
        let stack = EdgeStack::new_with_profile(DirectEdge::new_target(Sink::new(
            control.clone(),
            bulk.clone(),
            max_payload,
        )));
        stack
            .manage_profile(|im| {
                im.set_interface_state((), InterfaceState::Active { net_id, node_id })
            })
            .unwrap();
        Self {
            stack,
            control,
            bulk,
            max_payload,
        }
    }

    fn send(&self, hdr: &Header, body: &[u32]) -> Result<(), InterfaceSendError> {
        self.stack.manage_profile(|im| im.send(hdr, &body))
    }

    fn worker<Rx: CanRx>(
        &self,
        rx: Rx,
        tx: RecordingTx,
        config: CanConfig,
        count: &Arc<AtomicUsize>,
    ) -> CanRxTxWorker<EdgeStack, Rx, RecordingTx, StdQueue, Count, 2, 256> {
        CanRxTxWorker::new(
            self.stack.clone(),
            rx,
            tx,
            Count(count.clone()),
            (),
            BbqHandle::framed_consumer(&self.control),
            BbqHandle::framed_consumer(&self.bulk),
            config,
        )
    }

    /// Run a worker for this node for at most `for_ms`, or until the receiver
    /// fails fatally. Returns how many complete frames reached the processor.
    async fn run<Rx: CanRx>(
        &self,
        rx: Rx,
        tx: RecordingTx,
        config: CanConfig,
        for_ms: u64,
    ) -> usize {
        let count = Arc::new(AtomicUsize::new(0));
        let state = self.state();
        let mut worker = self.worker(rx, tx, config, &count);
        let _ = timeout(Duration::from_millis(for_ms), worker.run(state)).await;
        // The worker leaves the interface Down when it stops; bring it back so
        // the node can queue more traffic for the next run.
        drop(worker);
        self.stack
            .manage_profile(|im| im.set_interface_state((), state))
            .unwrap();
        count.load(Ordering::SeqCst)
    }

    fn state(&self) -> InterfaceState {
        self.stack
            .manage_profile(|im| im.interface_state(()))
            .unwrap()
    }

    /// Transmit everything queued and return the CAN frames, in order.
    async fn transmit(&self, config: CanConfig) -> Vec<CanFrame> {
        let tx = RecordingTx {
            max_payload: self.max_payload,
            ..Default::default()
        };
        self.run(ScriptedRx::idle(), tx.clone(), config, 100).await;
        tx.sent()
    }
}

fn header(dst: Address, class: TrafficClass) -> Header {
    let any_all = [0, 255].contains(&dst.port_id);
    Header {
        src: Address {
            network_id: 0,
            node_id: 0,
            port_id: 3,
        },
        dst,
        any_all: any_all.then_some(AnyAllAppendix {
            key: Key([1; 8]),
            nash: None,
        }),
        kind: FrameKind::ENDPOINT_REQ,
        class,
        ttl: 15,
    }
}

fn to(network_id: u16, node_id: u8) -> Address {
    Address {
        network_id,
        node_id,
        port_id: 4,
    }
}

/// 48 bytes of body: several classic frames.
const BIG: [u32; 12] = [0x5A5A_5A5A; 12];

/// Normal-class frames of `data` from node 30 to the segment router.
fn fragments(data: &[u8], tid: u8) -> Vec<CanFrame> {
    let mut out = Vec::new();
    fragment(
        data,
        CLASSIC as usize,
        NORMAL,
        CENTRAL_NODE_ID,
        30,
        tid,
        |f| {
            out.push(*f);
            Ok(())
        },
    )
    .unwrap();
    out
}

// ---- Reassembly timing and duplicates ----

#[tokio::test(start_paused = true)]
async fn worker_drops_a_continuation_that_arrives_after_the_timeout() {
    let f = fragments(&[0x55; 10], 1); // Normal class: 100 ms
    assert_eq!(f.len(), 2);
    let node = Node::new(10, CENTRAL_NODE_ID, 4096);
    let delivered = |script: Vec<(u64, CanFrame)>| {
        let rx = ScriptedRx::frames(script);
        node.run(rx, RecordingTx::default(), CanConfig::new(0), 2000)
    };
    // The worker's expire() ran before recv(); recv() then took a second. The
    // late frame must not complete the message.
    assert_eq!(delivered(vec![(0, f[0]), (1000, f[1])]).await, 0);
    // Control: the same two frames within the window do complete.
    assert_eq!(delivered(vec![(0, f[0]), (50, f[1])]).await, 1);
}

/// CAN retransmits a frame whose last EOF bit the transmitter saw disturbed,
/// after the receivers had already accepted it. Every such repeat — of a
/// multi-frame message's first, middle or last frame, or of a single-frame
/// message — must not deliver anything twice, nor lose the message.
#[tokio::test(start_paused = true)]
async fn repeated_frames_deliver_each_message_once() {
    let node = Node::new(10, CENTRAL_NODE_ID, 4096);
    let multi = fragments(&[0x55; 20], 1);
    let single = fragments(&[0x66; 5], 2);
    assert_eq!((multi.len(), single.len()), (3, 1));
    let script: Vec<(u64, CanFrame)> = multi
        .iter()
        .chain(single.iter())
        .flat_map(|f| [*f, *f])
        .enumerate()
        .map(|(t, f)| (t as u64, f))
        .collect();
    let rx = ScriptedRx::frames(script);
    assert_eq!(
        node.run(rx, RecordingTx::default(), CanConfig::new(0), 1000)
            .await,
        2
    );
}

// ---- Link addressing comes from LinkMeta ----

#[tokio::test(start_paused = true)]
async fn can_ids_carry_the_link_addressing() {
    let node = Node::new(10, 30, 4096);
    let dsts = |frames: &[CanFrame]| frames.iter().map(|f| f.id.dst_node()).collect::<Vec<_>>();

    // Off-segment: every frame goes to the segment router.
    node.send(&header(to(99, 50), TrafficClass::Normal), &BIG)
        .unwrap();
    let frames = node.transmit(CanConfig::new(0)).await;
    assert!(
        frames.len() > 1,
        "48-byte body must fragment on 8-byte frames"
    );
    assert!(dsts(&frames).iter().all(|&d| d == CENTRAL_NODE_ID));
    assert!(frames.iter().all(|f| f.id.src_node() == 30));

    // Same segment: straight to the node.
    node.send(&header(to(10, 42), TrafficClass::Normal), &[1])
        .unwrap();
    assert!(
        dsts(&node.transmit(CanConfig::new(0)).await)
            .iter()
            .all(|&d| d == 42)
    );

    // Broadcast: every node.
    let mut bcast = to(0, 0);
    bcast.port_id = 255;
    node.send(&header(bcast, TrafficClass::Normal), &[1])
        .unwrap();
    assert!(
        dsts(&node.transmit(CanConfig::new(0)).await)
            .iter()
            .all(|&d| d == BROADCAST_NODE_ID)
    );
}

multi_interface! {
    enum GatewaySink for GatewayInterface {
        Can(CanIf),
        Other(CanIf),
    }
}

/// A CAN sink inside a `multi_interface!` router (a USB↔CAN gateway) used to
/// never learn its node id: the macro did not forward the out-of-band hook,
/// so the gateway sent from node 0 and addressed its own node 1.
#[test]
fn gateway_router_addresses_can_frames_from_its_own_node() {
    let control = new_std_queue(4096);
    let bulk = new_std_queue(4096);
    let mut router: Router<GatewayInterface, rand::rngs::StdRng, 4, 4> =
        Router::new(rand::SeedableRng::seed_from_u64(0));
    let ident = router
        .register_interface(GatewaySink::Can(Sink::new(
            control.clone(),
            bulk.clone(),
            CLASSIC,
        )))
        .unwrap();
    let net_id = router.net_id_of(ident).unwrap();

    router
        .send(&header(to(net_id, 42), TrafficClass::Normal), &[1u32])
        .unwrap();

    let consumer = BbqHandle::framed_consumer::<u16>(&bulk);
    let entry = consumer.read().expect("frame queued");
    let q = QueuedFrame::parse(&entry).unwrap();
    assert_eq!((q.src_node, q.dst_node), (CENTRAL_NODE_ID, 42));
}

#[tokio::test(start_paused = true)]
async fn message_ids_count_per_class_from_the_seed() {
    // Two boots of "the same node" with different seeds issue different ids.
    for seed in [0u8, 4] {
        let node = Node::new(10, 30, 4096);
        node.send(&header(to(10, 42), TrafficClass::Normal), &BIG)
            .unwrap();
        node.send(&header(to(10, 42), TrafficClass::Control), &[1])
            .unwrap();
        node.send(&header(to(10, 42), TrafficClass::Normal), &[2])
            .unwrap();
        let frames = node.transmit(CanConfig::new(seed)).await;
        let starts: Vec<(u8, u8)> = frames
            .iter()
            .filter(|f| f.id.idx() == 0)
            .map(|f| (f.id.prio(), f.id.tid()))
            .collect();
        // Control goes first; each class counts on its own.
        assert_eq!(
            starts,
            vec![(0, seed + 1), (NORMAL, seed + 1), (NORMAL, seed + 2)]
        );
    }
}

// ---- Queueing ----

/// A message that does not fit the queue is refused whole, and what was
/// already queued goes out intact. (Queued as CAN fragments, the head of a
/// message used to go out and occupy receivers' slots until they timed it
/// out.)
#[tokio::test(start_paused = true)]
async fn a_send_that_does_not_fit_queues_nothing() {
    // Room for one entry of the largest frame (256 + 3 + 2 bytes), not two.
    let node = Node::new(10, 30, 300);
    node.send(&header(to(10, 42), TrafficClass::Normal), &BIG)
        .unwrap();
    assert!(
        node.send(&header(to(10, 43), TrafficClass::Normal), &[1])
            .is_err()
    );
    let frames = node.transmit(CanConfig::new(0)).await;
    assert!(frames.iter().all(|f| f.id.dst_node() == 42));
    assert_eq!(frames.last().unwrap().id.end(), Some(FrameEnd::Last));
    assert!(
        frames
            .iter()
            .enumerate()
            .all(|(i, f)| f.id.idx() as usize == i)
    );
}

/// Control traffic queued while a Bulk message is on the wire goes out
/// between its frames, not after the whole message.
#[tokio::test(start_paused = true)]
async fn control_cuts_into_a_bulk_message() {
    let node = Node::new(10, 30, 4096);
    node.send(&header(to(10, 42), TrafficClass::Bulk), &BIG)
        .unwrap();

    // The first Bulk frame parks the transmitter until Control is queued.
    let gate = Arc::new(Notify::new());
    let tx = RecordingTx {
        gate: Some(gate.clone()),
        ..Default::default()
    };
    let run = node.run(ScriptedRx::idle(), tx.clone(), CanConfig::new(0), 200);
    let inject = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        node.send(&header(to(10, 43), TrafficClass::Control), &[7])
            .unwrap();
        gate.notify_one();
    };
    tokio::join!(run, inject);

    let dsts: Vec<u8> = tx.sent().iter().map(|f| f.id.dst_node()).collect();
    let bulk_frames = dsts.iter().filter(|&&d| d == 42).count();
    assert!(bulk_frames > 2, "the Bulk message spans several frames");
    assert_eq!(dsts[0], 42, "the Bulk message started first");
    let first_control = dsts.iter().position(|&d| d == 43).unwrap();
    let last_bulk = dsts.iter().rposition(|&d| d == 42).unwrap();
    assert!(
        first_control < last_bulk,
        "Control went out mid-message: {dsts:?}"
    );
}

// ---- CAN FD through the worker ----

/// An FD sender pads a last frame that is not a frame length and keeps the
/// data length in its spare byte; an FD receiver takes it back out.
#[tokio::test(start_paused = true)]
async fn fd_messages_round_trip_through_the_worker() {
    let sender = Node::with_payload(10, 30, 4096, FD);
    // A short message fits one frame; 3 × BIG (180 encoded bytes) spans
    // several.
    sender
        .send(
            &header(to(10, CENTRAL_NODE_ID), TrafficClass::Normal),
            &[1u32; 12],
        )
        .unwrap();
    let big3: Vec<u32> = [BIG, BIG, BIG].concat();
    sender
        .send(
            &header(to(10, CENTRAL_NODE_ID), TrafficClass::Normal),
            &big3,
        )
        .unwrap();
    let frames = sender.transmit(CanConfig::new(0)).await;
    let ends: Vec<FrameEnd> = frames.iter().filter_map(|f| f.id.end()).collect();
    assert_eq!(frames[0].id.idx(), 0);
    assert!(frames.len() > 2);
    assert_eq!(
        ends.iter().filter(|&&e| e != FrameEnd::More).count(),
        2,
        "two messages: {ends:?}"
    );
    assert!(
        ends.contains(&FrameEnd::LastPadded),
        "a padded last frame is exercised: {ends:?}"
    );

    let receiver = Node::with_payload(10, CENTRAL_NODE_ID, 4096, FD);
    let script = frames
        .into_iter()
        .enumerate()
        .map(|(t, f)| (t as u64, f))
        .collect();
    let rx = ScriptedRx::frames(script);
    let tx = RecordingTx {
        max_payload: FD,
        ..Default::default()
    };
    assert_eq!(receiver.run(rx, tx, CanConfig::new(0), 1000).await, 2);
}

// ---- The receive filter follows address changes ----

/// The node id the receive filter checks against is learned from sent frames
/// and cached. If the interface's address changes without a transmission
/// since — a bus claim that was denied restores the previous node — frames
/// to the new address must still be taken.
#[tokio::test(start_paused = true)]
async fn the_receive_filter_follows_an_address_change_without_a_transmission() {
    let node = Node::new(10, 30, 4096);
    // Something to send from node 30, so the filter learns 30 from the TX side.
    node.send(&header(to(10, 42), TrafficClass::Normal), &[1])
        .unwrap();
    let to_40 = single_to(40);
    let mut rx = ScriptedRx::frames(vec![(0, to_40)]);
    // The frame arrives after the address has changed.
    rx.delay = Duration::from_millis(30);
    let change = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        node.stack
            .manage_profile(|im| {
                im.set_interface_state(
                    (),
                    InterfaceState::Active {
                        net_id: 10,
                        node_id: 40,
                    },
                )
            })
            .unwrap();
    };
    let (delivered, ()) = tokio::join!(
        node.run(rx, RecordingTx::default(), CanConfig::new(0), 500),
        change
    );
    assert_eq!(delivered, 1);
}

/// A single-frame message from the router to `node`.
fn single_to(node: u8) -> CanFrame {
    let mut out = None;
    fragment(
        &[0x77; 6],
        CLASSIC as usize,
        NORMAL,
        node,
        CENTRAL_NODE_ID,
        1,
        |f| {
            out = Some(*f);
            Ok(())
        },
    )
    .unwrap();
    out.unwrap()
}

// ---- A stuck transmitter, a noisy receiver ----

/// While the controller does not accept a frame (bus saturated by
/// higher-priority traffic, or no ACK), received frames must still be
/// processed: the RX FIFO of a CAN controller is only a few frames deep.
#[tokio::test(start_paused = true)]
async fn receiving_continues_while_transmit_is_stuck() {
    let node = Node::new(10, CENTRAL_NODE_ID, 4096);
    node.send(&header(to(10, 42), TrafficClass::Normal), &[1])
        .unwrap();
    let tx = RecordingTx {
        stuck_dst: Some(42),
        ..Default::default()
    };
    let f = fragments(&[0x55; 10], 1);
    let config = CanConfig {
        tx_timeout_ms: None,
        ..CanConfig::new(0)
    };
    let rx = ScriptedRx::frames(vec![(0, f[0]), (1, f[1])]);
    assert_eq!(node.run(rx, tx.clone(), config, 500).await, 1);
    assert!(tx.sent().is_empty(), "the stuck frame never went out");
}

/// A frame the controller does not accept within the TX timeout abandons its
/// message; the queue moves on instead of wedging.
#[tokio::test(start_paused = true)]
async fn tx_timeout_abandons_a_stuck_message() {
    let node = Node::new(10, 30, 4096);
    node.send(&header(to(10, 42), TrafficClass::Normal), &[1])
        .unwrap();
    node.send(&header(to(10, 43), TrafficClass::Normal), &[2])
        .unwrap();
    let tx = RecordingTx {
        stuck_dst: Some(42),
        ..Default::default()
    };
    let config = CanConfig {
        tx_timeout_ms: Some(20),
        ..CanConfig::new(0)
    };
    node.run(ScriptedRx::idle(), tx.clone(), config, 300).await;
    let sent = tx.sent();
    assert!(!sent.is_empty(), "the next message went out");
    assert!(sent.iter().all(|f| f.id.dst_node() == 43));
}

/// A transient adapter error (RX overrun, bus-off with recovery) is logged,
/// not fatal: frames after it are still delivered.
#[tokio::test(start_paused = true)]
async fn a_transient_rx_error_does_not_stop_the_worker() {
    let node = Node::new(10, CENTRAL_NODE_ID, 4096);
    let f = fragments(&[0x55; 10], 1);
    let rx = ScriptedRx::script(
        [(0, Err(TestError::Transient)), (1, Ok(f[0])), (2, Ok(f[1]))].into(),
        End::Fatal,
    );
    assert_eq!(
        node.run(rx, RecordingTx::default(), CanConfig::new(0), 500)
            .await,
        1
    );
}

/// An adapter that keeps reporting a transient error without ever waiting
/// (an error-passive controller, say) must not starve the transmit side: the
/// receive loop backs off instead of spinning.
#[tokio::test(start_paused = true)]
async fn a_stream_of_transient_errors_does_not_starve_transmit() {
    let node = Node::new(10, 30, 4096);
    node.send(&header(to(10, 42), TrafficClass::Control), &[1])
        .unwrap();
    let errors = Arc::new(AtomicUsize::new(0));
    let rx = ScriptedRx::script(VecDeque::new(), End::TransientForever(errors.clone()));
    let tx = RecordingTx::default();
    node.run(rx, tx.clone(), CanConfig::new(0), 100).await;
    assert!(!tx.sent().is_empty(), "the queued message went out");
    assert!(
        errors.load(Ordering::SeqCst) < 1000,
        "the receive loop spun: {} errors in 100 ms",
        errors.load(Ordering::SeqCst)
    );
}

/// Dropping a worker that did not finish `run()` (its task was cancelled)
/// takes the interface Down — and tells the state observers so.
#[tokio::test(start_paused = true)]
async fn dropping_a_cancelled_worker_notifies_state_observers() {
    static STATE: WaitQueue = WaitQueue::new();
    let node = Node::new(10, 30, 4096);
    let count = Arc::new(AtomicUsize::new(0));
    let mut worker = node
        .worker(
            ScriptedRx::idle(),
            RecordingTx::default(),
            CanConfig::new(0),
            &count,
        )
        .with_state_notify(&STATE);
    let _ = timeout(Duration::from_millis(10), worker.run(node.state())).await;

    let mut waiter = pin!(STATE.wait());
    // Register the waiter before the drop.
    assert!(
        timeout(Duration::from_millis(1), waiter.as_mut())
            .await
            .is_err()
    );
    drop(worker);
    assert!(
        timeout(Duration::from_millis(100), waiter).await.is_ok(),
        "no notification"
    );
    assert_eq!(node.state(), InterfaceState::Down);
}
