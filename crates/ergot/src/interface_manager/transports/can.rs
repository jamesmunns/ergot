//! CAN RX/TX worker.
//!
//! Drives one CAN interface through a [`CanRx`] and a [`CanTx`] half (a
//! classic CAN or CAN FD controller adapter): receives frames, reassembles
//! messages with the [`Reassembler`] and hands complete ergot frames to the
//! [`FrameProcessor`]; takes whole ergot frames from the two outgoing queues
//! filled by [`utils::can::Sink`], Control first, and fragments them as it
//! transmits. See `notes/2026-09-28-can-transport.md`.
//!
//! Receiving and transmitting run as two independent loops in one task, so a
//! message waiting for the bus never stops the receive side from draining
//! the controller's (often very small) RX FIFO. Timeouts come from the
//! [time backend](crate::time).

use core::convert::Infallible;
use core::future::{Future, pending};
use core::pin::pin;
use core::sync::atomic::{AtomicU8, Ordering};

use bbqueue::prod_cons::framed::FramedConsumer;
use bbqueue::traits::bbqhdl::BbqHandle;
use bbqueue::traits::notifier::AsyncNotifier;
use embassy_futures::select::{Either, Either3, select, select3};
use maitake_sync::WaitQueue;

use crate::interface_manager::transports::link::Link;
use crate::interface_manager::utils::can::{
    BROADCAST_NODE_ID, CanFrame, DropReason, Fragmenter, Push, QueuedFrame, Reassembler,
    ReassemblyTimeouts, TidCounters, is_max_payload,
};
use crate::interface_manager::{FrameProcessor, InterfaceState, Profile};
use crate::logging::{debug, trace, warn};
use crate::net_stack::NetStackHandle;
use crate::time::{Duration, Instant, TimedOut, sleep, sleep_until, with_timeout};

#[allow(unused_imports)]
use crate::interface_manager::utils;

/// What kind of failure a CAN adapter reports, which decides what the worker
/// does about it. Adapters map their controller's errors onto these.
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CanErrorKind {
    /// Received frames were lost, or may have been: an RX FIFO overrun, or
    /// a controller that was off the bus for a while. The adapter reports it
    /// before any frame received after the loss, and after it has discarded
    /// the frames received before it that it still held. The worker drops
    /// every message it was assembling and carries on.
    Overrun,
    /// A bus error or error-state change the controller recovers from on its
    /// own: bit, stuff, CRC, form or ACK errors, error-passive, bus-off with
    /// automatic recovery. The worker carries on.
    Bus,
    /// One frame was not sent (retransmission limit, aborted, no free TX
    /// buffer). The worker abandons that message and carries on.
    TxFailed,
    /// The controller is off the bus and will not come back by itself. The
    /// worker stops.
    Stopped,
    /// Anything else. The worker stops.
    Other,
}

impl CanErrorKind {
    /// Whether the worker has to stop.
    pub const fn is_fatal(self) -> bool {
        matches!(self, CanErrorKind::Stopped | CanErrorKind::Other)
    }
}

/// An error from a CAN adapter.
pub trait CanError: core::fmt::Debug {
    /// What kind of failure this is. The default, [`CanErrorKind::Other`],
    /// stops the worker.
    fn kind(&self) -> CanErrorKind {
        CanErrorKind::Other
    }
}

impl CanError for () {}

impl CanError for Infallible {
    fn kind(&self) -> CanErrorKind {
        match *self {}
    }
}

/// The transmit half of a CAN controller.
pub trait CanTx {
    type Error: CanError;

    /// Largest payload this link carries: 8 for classic CAN, a CAN FD frame
    /// length (typically 64) otherwise. Must match the `max_payload` of the
    /// interface's [`utils::can::Sink`].
    fn max_payload(&self) -> u8;

