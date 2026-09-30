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
use core::future::Future;

use bbqueue::prod_cons::framed::FramedConsumer;
use bbqueue::traits::bbqhdl::BbqHandle;
use bbqueue::traits::notifier::AsyncNotifier;
use embassy_futures::select::{Either, select};
use maitake_sync::WaitQueue;
use portable_atomic::{AtomicU8, Ordering};

use crate::interface_manager::transports::link::Link;
use crate::interface_manager::utils::can::{
    BROADCAST_NODE_ID, CanFrame, CanId, DropReason, Fragmenter, Push, QueuedFrame, Reassembler,
    ReassemblyTimeouts, is_max_payload,
};
use crate::interface_manager::{FrameProcessor, InterfaceState, Profile};
use crate::logging::{debug, trace, warn};
use crate::net_stack::NetStackHandle;
use crate::time::{Duration, Instant, TimedOut, sleep, with_timeout};

#[allow(unused_imports)]
use crate::interface_manager::utils;

/// What kind of failure a CAN adapter reports, which decides what the worker
/// does about it. Adapters map their controller's errors onto these.
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CanErrorKind {
    /// Received frames were lost (RX FIFO overrun). The worker carries on;
    /// reassembly drops the messages the lost frames belonged to.
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
    /// The worker drops the future when the TX timeout expires and abandons
    /// the rest of that message; the frame may or may not have gone out.
    fn send(&mut self, frame: &CanFrame) -> impl Future<Output = Result<(), Self::Error>>;
}

/// The receive half of a CAN controller.
pub trait CanRx {
    type Error: CanError;

    /// Receive one frame.
    fn recv(&mut self) -> impl Future<Output = Result<CanFrame, Self::Error>>;
}

/// How long the worker waits for the controller to accept one frame, by
/// default, before it abandons the message.
pub const DEFAULT_TX_TIMEOUT_MS: u64 = 100;

/// How long the receive loop pauses after a non-fatal adapter error, so an
/// adapter that keeps reporting one (e.g. while the controller is
/// error-passive) cannot starve the transmit loop and the rest of the task.
pub const RX_ERROR_BACKOFF_MS: u64 = 1;

/// Worker configuration.
#[derive(Debug, Clone, Copy)]
pub struct CanConfig {
    /// How long a partial message may sit without progress, per class, and
    /// the window for recognising CAN's retransmitted duplicates.
    pub reassembly: ReassemblyTimeouts,
    /// Where the per-class message counters (`tid`) start. Take it from an
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
}

impl CanConfig {
    /// Default timeouts with the given counter seed.
    pub const fn new(tid_seed: u8) -> Self {
        Self {
            reassembly: ReassemblyTimeouts::DEFAULT,
            tid_seed,
            tx_timeout_ms: Some(DEFAULT_TX_TIMEOUT_MS),
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
/// interface is up (its task was cancelled) takes the interface Down.
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
    /// The last `tid` issued, per traffic class.
    tids: [u8; 4],
    tx_timeout_ms: Option<u64>,
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
            tids: [config.tid_seed; 4],
            tx_timeout_ms: config.tx_timeout_ms,
        }
    }

    /// Set a [`WaitQueue`] to be notified on interface state transitions.
    pub fn with_state_notify(mut self, notify: &'static WaitQueue) -> Self {
        self.link.set_state_notify(notify);
        self
    }

    /// Run until an adapter reports a fatal error. Sets `initial_state` first
    /// and leaves the interface `Down` on exit.
    pub async fn run(
        &mut self,
        initial_state: InterfaceState,
    ) -> Result<(), CanWorkerError<Rx::Error, Tx::Error>> {
        self.link.set_state(initial_state);
        let res = self.run_inner().await;
        self.link.set_down();
        res
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
        } = self;
        // This device's node on the segment, 0 while unknown (node 0 is never
        // a segment address). The TX loop learns it from every frame it sends
        // (`LinkMeta::src_node`), so the RX filter follows an address claim
        // without taking the profile lock per frame.
        let own_node = AtomicU8::new(0);
        let rx_side = rx_loop(link, rx, processor, reassembler, &own_node);
        let tx_side = tx_loop(tx, control, bulk, tids, *tx_timeout_ms, &own_node);
        match select(rx_side, tx_side).await {
            Either::First(Err(e)) => Err(CanWorkerError::Rx(e)),
            Either::Second(Err(e)) => Err(CanWorkerError::Tx(e)),
            Either::First(Ok(never)) | Either::Second(Ok(never)) => match never {},
        }
    }
}

