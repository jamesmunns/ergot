//! The packet worker's liveness and state notifications. Liveness used to
//! exist only with `embassy-time`; it runs on any time backend, here tokio's
//! paused test time.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use std::{
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bbqueue::traits::bbqhdl::BbqHandle;
use ergot::{
    Address,
    interface_manager::{
        FrameProcessor, Interface, InterfaceSendError, InterfaceState, LivenessConfig, Profile,
        profiles::direct_edge::DirectEdge,
        transports::packet::{PacketReceiver, PacketRxTxWorker, PacketSender},
        utils::{
            framed_stream,
            std::{StdQueue, new_std_queue},
        },
    },
    net_stack::{ArcNetStack, NetStackHandle, NetStackSendError},
    topic,
};
use maitake_sync::WaitQueue;
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::{
    sync::mpsc,
    time::{sleep, timeout},
};

topic!(Probe, u32, "ergot/test/probe");

struct TestInterface;
impl Interface for TestInterface {
    type Sink = framed_stream::Sink<StdQueue>;
}

type Stack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<TestInterface>>;

/// Frames pushed into a channel. Channel receives are cancel-safe.
struct ChannelRx(mpsc::UnboundedReceiver<Vec<u8>>);

impl PacketReceiver for ChannelRx {
    type Error = ();

    async fn recv(&mut self, buf: &mut [u8]) -> Result<usize, ()> {
        let frame = self.0.recv().await.ok_or(())?;
        buf[..frame.len()].copy_from_slice(&frame);
        Ok(frame.len())
    }
}

struct DiscardTx;

impl PacketSender for DiscardTx {
    type Error = ();

    async fn send(&mut self, _data: &[u8]) -> Result<(), ()> {
        Ok(())
    }
}

/// Counts frames; changes no state itself.
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

const ACTIVE: InterfaceState = InterfaceState::Active {
    net_id: 1,
    node_id: 2,
};

fn stack() -> (Stack, StdQueue) {
    let queue = new_std_queue(1024);
    let stack = Stack::new_with_profile(DirectEdge::new_target(
        framed_stream::Sink::new_from_handle(queue.clone(), 256),
    ));
    (stack, queue)
}

fn state(stack: &Stack) -> Option<InterfaceState> {
    stack.manage_profile(|im| im.interface_state(()))
}