    /// Hand one frame to the controller.
    ///
    /// The future completes once the frame has been **accepted for
    /// transmission in order** behind anything this worker sent before it.
    /// This is the TX contract reassembly relies on: frames of one sender
    /// arrive in the order they were handed over (on bxCAN, set the transmit
    /// FIFO priority mode, `TXFP`). Adapters for controllers that evict a
    /// lower-priority mailbox must requeue the evicted frame.
    ///
    /// Every frame already has a length a controller sends as is (padding is
    /// done by the worker), so the DLC is simply the frame's `len`.
    ///
    /// The worker drops the future when the TX timeout expires, or when a
    /// Control message has waited long enough behind a frame of another
    /// class, and abandons the rest of that message. The frame may or may
    /// not have gone out by then, but it must not go out later: an adapter
    /// that holds the frame when its future is dropped aborts it. A frame
    /// that left after the next message began would end that message at the
    /// receiver.
    fn send(&mut self, frame: &CanFrame) -> impl Future<Output = Result<(), Self::Error>>;
}

/// The receive half of a CAN controller.
pub trait CanRx {
    type Error: CanError;

    /// Receive one frame.
    ///
    /// Must be cancel-safe: the worker races it against changes of this
    /// interface's node, and a frame must not be lost when that wins.
    fn recv(&mut self) -> impl Future<Output = Result<CanFrame, Self::Error>>;

    /// Accept at least the frames addressed to `node` and to
    /// [`BROADCAST_NODE_ID`], or every frame for `None`: program the
    /// controller's acceptance filters, e.g. with
    /// [`CanId::dst_filter`](utils::can::CanId::dst_filter) for
    /// both. Accepting more is fine, the worker filters in software too.
    ///
    /// The worker calls it once at start and whenever this interface's node
    /// changes (a granted or restored address claim), from the receive loop,
    /// which runs before the transmit loop: a new node's filter is in place
    /// before the first frame from that node goes out, so its replies are
    /// not lost. After an error the worker can go on from, it calls again
    /// [`NODE_FILTER_RETRY_MS`] later: the controller may still be passing
    /// only the frames of the node before.
    ///
    /// The default accepts everything, which costs the CPU the frames meant
    /// for other nodes. Controllers that cannot change their filters while
    /// on the bus are best left at that.
    fn set_node_filter(&mut self, node: Option<u8>) -> Result<(), Self::Error> {
        let _ = node;
        Ok(())
    }
}

/// How long the worker waits for the controller to accept one frame, by
/// default, before it abandons the message.
pub const DEFAULT_TX_TIMEOUT_MS: u64 = 100;

/// How long the worker waits for the controller to accept a frame once one
/// has timed out, until a frame is accepted again. While nothing is accepted
/// (a node alone on the bus, a cable pulled), each message then costs this
/// long instead of the whole TX timeout, so what the queues hold when the
/// bus comes back was queued a moment ago, not seconds ago.
pub const STUCK_TX_TIMEOUT_MS: u64 = 10;

/// How long a Control message waits, by default, for a frame of another
/// class that the controller has not accepted.
pub const DEFAULT_CONTROL_PATIENCE_MS: u64 = 5;

/// How long the receive loop waits before it asks the adapter again when
/// two non-fatal errors came with no frame between them, so an adapter that
/// keeps reporting one (e.g. while the controller is error-passive) cannot
/// starve the transmit loop and the rest of the task. An error that stands
/// alone costs no wait: a pause in receiving is what overruns a small RX
/// FIFO.
pub const RX_ERROR_BACKOFF_MS: u64 = 1;

/// How long after a non-fatal failure of [`CanRx::set_node_filter`] the
/// worker calls it again.
pub const NODE_FILTER_RETRY_MS: u64 = 10;

/// Worker configuration.
#[derive(Debug, Clone, Copy)]
pub struct CanConfig {
    /// How long a partial message may sit without progress, per class, and
    /// the window for recognising CAN's retransmitted duplicates.
    pub reassembly: ReassemblyTimeouts,
    /// Where the message counters (`tid`) start. Take it from an
    /// RNG or a boot counter, so a restarted node does not repeat the `tid`s
    /// of its previous life: a receiver may still hold a stalled message from
    /// before the reboot, which a new message with the same `tid` and a lost
    /// first frame would complete with the wrong tail.
    pub tid_seed: u8,
    /// How long to wait for the controller to accept a frame before the
    /// message is abandoned. `None` waits forever — then a node alone on the
    /// bus (no ACKs, the controller retries indefinitely) never frees its
    /// queues.
    pub tx_timeout_ms: Option<u64>,
    /// How long a Control message waits for a frame of another class that
    /// the controller has not accepted, before the worker gives that
    /// message up and sends the Control one. A frame of the lowest priority
    /// can wait for the bus as long as other nodes send, and Control would
    /// wait with it. `None` lets it wait, up to the TX timeout.
    pub control_patience_ms: Option<u64>,
}

