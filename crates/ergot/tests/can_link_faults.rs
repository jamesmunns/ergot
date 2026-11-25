//! The CAN worker on a link that misbehaves: adapters that report errors, a
//! controller that accepts nothing, frames lost in its FIFO, an address that
//! changes under traffic, a sender that serves several nodes. Each case runs
//! the real sink and worker over scripted adapters, on tokio's paused time,
//! which is also the worker's clock.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use std::{
    collections::VecDeque,
    future::{pending, poll_fn},
    sync::{Arc, Mutex},
    task::{Poll, Waker},
    time::Duration,
};

use bbqueue::traits::bbqhdl::BbqHandle;
use ergot::{
    Address, AnyAllAppendix, FrameKind, Header, Key, TrafficClass,
    interface_manager::{
        FrameProcessor, InterfaceSendError, InterfaceState, Profile,
        interface_impls::can::CanInterface,
        profiles::direct_edge::{CENTRAL_NODE_ID, DirectEdge},
        transports::can::{
            CanConfig, CanError, CanErrorKind, CanRx, CanRxTxWorker, CanTx, CanWorkerError,
        },
        utils::{
            can::{CanFrame, FrameEnd, Sink, fragment},
            std::{StdQueue, new_std_queue},
        },
    },
    net_stack::{ArcNetStack, NetStackHandle},
    time::{Instant, sleep_until},
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::time::timeout;

const CLASSIC: u8 = 8;
const FD: u8 = 64;
const NORMAL: u8 = 1;
type CanIf = CanInterface<StdQueue, 256>;
type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<CanIf>>;

#[derive(Debug, Clone, Copy, PartialEq)]
struct TestError(CanErrorKind);

impl CanError for TestError {
    fn kind(&self) -> CanErrorKind {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Event {
    Filter(Option<u8>),
    Sent { src: u8 },
}
type Log = Arc<Mutex<Vec<Event>>>;

// ---- RX adapters ----

/// Replays frames at their arrival time (ms from creation), then idles or
/// fails fatally.
struct ScriptedRx {
    script: VecDeque<(u64, CanFrame)>,
    start: Instant,
    fatal_at_end: bool,
    log: Log,
}

impl ScriptedRx {
    fn new(script: Vec<(u64, CanFrame)>, fatal_at_end: bool) -> Self {
        Self {
            script: script.into(),
            start: Instant::now(),
            fatal_at_end,
            log: Log::default(),
        }
    }
    fn idle() -> Self {
        Self::new(vec![], false)
    }
}

impl CanRx for ScriptedRx {
    type Error = TestError;
    async fn recv(&mut self) -> Result<CanFrame, TestError> {
        match self.script.front() {
            Some(&(at, _)) => {
                sleep_until(self.start + Duration::from_millis(at)).await;
                Ok(self.script.pop_front().unwrap().1)
            }
            None if self.fatal_at_end => Err(TestError(CanErrorKind::Stopped)),
            None => pending().await,
        }
    }
    fn set_node_filter(&mut self, node: Option<u8>) -> Result<(), TestError> {
        self.log.lock().unwrap().push(Event::Filter(node));
        Ok(())
    }
}

/// A controller that is quiet until `quiet_ms`, then error-passive, behind
/// an adapter that reports the state on every call: a `recv()` that finds no
/// frame returns a transient error at once. Frames that are due are still
/// returned first.
struct PassiveRx {
    start: Instant,
    quiet_ms: u64,
    frames: VecDeque<(u64, CanFrame)>,
    log: Log,
    calls: usize,
}

impl CanRx for PassiveRx {
    type Error = TestError;
    async fn recv(&mut self) -> Result<CanFrame, TestError> {
        sleep_until(self.start + Duration::from_millis(self.quiet_ms)).await;
        if let Some(&(at, f)) = self.frames.front()
            && Instant::now() >= self.start + Duration::from_millis(at)
        {
            self.frames.pop_front();
            return Ok(f);
        }
        self.calls += 1;
        assert!(self.calls < 100_000, "receive loop spins");
        Err(TestError(CanErrorKind::Bus))
    }
    fn set_node_filter(&mut self, node: Option<u8>) -> Result<(), TestError> {
        self.log.lock().unwrap().push(Event::Filter(node));
        Ok(())
    }
}

// ---- TX adapter ----

#[derive(Clone)]
struct ProbeTx {
    max_payload: u8,
    start: Instant,
    /// (ms since creation, frame)
    sent: Arc<Mutex<Vec<(u64, CanFrame)>>>,
    /// Never accept frames for this destination.
    stuck_dst: Option<u8>,
    /// Report this error for the n-th frame handed over (0-based), once.
    fail: Arc<Mutex<Option<(usize, CanErrorKind)>>>,
    handed: Arc<Mutex<usize>>,
    log: Log,
}

impl ProbeTx {
    fn new(max_payload: u8) -> Self {
        Self {
            max_payload,
            start: Instant::now(),
            sent: Arc::default(),
            stuck_dst: None,
            fail: Arc::default(),
            handed: Arc::default(),
            log: Log::default(),
        }
    }
    fn frames(&self) -> Vec<CanFrame> {
        self.sent.lock().unwrap().iter().map(|(_, f)| *f).collect()
    }
    fn timed(&self) -> Vec<(u64, CanFrame)> {
        self.sent.lock().unwrap().clone()
    }
}

impl CanTx for ProbeTx {
    type Error = TestError;
    fn max_payload(&self) -> u8 {
        self.max_payload
    }
    async fn send(&mut self, frame: &CanFrame) -> Result<(), TestError> {
        if self.stuck_dst == Some(frame.id.dst_node()) {
            pending::<()>().await;
        }
        let n = {
            let mut h = self.handed.lock().unwrap();
            *h += 1;
            *h - 1
        };
        {
            let mut fail = self.fail.lock().unwrap();
            if let Some((at, kind)) = *fail
                && at == n
            {
                *fail = None;
                return Err(TestError(kind));
            }
        }
        let at = Instant::now()
            .saturating_duration_since(self.start)
            .as_millis() as u64;
        self.sent.lock().unwrap().push((at, *frame));
        self.log.lock().unwrap().push(Event::Sent {
            src: frame.id.src_node(),
        });
        Ok(())
    }
}

// ---- Processor ----

/// Keeps every complete frame handed to the processor.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Vec<u8>>>>);

impl<N: NetStackHandle> FrameProcessor<N> for Capture {
    fn process_frame(
        &mut self,
        data: &[u8],
        _nsh: &N,
        _ident: <N::Profile as Profile>::InterfaceIdent,
    ) -> bool {
        self.0.lock().unwrap().push(data.to_vec());
        false
    }
    fn reset(&mut self) {}
}

// ---- Node ----

struct Node {
    stack: EdgeStack,
    control: StdQueue,
    bulk: StdQueue,
}

type RunResult<Rx, Tx> =
    Option<Result<(), CanWorkerError<<Rx as CanRx>::Error, <Tx as CanTx>::Error>>>;

impl Node {
    fn new(net_id: u16, node_id: u8, queue_len: usize, max_payload: u8) -> Self {
        let control = new_std_queue(queue_len);
        let bulk = new_std_queue(queue_len);
        let stack = EdgeStack::new_with_profile(DirectEdge::new_target(Sink::new(
            control.clone(),
            bulk.clone(),
            max_payload,
        )));
        let node = Self {
            stack,
            control,
            bulk,
        };
        node.set_state(InterfaceState::Active { net_id, node_id });
        node
    }

    fn set_state(&self, state: InterfaceState) {
        self.stack
            .manage_profile(|im| im.set_interface_state((), state))
            .unwrap();
    }

    fn state(&self) -> InterfaceState {
        self.stack
            .manage_profile(|im| im.interface_state(()))
            .unwrap()
    }

    fn send(&self, hdr: &Header, body: &[u32]) -> Result<(), InterfaceSendError> {
        self.stack.manage_profile(|im| im.send(hdr, &body))
    }

    /// Run a worker for at most `for_ms`. `None` if it was still running.
    /// The interface is left as the worker left it (Down).
    async fn run<Rx: CanRx, Tx: CanTx, P: FrameProcessor<EdgeStack>>(
        &self,
        rx: Rx,
        tx: Tx,
        processor: P,
        config: CanConfig,
        for_ms: u64,
    ) -> RunResult<Rx, Tx> {
        let state = self.state();
        let mut worker = CanRxTxWorker::<_, _, _, StdQueue, _, 2, 256>::new(
            self.stack.clone(),
            rx,
            tx,
            processor,
            (),
            BbqHandle::framed_consumer(&self.control),
            BbqHandle::framed_consumer(&self.bulk),
            config,
        );
        let res = timeout(Duration::from_millis(for_ms), worker.run(state))
            .await
            .ok();
        drop(worker);
        res
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

fn describe(frames: &[CanFrame]) -> String {
    frames
        .iter()
        .map(|f| {
            format!(
                "[prio {} dst {} src {} tid {} idx {} {:?} len {}]",
                f.id.prio(),
                f.id.dst_node(),
                f.id.src_node(),
                f.id.tid(),
                f.id.idx(),
                f.id.end().unwrap(),
                f.len
            )
        })
        .collect::<Vec<_>>()
        .join("\n  ")
}

// ===========================================================================
// A sender gives up a message to node 1 after a part of it, then sends eight
// messages to node 42, the last one long. Node 1 hears them all (no hardware
// filter). The frames of the long message must not complete the one node 1
// holds a part of: they are for another node, though their `tid` and `idx`
// would fit.
// ===========================================================================

async fn splice_case(fail_frame: usize) -> (Vec<CanFrame>, Vec<Vec<u8>>) {
    // CAN FD sender, node 30, everything Normal class.
    let sender = Node::new(10, 30, 16384, FD);
    // A: three FD frames, for node 1.
    sender
        .send(
            &header(to(10, CENTRAL_NODE_ID), TrafficClass::Normal),
            &[0xAAAA_AAAA; 36],
        )
        .unwrap();
    // Seven small messages (one frame each) for node 42.
    for i in 0..7u32 {
        sender
            .send(&header(to(10, 42), TrafficClass::Normal), &[i])
            .unwrap();
    }
    // B: three FD frames, for node 42.
    sender
        .send(
            &header(to(10, 42), TrafficClass::Normal),
            &[0xBBBB_BBBB; 36],
        )
        .unwrap();

    // The controller reports one transient TX failure, on a frame of A.
    let tx = ProbeTx::new(FD);
    *tx.fail.lock().unwrap() = Some((fail_frame, CanErrorKind::TxFailed));
    sender
        .run(
            ScriptedRx::idle(),
            tx.clone(),
            Capture::default(),
            CanConfig::new(0),
            50,
        )
        .await;
    let wire = tx.frames();

    // Node 1 hears all of it (no hardware filter: the trait's default), one
    // frame per millisecond.
    let receiver = Node::new(10, CENTRAL_NODE_ID, 4096, FD);
    let got = Capture::default();
    let script = wire
        .iter()
        .enumerate()
        .map(|(t, f)| (t as u64, *f))
        .collect();
    receiver
        .run(
            ScriptedRx::new(script, true),
            ProbeTx::new(FD),
            got.clone(),
            CanConfig::new(0),
            1000,
        )
        .await;
    let delivered = got.0.lock().unwrap().clone();
    (wire, delivered)
}

#[tokio::test(start_paused = true)]
async fn an_abandoned_message_is_not_completed_with_another_nodes_frames() {
    for fail_frame in [1usize, 2] {
        let (wire, delivered) = splice_case(fail_frame).await;
        // A was never completed by its sender, and B is not for node 1.
        assert!(
            delivered.is_empty(),
            "node 1 was handed {} frame(s) although nothing complete was addressed to it; \
             on the wire:\n  {}",
            delivered.len(),
            describe(&wire)
        );
    }
}

// ===========================================================================
// While the adapter reports a transient error on every call, the interface
// gets a new node. The filter follows before the first frame from the new
// node goes out, and a frame for the new node is delivered.
// ===========================================================================

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

#[tokio::test(start_paused = true)]
async fn the_node_filter_follows_an_address_change_while_rx_reports_errors() {
    let node = Node::new(10, 30, 4096, CLASSIC);
    let log = Log::default();
    let rx = PassiveRx {
        start: Instant::now(),
        quiet_ms: 5,
        // A frame for the new address, well after the change.
        frames: [(30, single_to(40))].into(),
        log: log.clone(),
        calls: 0,
    };
    let mut tx = ProbeTx::new(CLASSIC);
    tx.log = log.clone();
    let got = Capture::default();
    let change = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        node.set_state(InterfaceState::Active {
            net_id: 10,
            node_id: 40,
        });
        node.send(&header(to(10, 42), TrafficClass::Normal), &[2])
            .unwrap();
    };
    tokio::join!(
        node.run(rx, tx, got.clone(), CanConfig::new(0), 100),
        change
    );
    let log = log.lock().unwrap().clone();
    let delivered = got.0.lock().unwrap().len();
    let pos = |e: Event| log.iter().position(|x| *x == e);
    assert!(pos(Event::Sent { src: 40 }).is_some(), "it did send");
    assert!(
        pos(Event::Filter(Some(40))).is_some_and(|f| f < pos(Event::Sent { src: 40 }).unwrap()),
        "no filter for node 40 before a frame from node 40 went out"
    );
    assert_eq!(delivered, 1, "the frame for node 40 was filtered away");
}

// ===========================================================================
// A small hardware FIFO (three frames, as bxCAN's) fed at a constant rate.
// One scheduling hiccup overruns it once, and the adapter reports that as
// `CanErrorKind::Overrun`: before the frames it still holds, or after them.
// The worker must go on receiving at once. A pause after the error would
// overrun the FIFO again, and that one the next.
// ===========================================================================

#[derive(Default)]
struct Fifo {
    q: VecDeque<CanFrame>,
    overrun: bool,
    lost: usize,
    reports: usize,
    waker: Option<Waker>,
}

struct FifoRx {
    fifo: Arc<Mutex<Fifo>>,
    report_first: bool,
}

impl CanRx for FifoRx {
    type Error = TestError;
    async fn recv(&mut self) -> Result<CanFrame, TestError> {
        poll_fn(|cx| {
            let mut f = self.fifo.lock().unwrap();
            if self.report_first && f.overrun {
                f.overrun = false;
                f.reports += 1;
                return Poll::Ready(Err(TestError(CanErrorKind::Overrun)));
            }
            if let Some(frame) = f.q.pop_front() {
                return Poll::Ready(Ok(frame));
            }
            if f.overrun {
                f.overrun = false;
                f.reports += 1;
                return Poll::Ready(Err(TestError(CanErrorKind::Overrun)));
            }
            f.waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
}

/// Feed `n` single-frame messages into a `depth`-frame FIFO, one every
/// `period_us`. `stall_frames` frames arrive back to back first, as after
/// one scheduling hiccup. Returns (delivered, lost in the FIFO, overrun
/// reports).
async fn fifo_run(
    n: usize,
    period_us: u64,
    depth: usize,
    stall_frames: usize,
    report_first: bool,
) -> (usize, usize, usize) {
    let node = Node::new(10, CENTRAL_NODE_ID, 4096, CLASSIC);
    let fifo = Arc::new(Mutex::new(Fifo::default()));
    let got = Capture::default();
    let push = |fifo: &Arc<Mutex<Fifo>>, i: usize| {
        let mut out = None;
        fragment(
            &[i as u8; 6],
            CLASSIC as usize,
            NORMAL,
            CENTRAL_NODE_ID,
            30,
            (i % 8) as u8,
            |f| {
                out = Some(*f);
                Ok(())
            },
        )
        .unwrap();
        let mut f = fifo.lock().unwrap();
        if f.q.len() >= depth {
            f.overrun = true;
            f.lost += 1;
        } else {
            f.q.push_back(out.unwrap());
        }
        if let Some(w) = f.waker.take() {
            w.wake();
        }
    };
    let bus = async {
        for i in 0..stall_frames {
            push(&fifo, i);
        }
        // Time moves in 50 us steps, so the worker's timers fire between
        // frames too.
        const STEP_US: u64 = 50;
        for i in stall_frames..n {
            push(&fifo, i);
            for _ in 0..period_us / STEP_US {
                tokio::time::advance(Duration::from_micros(STEP_US)).await;
            }
        }
        // Let the worker drain what is left.
        tokio::time::advance(Duration::from_millis(5)).await;
        tokio::time::advance(Duration::from_millis(5)).await;
    };
    let rx = FifoRx {
        fifo: fifo.clone(),
        report_first,
    };
    let worker = node.run(
        rx,
        ProbeTx::new(CLASSIC),
        got.clone(),
        CanConfig::new(0),
        60_000,
    );
    tokio::select! {
        _ = worker => {}
        _ = bus => {}
    }
    let f = fifo.lock().unwrap();
    let delivered = got.0.lock().unwrap().len();
    (delivered, f.lost, f.reports)
}

#[tokio::test(start_paused = true)]
async fn one_overrun_costs_only_the_frames_it_lost() {
    const N: usize = 2000;
    // 4 frames a millisecond is about half load at 1 Mbit/s. Without a
    // hiccup the model loses nothing.
    assert_eq!(fifo_run(N, 250, 3, 0, true).await, (N, 0, 0));

    // One hiccup: 4 frames arrive before the worker runs once, so 1 is lost.
    for (period_us, report_first) in [(250, true), (1000, true), (250, false), (500, false)] {
        let (delivered, lost, reports) = fifo_run(N, period_us, 3, 4, report_first).await;
        assert!(
            lost <= 4,
            "a frame every {period_us} us, overrun reported {}: {lost} frames lost after a \
             single overrun, {delivered} of {N} delivered, {reports} overrun reports",
            if report_first { "first" } else { "last" }
        );
    }
}

// ===========================================================================
// How long Control waits behind a frame of another class that the controller
// does not accept, and how fast a queue drains when nothing is accepted.
// ===========================================================================

#[tokio::test(start_paused = true)]
async fn control_does_not_wait_out_a_frame_the_controller_holds() {
    let node = Node::new(10, 30, 4096, CLASSIC);
    node.send(&header(to(10, 42), TrafficClass::Bulk), &[0x5A5A_5A5A; 12])
        .unwrap();
    let mut tx = ProbeTx::new(CLASSIC);
    tx.stuck_dst = Some(42);
    let inject = async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        node.send(&header(to(10, 43), TrafficClass::Control), &[7])
            .unwrap();
    };
    tokio::join!(
        node.run(
            ScriptedRx::idle(),
            tx.clone(),
            Capture::default(),
            CanConfig::new(0),
            400
        ),
        inject
    );
    let sent = tx.timed();
    let first_control = sent.iter().find(|(_, f)| f.id.prio() == 0).map(|(t, _)| *t);
    assert!(
        first_control.is_some_and(|t| t < 20),
        "Control queued at 5 ms waited behind the stuck Bulk frame until {first_control:?} ms"
    );
}

#[tokio::test(start_paused = true)]
async fn a_queue_drains_fast_while_nothing_is_accepted() {
    let node = Node::new(10, 30, 16384, CLASSIC);
    for i in 0..10u32 {
        node.send(&header(to(10, 42), TrafficClass::Control), &[i])
            .unwrap();
    }
    node.send(&header(to(10, 43), TrafficClass::Control), &[99])
        .unwrap();
    let mut tx = ProbeTx::new(CLASSIC);
    tx.stuck_dst = Some(42);
    node.run(
        ScriptedRx::idle(),
        tx.clone(),
        Capture::default(),
        CanConfig::new(0),
        5000,
    )
    .await;
    let t = tx.timed().first().map(|(t, _)| *t);
    assert!(
        t.is_some_and(|t| t <= 200),
        "ten stuck messages held the queue for {t:?} ms"
    );
}

// ===========================================================================
// What a link still has queued when it stops does not go out when it is run
// again, a minute later and under another node.
// ===========================================================================

#[tokio::test(start_paused = true)]
async fn what_was_queued_does_not_outlive_the_link() {
    let node = Node::new(10, 30, 4096, CLASSIC);
    node.send(&header(to(10, 42), TrafficClass::Control), &[1])
        .unwrap();
    // The controller accepts nothing and the receiver then fails fatally.
    let mut stuck = ProbeTx::new(CLASSIC);
    stuck.stuck_dst = Some(42);
    let res = node
        .run(
            ScriptedRx::new(vec![], true),
            stuck,
            Capture::default(),
            CanConfig::new(0),
            50,
        )
        .await;
    assert!(matches!(res, Some(Err(CanWorkerError::Rx(_)))));
    assert_eq!(node.state(), InterfaceState::Down);
    assert!(
        node.send(&header(to(10, 42), TrafficClass::Control), &[2])
            .is_err(),
        "a Down interface takes no sends"
    );

    // A minute later the application restarts the link, now as node 40.
    tokio::time::sleep(Duration::from_secs(60)).await;
    node.set_state(InterfaceState::Active {
        net_id: 10,
        node_id: 40,
    });
    let log = Log::default();
    let mut rx = ScriptedRx::idle();
    rx.log = log.clone();
    let mut tx = ProbeTx::new(CLASSIC);
    tx.log = log.clone();
    node.run(rx, tx.clone(), Capture::default(), CanConfig::new(0), 50)
        .await;
    assert!(
        tx.frames().is_empty(),
        "a message queued 60 s ago under node 30 went out after the restart as node 40:\n  {}",
        describe(&tx.frames())
    );
}

// ===========================================================================
// Errors of the transmit half.
// ===========================================================================

/// A transient error on the second frame of a message: its rest is not sent,
/// and the next message starts cleanly.
#[tokio::test(start_paused = true)]
async fn a_transient_tx_error_abandons_only_its_message() {
    let node = Node::new(10, 30, 4096, CLASSIC);
    node.send(
        &header(to(10, 42), TrafficClass::Normal),
        &[0x5A5A_5A5A; 12],
    )
    .unwrap();
    node.send(&header(to(10, 43), TrafficClass::Normal), &[0x5A5A_5A5A; 4])
        .unwrap();
    let tx = ProbeTx::new(CLASSIC);
    *tx.fail.lock().unwrap() = Some((1, CanErrorKind::TxFailed));
    let res = node
        .run(
            ScriptedRx::idle(),
            tx.clone(),
            Capture::default(),
            CanConfig::new(0),
            50,
        )
        .await;
    let wire = tx.frames();
    assert!(
        res.is_none(),
        "a transient TX error must not stop the worker"
    );
    let to_42: Vec<_> = wire.iter().filter(|f| f.id.dst_node() == 42).collect();
    assert_eq!(
        to_42.len(),
        1,
        "only the first frame of the abandoned message"
    );
    let to_43: Vec<_> = wire.iter().filter(|f| f.id.dst_node() == 43).collect();
    assert_eq!(to_43[0].id.idx(), 0);
    assert_eq!(to_43.last().unwrap().id.end(), Some(FrameEnd::Last));
}

/// A fatal error stops the worker, takes the interface down and leaves
/// nothing queued for the next run.
#[tokio::test(start_paused = true)]
async fn a_fatal_tx_error_stops_the_link_and_empties_its_queues() {
    for class in [TrafficClass::Normal, TrafficClass::Control] {
        let node = Node::new(10, 30, 4096, CLASSIC);
        node.send(&header(to(10, 42), class), &[1]).unwrap();
        node.send(&header(to(10, 42), class), &[2]).unwrap();
        let tx = ProbeTx::new(CLASSIC);
        *tx.fail.lock().unwrap() = Some((0, CanErrorKind::Stopped));
        let res = node
            .run(
                ScriptedRx::idle(),
                tx.clone(),
                Capture::default(),
                CanConfig::new(0),
                50,
            )
            .await;
        assert!(matches!(res, Some(Err(CanWorkerError::Tx(_)))));
        assert_eq!(node.state(), InterfaceState::Down);
        node.set_state(InterfaceState::Active {
            net_id: 10,
            node_id: 30,
        });
        let again = ProbeTx::new(CLASSIC);
        node.run(
            ScriptedRx::idle(),
            again.clone(),
            Capture::default(),
            CanConfig::new(0),
            50,
        )
        .await;
        assert!(
            again.frames().is_empty(),
            "{class:?}: the next run sent what the stopped one had left:\n  {}",
            describe(&again.frames())
        );
    }
}

// ===========================================================================
// `set_node_filter` fails once, transiently, when the node changes. The
// worker sets the filter again, so the controller does not stay on the node
// before.
// ===========================================================================

/// Models the hardware filter: frames that do not pass the filter last set
/// successfully never reach `recv()`.
struct HwFilterRx {
    frames: VecDeque<(u64, CanFrame)>,
    start: Instant,
    hw: Option<u8>,
    set_calls: usize,
    fail_call: usize,
}

impl CanRx for HwFilterRx {
    type Error = TestError;
    async fn recv(&mut self) -> Result<CanFrame, TestError> {
        loop {
            let Some(&(at, _)) = self.frames.front() else {
                return pending().await;
            };
            sleep_until(self.start + Duration::from_millis(at)).await;
            let (_, f) = self.frames.pop_front().unwrap();
            let dst = f.id.dst_node();
            if self.hw.is_none_or(|n| dst == n || dst == 0xFF) {
                return Ok(f);
            }
        }
    }
    fn set_node_filter(&mut self, node: Option<u8>) -> Result<(), TestError> {
        self.set_calls += 1;
        if self.set_calls == self.fail_call {
            return Err(TestError(CanErrorKind::Bus));
        }
        self.hw = node;
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn a_transient_filter_error_does_not_leave_the_node_deaf() {
    let node = Node::new(10, 30, 4096, CLASSIC);
    let mut frames = VecDeque::new();
    for (i, at) in [30u64, 60, 90].into_iter().enumerate() {
        let mut out = None;
        fragment(
            &[0x77; 6],
            CLASSIC as usize,
            NORMAL,
            40,
            CENTRAL_NODE_ID,
            i as u8 + 1,
            |f| {
                out = Some(*f);
                Ok(())
            },
        )
        .unwrap();
        frames.push_back((at, out.unwrap()));
    }
    let rx = HwFilterRx {
        frames,
        start: Instant::now(),
        hw: None,
        set_calls: 0,
        // The call for node 30 succeeds, the one for node 40 fails once.
        fail_call: 2,
    };
    let got = Capture::default();
    let change = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        node.set_state(InterfaceState::Active {
            net_id: 10,
            node_id: 40,
        });
    };
    tokio::join!(
        node.run(
            rx,
            ProbeTx::new(CLASSIC),
            got.clone(),
            CanConfig::new(0),
            200
        ),
        change
    );
    assert_eq!(got.0.lock().unwrap().len(), 3);
}

// ===========================================================================
// Frames already waiting in the controller when the worker starts are
// filtered for the node the interface has by then.
// ===========================================================================

struct WaitingRx {
    frames: VecDeque<CanFrame>,
    order: Arc<Mutex<Vec<String>>>,
}

impl CanRx for WaitingRx {
    type Error = TestError;
    async fn recv(&mut self) -> Result<CanFrame, TestError> {
        match self.frames.pop_front() {
            Some(f) => {
                self.order
                    .lock()
                    .unwrap()
                    .push(format!("recv(dst {})", f.id.dst_node()));
                Ok(f)
            }
            None => pending().await,
        }
    }
    fn set_node_filter(&mut self, node: Option<u8>) -> Result<(), TestError> {
        self.order
            .lock()
            .unwrap()
            .push(format!("set_node_filter({node:?})"));
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn frames_waiting_at_start_are_filtered_like_any_other() {
    // This interface is node 30 before the worker starts.
    let node = Node::new(10, 30, 4096, CLASSIC);
    // Three single-frame messages for node 42 sit in the controller.
    let frames = (1..=3u8)
        .map(|tid| {
            let mut out = None;
            fragment(
                &[0x77; 6],
                CLASSIC as usize,
                NORMAL,
                42,
                CENTRAL_NODE_ID,
                tid,
                |f| {
                    out = Some(*f);
                    Ok(())
                },
            )
            .unwrap();
            out.unwrap()
        })
        .collect();
    let order = Arc::new(Mutex::new(Vec::new()));
    let rx = WaitingRx {
        frames,
        order: order.clone(),
    };
    let got = Capture::default();
    node.run(
        rx,
        ProbeTx::new(CLASSIC),
        got.clone(),
        CanConfig::new(0),
        50,
    )
    .await;
    assert_eq!(
        got.0.lock().unwrap().len(),
        0,
        "messages for node 42 reached node 30's processor; adapter calls: {:?}",
        order.lock().unwrap()
    );
}

// ===========================================================================
// A receiver sees only the starts addressed to it or to everyone. `tid`
// counts per destination, so the two messages for this node differ in it
// whatever went to other nodes between them, and the second is not taken
// for CAN's repeat of the first.
// ===========================================================================

async fn burst(gap_frames_per_ms: u64, extra_delay_ms: u64) -> (Vec<CanFrame>, usize) {
    let sender = Node::new(10, 30, 16384, CLASSIC);
    sender
        .send(&header(to(10, CENTRAL_NODE_ID), TrafficClass::Normal), &[1])
        .unwrap();
    for i in 0..7u32 {
        sender
            .send(&header(to(10, 42), TrafficClass::Normal), &[i])
            .unwrap();
    }
    sender
        .send(&header(to(10, CENTRAL_NODE_ID), TrafficClass::Normal), &[2])
        .unwrap();
    let tx = ProbeTx::new(CLASSIC);
    sender
        .run(
            ScriptedRx::idle(),
            tx.clone(),
            Capture::default(),
            CanConfig::new(0),
            50,
        )
        .await;
    let wire = tx.frames();

    let receiver = Node::new(10, CENTRAL_NODE_ID, 4096, CLASSIC);
    let got = Capture::default();
    let last_start = wire.iter().rposition(|f| f.id.idx() == 0).unwrap();
    let script = wire
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let t =
                2 + i as u64 / gap_frames_per_ms + if i >= last_start { extra_delay_ms } else { 0 };
            (t, *f)
        })
        .collect();
    receiver
        .run(
            ScriptedRx::new(script, true),
            ProbeTx::new(CLASSIC),
            got.clone(),
            CanConfig::new(0),
            1000,
        )
        .await;
    let n = got.0.lock().unwrap().len();
    (wire, n)
}

#[tokio::test(start_paused = true)]
async fn a_new_message_is_not_taken_for_a_repeat() {
    // The ninth message starts more than `duplicate_ms` after the first.
    let (_, spaced) = burst(4, 25).await;
    assert_eq!(spaced, 2);
    // Nine messages back to back, 4 frames a millisecond: the first and the
    // ninth for node 1, the seven between for node 42.
    let (wire, got) = burst(4, 0).await;
    assert_eq!(
        got,
        2,
        "the second message for node 1 was dropped; on the wire:\n  {}",
        describe(&wire)
    );
}

/// One sender serves `dests` nodes in turn, one Normal message each per
/// cycle. Returns how many of `cycles` messages node 40 was handed.
async fn round_robin(period_ms: u64, cycles: u32, dests: u8) -> usize {
    let sender = Node::new(10, 30, 16384, CLASSIC);
    let tx = ProbeTx::new(CLASSIC);
    let feed = async {
        for cycle in 0..cycles {
            for d in 0..dests {
                sender
                    .send(&header(to(10, 40 + d), TrafficClass::Normal), &[cycle])
                    .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(period_ms)).await;
        }
    };
    tokio::join!(
        sender.run(
            ScriptedRx::idle(),
            tx.clone(),
            Capture::default(),
            CanConfig::new(0),
            period_ms * cycles as u64 + 50,
        ),
        feed
    );
    let wire = tx.timed();
    assert_eq!(
        wire.iter()
            .filter(|(_, f)| f.id.idx() == 0 && f.id.dst_node() == 40)
            .count(),
        cycles as usize,
        "the sender sent node 40 one message per cycle"
    );

    let receiver = Node::new(10, 40, 4096, CLASSIC);
    let got = Capture::default();
    // Same timing as sent, 2 ms later.
    let script = wire.iter().map(|(t, f)| (t + 2, *f)).collect();
    receiver
        .run(
            ScriptedRx::new(script, true),
            ProbeTx::new(CLASSIC),
            got.clone(),
            CanConfig::new(0),
            10_000,
        )
        .await;
    got.0.lock().unwrap().len()
}

#[tokio::test(start_paused = true)]
async fn serving_several_nodes_in_turn_loses_nothing() {
    for (period, dests) in [(10u64, 8u8), (20, 8), (25, 8), (10, 7), (10, 4), (10, 16)] {
        let got = round_robin(period, 30, dests).await;
        assert_eq!(
            got, 30,
            "{dests} destinations, one message each every {period} ms: node 40 got {got} of 30"
        );
    }
}

// ===========================================================================
// An overrun reported in the middle of a message: the message is dropped,
// and frames that fit behind what was assembled do not complete it.
// ===========================================================================

struct ListedRx(VecDeque<Result<CanFrame, TestError>>);

impl CanRx for ListedRx {
    type Error = TestError;
    async fn recv(&mut self) -> Result<CanFrame, TestError> {
        match self.0.pop_front() {
            Some(next) => next,
            None => pending().await,
        }
    }
}

#[tokio::test(start_paused = true)]
async fn an_overrun_drops_what_was_assembling() {
    let mut frames = Vec::new();
    fragment(
        &[0x77; 20],
        CLASSIC as usize,
        NORMAL,
        30,
        CENTRAL_NODE_ID,
        1,
        |f| {
            frames.push(*f);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(frames.len(), 3);
    for overrun in [false, true] {
        let mut script = VecDeque::from([Ok(frames[0])]);
        if overrun {
            script.push_back(Err(TestError(CanErrorKind::Overrun)));
        }
        script.extend([Ok(frames[1]), Ok(frames[2])]);
        let node = Node::new(10, 30, 4096, CLASSIC);
        let got = Capture::default();
        node.run(
            ListedRx(script),
            ProbeTx::new(CLASSIC),
            got.clone(),
            CanConfig::new(0),
            50,
        )
        .await;
        assert_eq!(got.0.lock().unwrap().len(), usize::from(!overrun));
    }
}

// ===========================================================================
// Queue sizes.
// ===========================================================================

/// A queue of the smallest size the sink takes goes on taking sends,
/// wherever earlier ones have left its read and write positions.
#[tokio::test(start_paused = true)]
async fn the_smallest_queue_keeps_taking_sends() {
    let queue_len = Sink::<StdQueue, 256>::min_queue_len(CLASSIC);
    let node = Node::new(10, 30, queue_len, CLASSIC);
    for n in 0..300u32 {
        // The queue is empty before each send.
        node.send(&header(to(10, 42), TrafficClass::Normal), &[n; 3])
            .unwrap_or_else(|e| panic!("send {n} refused by an empty queue: {e:?}"));
        let tx = ProbeTx::new(CLASSIC);
        node.run(
            ScriptedRx::idle(),
            tx.clone(),
            Capture::default(),
            CanConfig::new(0),
            10,
        )
        .await;
        assert!(!tx.frames().is_empty());
        node.set_state(InterfaceState::Active {
            net_id: 10,
            node_id: 30,
        });
    }
}

/// A smaller queue could be left where it refuses every send while empty,
/// so the sink does not take one.
#[test]
#[should_panic(expected = "twice its largest entry")]
fn a_queue_too_small_to_always_take_a_send_is_refused() {
    let queue_len = Sink::<StdQueue, 256>::min_queue_len(CLASSIC) - 1;
    let _ = Sink::<StdQueue, 256>::new(new_std_queue(queue_len), new_std_queue(queue_len), CLASSIC);
}