async fn rx_loop<N, Rx, P, const K: usize, const MTU: usize>(
    link: &Link<N>,
    rx: &mut Rx,
    processor: &mut P,
    reassembler: &mut Reassembler<K, MTU>,
    own_node: &AtomicU8,
) -> Result<Infallible, Rx::Error>
where
    N: NetStackHandle,
    Rx: CanRx,
    P: FrameProcessor<N>,
{
    loop {
        let frame = match rx.recv().await {
            Ok(frame) => frame,
            Err(e) if e.kind().is_fatal() => return Err(e),
            Err(_e) => {
                warn!("can rx: {:?}, continuing", _e.kind());
                // An adapter may report the same condition again right away
                // (e.g. an error-passive controller): don't spin.
                sleep(Duration::from_millis(RX_ERROR_BACKOFF_MS)).await;
                continue;
            }
        };
        let now = Instant::now();
        reassembler.expire(now);
        if handle_rx(&frame, link, processor, reassembler, now, own_node) {
            link.notify();
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
    own_node: &AtomicU8,
) -> bool
where
    N: NetStackHandle,
    P: FrameProcessor<N>,
{
    let id = frame.id;
    // Software filter: mine or everyone's. Adapters should also program the
    // hardware filters this way.
    //
    // Only a message's first frame is filtered: a later frame only extends a
    // message whose first frame passed here, since one (sender, class)
    // assembles one message, for one destination, at a time.
    if id.idx() == 0
        && id.dst_node() != BROADCAST_NODE_ID
        && id.dst_node() != own_node.load(Ordering::Relaxed)
    {
        // Not the node we last knew (or nothing sent yet): ask the profile.
        // Our address may have changed without a transmission since, e.g. a
        // bus claim that was denied and restored the previous node. Until the
        // node is known at all, accept everything. Without hardware filters
        // this takes the profile lock for every other node's first frame.
        let me = link
            .nsh
            .stack()
            .manage_profile(|im| im.interface_node_id(link.ident.clone()))
            .unwrap_or(0);
        own_node.store(me, Ordering::Relaxed);
        if me != 0 && id.dst_node() != me {
            return false;
        }
    }
    match reassembler.push(frame, now) {
        Push::Single(data) => processor.process_frame(data, &link.nsh, link.ident.clone()),
        Push::Complete(slot) => {
            let data = reassembler.finish(slot);
            processor.process_frame(data, &link.nsh, link.ident.clone())
        }
        Push::Pending => false,
        // Routine without hardware filters: another node's continuation.
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
    tx: &mut Tx,
    control: &FramedConsumer<Q>,
    bulk: &FramedConsumer<Q>,
    tids: &mut [u8; 4],
    timeout_ms: Option<u64>,
    own_node: &AtomicU8,
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
                let res = send_message(tx, &entry, tids, timeout_ms, own_node).await;
                entry.release();
                res?;
                continue;
            }
            Either::Second(entry) => entry,
        };
        // A message from the Bulk queue. Control messages cut in between its
        // frames: another class, so the receiver assembles them apart. Within
        // a queue, messages never interleave — the receiver relies on it.
        if let Some(fragments) = begin_message(&entry, tx.max_payload(), tids, own_node) {
            for fragment in fragments {
                while let Ok(urgent) = control.read() {
                    let res = send_message(tx, &urgent, tids, timeout_ms, own_node).await;
                    urgent.release();
                    res?;
                }
                if !send_frame(tx, &fragment, timeout_ms).await? {
                    break;
                }
            }
        }
        entry.release();
    }
}

/// Parse one queue entry, give it its class's next `tid` and fragment it.
/// `None` if the entry cannot be sent at all (logged).
fn begin_message<'a>(
    entry: &'a [u8],
    max_payload: u8,
    tids: &mut [u8; 4],
    own_node: &AtomicU8,
) -> Option<Fragmenter<'a>> {
    let Some(q) = QueuedFrame::parse(entry) else {
        warn!("can tx: malformed queue entry, dropping");
        return None;
    };
    own_node.store(q.src_node, Ordering::Relaxed);
    let tid = &mut tids[usize::from(q.prio & 0b11)];
    *tid = tid.wrapping_add(1) & CanId::MAX_TID;
    Fragmenter::new(
        q.frame,
        max_payload as usize,
        q.prio,
        q.dst_node,
        q.src_node,
        *tid,
    )
    .inspect_err(|_e| {
        warn!("can tx: cannot fragment a queued frame: {:?}", _e);
    })
    .ok()
}

/// Send every frame of one queue entry. A frame that is not accepted abandons
/// the rest; the receiver drops the partial message when the sender's next
/// message of that class starts, or when it times out.
async fn send_message<Tx: CanTx>(
    tx: &mut Tx,
    entry: &[u8],
    tids: &mut [u8; 4],
    timeout_ms: Option<u64>,
    own_node: &AtomicU8,
) -> Result<(), Tx::Error> {
    let Some(fragments) = begin_message(entry, tx.max_payload(), tids, own_node) else {
        return Ok(());
    };
    for fragment in fragments {
        if !send_frame(tx, &fragment, timeout_ms).await? {
            break;
        }
    }
    Ok(())
}

/// Hand one frame to the controller. `Ok(false)` if it was not accepted (TX
/// timeout, or a non-fatal adapter error): the caller abandons the message.
async fn send_frame<Tx: CanTx>(
    tx: &mut Tx,
    frame: &CanFrame,
    timeout_ms: Option<u64>,
) -> Result<bool, Tx::Error> {
    trace!("can tx: id={:#x} len={}", frame.id.raw(), frame.len);
    let res = match timeout_ms {
        Some(ms) => match with_timeout(Duration::from_millis(ms), tx.send(frame)).await {
            Ok(res) => res,
            Err(TimedOut) => {
                warn!("can tx: not accepted within {} ms, abandoning message", ms);
                return Ok(false);
            }
        },
        None => tx.send(frame).await,
    };
    match res {
        Ok(()) => Ok(true),
        Err(e) if e.kind().is_fatal() => Err(e),
        Err(_e) => {
            warn!("can tx: {:?}, abandoning message", _e.kind());
            Ok(false)
        }
    }
}