impl CanConfig {
    /// Default timeouts with the given counter seed.
    pub const fn new(tid_seed: u8) -> Self {
        Self {
            reassembly: ReassemblyTimeouts::DEFAULT,
            tid_seed,
            tx_timeout_ms: Some(DEFAULT_TX_TIMEOUT_MS),
            control_patience_ms: Some(DEFAULT_CONTROL_PATIENCE_MS),
        }
    }
}

/// Error returned by [`CanRxTxWorker::run`]: a fatal adapter error.
#[derive(Debug)]
pub enum CanWorkerError<RxE: core::fmt::Debug, TxE: core::fmt::Debug> {
    Rx(RxE),
    Tx(TxE),
}

/// Combined RX/TX worker for one CAN interface.
///
/// `K` reassembly slots of `MTU` bytes each. A worker dropped while its
/// interface is up (its task was cancelled) takes the interface Down and
/// empties its queues, as the end of [`Self::run`] does.
pub struct CanRxTxWorker<N, Rx, Tx, Q, P, const K: usize, const MTU: usize>
where
    N: NetStackHandle,
    Rx: CanRx,
    Tx: CanTx,
    Q: BbqHandle,
    Q::Notifier: AsyncNotifier,
    P: FrameProcessor<N>,
{
    link: Link<N>,
    rx: Rx,
    tx: Tx,
    processor: P,
    control: FramedConsumer<Q>,
    bulk: FramedConsumer<Q>,
    reassembler: Reassembler<K, MTU>,
    tids: TidCounters,
    tx_timeout_ms: Option<u64>,
    control_patience_ms: Option<u64>,
}

impl<N, Rx, Tx, Q, P, const K: usize, const MTU: usize> CanRxTxWorker<N, Rx, Tx, Q, P, K, MTU>
where
    N: NetStackHandle,
    Rx: CanRx,
    Tx: CanTx,
    Q: BbqHandle,
    Q::Notifier: AsyncNotifier,
    P: FrameProcessor<N>,
{
    /// `control` / `bulk` are the consumer halves of the two queues given to
    /// the matching [`utils::can::Sink`].
    ///
    /// Panics if `tx.max_payload()` is not usable ([`is_max_payload`]).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        nsh: N,
        rx: Rx,
        tx: Tx,
        processor: P,
        ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
        control: FramedConsumer<Q>,
        bulk: FramedConsumer<Q>,
        config: CanConfig,
    ) -> Self {
        assert!(
            is_max_payload(tx.max_payload() as usize),
            "CAN max_payload must be 8 or a CAN FD frame length"
        );
        Self {
            link: Link::new(nsh, ident),
            rx,
            tx,
            processor,
            control,
            bulk,
            reassembler: Reassembler::new(config.reassembly),
            tids: TidCounters::new(config.tid_seed),
            tx_timeout_ms: config.tx_timeout_ms,
            control_patience_ms: config.control_patience_ms,
        }
    }

    /// Set a [`WaitQueue`] woken whenever this worker changes its
    /// interface's state (a frame activates it, or the worker stopping takes
    /// it down). Changes made elsewhere, such as the bus address claim, do
    /// not reach it; wait with
    /// [`NetStack::wait_profile`](crate::NetStack::wait_profile) to see
    /// every change.
    ///
    /// The queue uses maitake's default mutex, which on `no_std` is a plain
    /// spinlock unless `maitake-sync/critical-section` is enabled. Without
    /// it, this worker and the queue's waiters must not preempt each other.
    pub fn with_state_notify(mut self, notify: &'static WaitQueue) -> Self {
        self.link.set_state_notify(notify);
        self
    }

    /// Run until an adapter reports a fatal error. Sets `initial_state` first
    /// and leaves the interface `Down` on exit, with its queues empty: what
    /// they still held was meant for the link as it was, and would go out
    /// whenever the link is next run.
    pub async fn run(
        &mut self,
        initial_state: InterfaceState,
    ) -> Result<(), CanWorkerError<Rx::Error, Tx::Error>> {
        self.link.set_state(initial_state);
        let res = self.run_inner().await;
        self.stop();
        res
    }

    /// Take the interface down, then drop what its queues hold. In this
    /// order nothing is queued behind the drain.
    fn stop(&mut self) {
        self.link.set_down();
        for queue in [&self.control, &self.bulk] {
            while let Ok(entry) = queue.read() {
                entry.release();
            }
        }
    }

    async fn run_inner(&mut self) -> Result<(), CanWorkerError<Rx::Error, Tx::Error>> {
        let Self {
            link,
            rx,
            tx,
            processor,
            control,
            bulk,
            reassembler,
            tids,
            tx_timeout_ms,
            control_patience_ms,
        } = self;
        // This interface's node as the receive loop last saw it, for the
        // transmit loop.
        let node = AtomicU8::new(0);
        // `select` polls the receive loop first: a node change it picks up
        // reaches the filters before the transmit loop can send from the new
        // node (see `CanRx::set_node_filter`).
        let rx_side = rx_loop(link, rx, processor, reassembler, &node);
        let out = Transmitter {
            tx,
            tids,
            timeout_ms: *tx_timeout_ms,
            patience_ms: *control_patience_ms,
            node: &node,
            stuck: false,
        };
        let tx_side = tx_loop(out, control, bulk);
        match select(rx_side, tx_side).await {
            Either::First(Err(e)) => Err(CanWorkerError::Rx(e)),
            Either::Second(Err(e)) => Err(CanWorkerError::Tx(e)),
            Either::First(Ok(never)) | Either::Second(Ok(never)) => match never {},
        }
    }
}