/// The liveness timer arms at the first frame, and a silence past the
/// timeout takes the interface Inactive and tells the observers.
#[tokio::test(start_paused = true)]
async fn liveness_times_out_after_the_last_frame() {
    static STATE: WaitQueue = WaitQueue::new();
    let (stack, queue) = stack();
    let (frames, rx) = mpsc::unbounded_channel();
    let count = Arc::new(AtomicUsize::new(0));
    let mut worker = PacketRxTxWorker::new(
        stack.clone(),
        ChannelRx(rx),
        DiscardTx,
        Count(count.clone()),
        (),
        queue.framed_consumer(),
    )
    .with_liveness(LivenessConfig { timeout_ms: 500 })
    .with_state_notify(&STATE);

    let mut scratch = [0u8; 64];
    let run = worker.run(ACTIVE, &mut scratch);
    let drive = async {
        // Not armed before the first frame.
        sleep(Duration::from_secs(10)).await;
        assert_eq!(state(&stack), Some(ACTIVE));

        frames.send(vec![1, 2, 3]).unwrap();
        sleep(Duration::from_millis(400)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(state(&stack), Some(ACTIVE));

        let notified = STATE.wait();
        sleep(Duration::from_millis(200)).await;
        assert_eq!(state(&stack), Some(InterfaceState::Inactive));
        assert!(
            timeout(Duration::from_millis(1), notified).await.is_ok(),
            "observers were not told"
        );
    };
    tokio::select! {
        res = run => panic!("worker ended: {res:?}"),
        () = drive => {}
    }
}

/// By default a liveness timeout takes the interface `Inactive`, which gates
/// transmit. An upstream that opts into reverting to link-local keeps its
/// node_id and can still send, e.g. the link-local ping that provokes the
/// frame re-discovering its net_id.
#[tokio::test(start_paused = true)]
async fn liveness_timeout_can_revert_to_link_local() {
    // Link-local, so it still routes out of a link-local interface.
    let peer = Address {
        network_id: 0,
        node_id: 1,
        port_id: 1,
    };
    for link_local in [false, true] {
        let (stack, queue) = stack();
        let (frames, rx) = mpsc::unbounded_channel();
        let mut worker = PacketRxTxWorker::new(
            stack.clone(),
            ChannelRx(rx),
            DiscardTx,
            Count(Arc::default()),
            (),
            queue.framed_consumer(),
        )
        .with_liveness(LivenessConfig { timeout_ms: 500 });
        if link_local {
            worker = worker.revert_to_link_local_on_timeout();
        }

        let mut scratch = [0u8; 64];
        let run = worker.run(ACTIVE, &mut scratch);
        let drive = async {
            frames.send(vec![1, 2, 3]).unwrap();
            sleep(Duration::from_millis(600)).await;
            let sent = stack.topics().unicast::<Probe>(peer, &7);
            if link_local {
                assert_eq!(state(&stack), Some(InterfaceState::link_local(2)));
                assert_eq!(sent, Ok(()));
            } else {
                assert_eq!(state(&stack), Some(InterfaceState::Inactive));
                assert_eq!(
                    sent,
                    Err(NetStackSendError::InterfaceSend(
                        InterfaceSendError::NoRouteToDest
                    ))
                );
            }
        };
        tokio::select! {
            res = run => panic!("worker ended: {res:?}"),
            () = drive => {}
        }
    }
}

/// The deadline runs from the last received frame: a steady stream of
/// outgoing frames, each sent well within the timeout, must not keep a link
/// that has gone quiet looking alive.
#[tokio::test(start_paused = true)]
async fn transmitting_does_not_postpone_liveness() {
    let (stack, queue) = stack();
    let (frames, rx) = mpsc::unbounded_channel();
    let mut worker = PacketRxTxWorker::new(
        stack.clone(),
        ChannelRx(rx),
        DiscardTx,
        Count(Arc::default()),
        (),
        queue.framed_consumer(),
    )
    .with_liveness(LivenessConfig { timeout_ms: 500 });

    let mut scratch = [0u8; 64];
    let run = worker.run(ACTIVE, &mut scratch);
    let drive = async {
        frames.send(vec![1, 2, 3]).unwrap();
        let producer = queue.framed_producer::<u16>();
        // Something to send every 100 ms, for 700 ms after the last frame in.
        for _ in 0..7 {
            sleep(Duration::from_millis(100)).await;
            let mut grant = producer.grant(4).unwrap();
            grant.copy_from_slice(&[9; 4]);
            grant.commit(4);
        }
        assert_eq!(state(&stack), Some(InterfaceState::Inactive));
    };
    tokio::select! {
        res = run => panic!("worker ended: {res:?}"),
        () = drive => {}
    }
}

/// A worker dropped while its interface is up (its task was cancelled) takes
/// the interface Down and tells the observers.
#[tokio::test]
async fn dropping_a_worker_sets_its_interface_down() {
    static STATE: WaitQueue = WaitQueue::new();
    let (stack, queue) = stack();
    let (_frames, rx) = mpsc::unbounded_channel();
    let mut worker = PacketRxTxWorker::new(
        stack.clone(),
        ChannelRx(rx),
        DiscardTx,
        Count(Arc::default()),
        (),
        BbqHandle::framed_consumer(&queue),
    )
    .with_state_notify(&STATE);

    let mut scratch = [0u8; 64];
    let _ = timeout(Duration::from_millis(10), worker.run(ACTIVE, &mut scratch)).await;
    assert_eq!(state(&stack), Some(ACTIVE));

    let mut notified = pin!(STATE.wait());
    // Register the waiter before the drop.
    assert!(
        timeout(Duration::from_millis(1), notified.as_mut())
            .await
            .is_err()
    );
    drop(worker);
    assert_eq!(state(&stack), Some(InterfaceState::Down));
    assert!(
        timeout(Duration::from_millis(100), notified).await.is_ok(),
        "observers were not told"
    );
}