impl<N, Rx, Tx, Q, P, const K: usize, const MTU: usize> Drop
    for CanRxTxWorker<N, Rx, Tx, Q, P, K, MTU>
where
    N: NetStackHandle,
    Rx: CanRx,
    Tx: CanTx,
    Q: BbqHandle,
    Q::Notifier: AsyncNotifier,
    P: FrameProcessor<N>,
{
    fn drop(&mut self) {
        self.stop();
    }
}

async fn rx_loop<N, Rx, P, const K: usize, const MTU: usize>(
    link: &Link<N>,
    rx: &mut Rx,
    processor: &mut P,
    reassembler: &mut Reassembler<K, MTU>,
    seen_node: &AtomicU8,
) -> Result<Infallible, Rx::Error>
where
    N: NetStackHandle,
    Rx: CanRx,
    P: FrameProcessor<N>,
{
    let stack = link.nsh.stack();
    // Resolves with this interface's node on the segment once it differs from
    // `current` (0 while unknown: node 0 is never a segment address). The
    // first one resolves right away with the node the worker started with.
    let node_change = |current: u8| {
        let ident = link.ident.clone();
        stack.wait_profile(move |im| {
            let node = im.interface_node_id(ident.clone()).unwrap_or(0);
            (node != current).then_some(node)
        })
    };
    let mut node = 0;
    // Kept across frames: a fresh wait would take the profile lock per frame.
    let mut next_node = pin!(node_change(node));
    // When to set the node filter again, after the adapter failed to.
    let mut filter_retry: Option<Instant> = None;
    // Whether the adapter's last answer was an error, and until when not to
    // ask it again after a second one.
    let mut failing = false;
    let mut quiet_until: Option<Instant> = None;
    loop {
        // A node change comes first: the frames already waiting are filtered
        // for the node the worker starts with, a stream of errors cannot keep
        // the filter from following the node, and it is in place before the
        // transmit loop sends from the new node.
        let event = {
            let retry = async {
                match filter_retry {
                    Some(at) => sleep_until(at).await,
                    None => pending().await,
                }
            };
            let recv = async {
                if let Some(at) = quiet_until {
                    sleep_until(at).await;
                }
                rx.recv().await
            };
            select3(next_node.as_mut(), retry, recv).await
        };
        let frame = match event {
            Either3::First(new) => {
                node = new;
                seen_node.store(node, Ordering::Relaxed);
                next_node.set(node_change(node));
                filter_retry = set_node_filter(rx, node)?;
                continue;
            }
            Either3::Second(()) => {
                filter_retry = set_node_filter(rx, node)?;
                continue;
            }
            Either3::Third(Ok(frame)) => frame,
            Either3::Third(Err(e)) if e.kind().is_fatal() => return Err(e),
            Either3::Third(Err(e)) => {
                warn!("can rx: {:?}, continuing", e.kind());
                if e.kind() == CanErrorKind::Overrun {
                    // Which frames are missing is unknown, so nothing that
                    // was assembling can be trusted to continue.
                    reassembler.reset();
                }
                quiet_until = failing
                    .then(|| Instant::now().checked_add(Duration::from_millis(RX_ERROR_BACKOFF_MS)))
                    .flatten();
                failing = true;
                continue;
            }
        };
        failing = false;
        quiet_until = None;
        let now = Instant::now();
        reassembler.expire(now);
        if handle_rx(&frame, link, processor, reassembler, now, node) {
            link.notify();
        }
    }
}

/// Program the adapter's filters for `node` (0: none yet). Returns when to
/// try again if the adapter failed and can go on.
fn set_node_filter<Rx: CanRx>(rx: &mut Rx, node: u8) -> Result<Option<Instant>, Rx::Error> {
    match rx.set_node_filter((node != 0).then_some(node)) {
        Ok(()) => Ok(None),
        Err(e) if e.kind().is_fatal() => Err(e),
        Err(_e) => {
            // The software filter holds meanwhile, but the controller may
            // still pass only the frames of the node before.
            warn!("can rx: node filter not set: {:?}, retrying", _e.kind());
            Ok(Instant::now().checked_add(Duration::from_millis(NODE_FILTER_RETRY_MS)))
        }
    }
}

/// Filter, reassemble and deliver one received frame. Returns `true` if the
/// interface state changed.
fn handle_rx<N, P, const K: usize, const MTU: usize>(
    frame: &CanFrame,
    link: &Link<N>,
    processor: &mut P,
    reassembler: &mut Reassembler<K, MTU>,
    now: Instant,
    node: u8,
) -> bool
where
    N: NetStackHandle,
    P: FrameProcessor<N>,
{
    let id = frame.id;
    // Software filter: mine or everyone's, as the hardware filter set by
    // `CanRx::set_node_filter` (which may accept more). Until the node is
    // known at all, accept everything.
    //
    // Every frame is filtered, not only a message's first: the sender may
    // have given up on a message to this node, and the later frames of its
    // next one, to another node, must not continue it.
    if node != 0 && id.dst_node() != node && id.dst_node() != BROADCAST_NODE_ID {
        return false;
    }
    match reassembler.push(frame, now) {
        Push::Single(data) => processor.process_frame(data, &link.nsh, link.ident.clone()),
        Push::Complete(slot) => {
            let data = reassembler.finish(slot);
            processor.process_frame(data, &link.nsh, link.ident.clone())
        }
        Push::Pending => false,
        Push::Dropped(DropReason::NoMessage) => {
            trace!("can rx: continuation of no message assembling here");
            false
        }
        Push::Dropped(DropReason::Duplicate) => {
            trace!("can rx: repeated frame {:#x}", id.raw());
            false
        }
        Push::Dropped(_reason) => {
            debug!("can rx: frame dropped: {:?}", _reason);
            false
        }
    }
}

async fn tx_loop<Tx, Q>(
    mut out: Transmitter<'_, Tx>,
    control: &FramedConsumer<Q>,
    bulk: &FramedConsumer<Q>,
) -> Result<Infallible, Tx::Error>
where
    Tx: CanTx,
    Q: BbqHandle,
    Q::Notifier: AsyncNotifier,
{
    loop {
        // `select` polls in order, so a ready Control entry always wins over a
        // ready Bulk one.
        let entry = match select(control.wait_read(), bulk.wait_read()).await {
            Either::First(entry) => {
                let res = out.send_message(&entry).await;
                entry.release();
                res?;
                continue;
            }
            Either::Second(entry) => entry,
        };
        // A message from the Bulk queue. Control messages cut in between its
        // frames: another class, so the receiver assembles them apart. Within
        // a queue, messages never interleave — the receiver relies on it.
        if let Some(fragments) = out.begin_message(&entry) {
            for fragment in fragments {
                while let Ok(urgent) = control.read() {
                    let res = out.send_message(&urgent).await;
                    urgent.release();
                    res?;
                }
                if !out.send_frame_behind(&fragment, control).await? {
                    break;
                }
            }
        }
        entry.release();
    }
}

/// The transmit half and what the transmit loop keeps between frames.
struct Transmitter<'a, Tx> {
    tx: &'a mut Tx,
    tids: &'a mut TidCounters,
    timeout_ms: Option<u64>,
    patience_ms: Option<u64>,
    /// This interface's node, 0 while unknown.
    node: &'a AtomicU8,
    /// A frame timed out and none has been accepted since.
    stuck: bool,
}

impl<Tx: CanTx> Transmitter<'_, Tx> {
    /// Parse one queue entry, give it the next `tid` of its class and
    /// destination, and fragment it. `None` if the entry is not to be sent
    /// (logged).
    fn begin_message<'e>(&mut self, entry: &'e [u8]) -> Option<Fragmenter<'e>> {
        let Some(q) = QueuedFrame::parse(entry) else {
            warn!("can tx: malformed queue entry, dropping");
            return None;
        };
        // Queued from another node than this interface has now: the address
        // changed since, and may be another device's by now.
        let node = self.node.load(Ordering::Relaxed);
        if node != 0 && q.src_node != node {
            debug!(
                "can tx: dropping a frame queued as node {}, now {}",
                q.src_node, node
            );
            return None;
        }
        let tid = self.tids.next(q.prio, q.dst_node);
        Fragmenter::new(
            q.frame,
            self.tx.max_payload() as usize,
            q.prio,
            q.dst_node,
            q.src_node,
            tid,
        )
        .inspect_err(|_e| {
            warn!("can tx: cannot fragment a queued frame: {:?}", _e);
        })
        .ok()
    }

    /// Send every frame of one queue entry. A frame that is not accepted
    /// abandons the rest; the receiver drops the partial message when the
    /// sender's next message of that class starts, or when it times out.
    async fn send_message(&mut self, entry: &[u8]) -> Result<(), Tx::Error> {
        let Some(fragments) = self.begin_message(entry) else {
            return Ok(());
        };
        for fragment in fragments {
            if !self.send_frame(&fragment).await? {
                break;
            }
        }
        Ok(())
    }

    /// Hand one frame to the controller. `Ok(false)` if it was not accepted
    /// (TX timeout, or a non-fatal adapter error): the caller abandons the
    /// message.
    async fn send_frame(&mut self, frame: &CanFrame) -> Result<bool, Tx::Error> {
        trace!("can tx: id={:#x} len={}", frame.id.raw(), frame.len);
        let timeout_ms = match self.timeout_ms {
            Some(ms) if self.stuck => Some(ms.min(STUCK_TX_TIMEOUT_MS)),
            timeout_ms => timeout_ms,
        };
        let res = match timeout_ms {
            Some(ms) => match with_timeout(Duration::from_millis(ms), self.tx.send(frame)).await {
                Ok(res) => res,
                Err(TimedOut) => {
                    if self.stuck {
                        debug!("can tx: still not accepted, abandoning message");
                    } else {
                        warn!("can tx: not accepted within {} ms, abandoning message", ms);
                    }
                    self.stuck = true;
                    return Ok(false);
                }
            },
            None => self.tx.send(frame).await,
        };
        match res {
            Ok(()) => {
                self.stuck = false;
                Ok(true)
            }
            Err(e) if e.kind().is_fatal() => Err(e),
            Err(_e) => {
                warn!("can tx: {:?}, abandoning message", _e.kind());
                Ok(false)
            }
        }
    }

    /// [`Self::send_frame`] for a frame of the Bulk queue, which gives way
    /// to Control: `Ok(false)` as well once an entry of `control` has waited
    /// for the patience set, with this frame still not accepted.
    async fn send_frame_behind<Q>(
        &mut self,
        frame: &CanFrame,
        control: &FramedConsumer<Q>,
    ) -> Result<bool, Tx::Error>
    where
        Q: BbqHandle,
        Q::Notifier: AsyncNotifier,
    {
        let Some(patience_ms) = self.patience_ms else {
            return self.send_frame(frame).await;
        };
        let urgent = async {
            // Dropping the grant leaves the entry in its queue.
            drop(control.wait_read().await);
            sleep(Duration::from_millis(patience_ms)).await;
        };
        match select(self.send_frame(frame), urgent).await {
            Either::First(res) => res,
            Either::Second(()) => {
                debug!("can tx: abandoning a message for Control");
                Ok(false)
            }
        }
    }
}
