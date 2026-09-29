//! Generic packet (frame-based) RX/TX worker.
//!
//! Eliminates boilerplate for transports where each receive/send
//! operation yields a complete ergot frame (BLE L2CAP, UDP datagrams,
//! CAN FD, ESP-NOW, SPI, etc.).
//!
//! Transport authors implement [`PacketReceiver`] and [`PacketSender`],
//! then use [`PacketRxTxWorker`] to get the full RX/TX loop with
//! optional liveness timeout and state change notifications.
//!
//! # Example
//!
//! ```rust,ignore
//! use ergot::interface_manager::transports::packet::*;
//!
//! struct MyReceiver { /* ... */ }
//! impl PacketReceiver for MyReceiver {
//!     type Error = MyError;
//!     async fn recv(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
//!         // receive one complete frame
//!     }
//! }
//!
//! struct MySender { /* ... */ }
//! impl PacketSender for MySender {
//!     type Error = MyError;
//!     async fn send(&mut self, data: &[u8]) -> Result<(), Self::Error> {
//!         // send one complete frame
//!     }
//! }
//!
//! let mut worker = PacketRxTxWorker::new(nsh, rx, tx, processor, ident, consumer)
//!     .with_liveness(LivenessConfig { timeout_ms: 5000 })
//!     .with_state_notify(&STATE_NOTIFY);
//! worker.run(InterfaceState::Inactive, &mut scratch_buf).await?;
//! ```

use core::future::pending;

#[cfg(time_sleep)]
use crate::interface_manager::LivenessConfig;
use crate::interface_manager::transports::link::Link;
use crate::interface_manager::{FrameProcessor, InterfaceState, Profile};
use crate::logging::{trace, warn};
use crate::net_stack::NetStackHandle;
#[cfg(time_sleep)]
use crate::time::{Duration, Instant, sleep_until};
use bbqueue::prod_cons::framed::FramedConsumer;
use bbqueue::traits::bbqhdl::BbqHandle;
use bbqueue::traits::notifier::AsyncNotifier;
use embassy_futures::select::{Either3, select3};
use maitake_sync::WaitQueue;

/// Receive one complete frame from the transport.
///
/// Implementations fill `buf` with a single frame and return the number of
/// bytes written. The slice `&buf[..n]` is passed directly to
/// [`FrameProcessor::process_frame`].
pub trait PacketReceiver {
    type Error: core::fmt::Debug;

    /// Receive a single packet into `buf`. Returns the number of bytes received.
    ///
    /// The returned future MUST be cancel-safe: the RX worker races it against
    /// other futures (a TX-ready signal, a liveness timer) in a `select`, so it can
    /// be dropped before completing. Dropping it must not lose or partially consume
    /// a frame — an implementation that awaits more than once per frame (e.g. reads
    /// a length prefix and then the body) would desync if cancelled between the two.
    fn recv(
        &mut self,
        buf: &mut [u8],
    ) -> impl core::future::Future<Output = Result<usize, Self::Error>>;
}

/// Send one complete frame over the transport.
///
/// `data` is a serialized ergot frame read from the outgoing bbqueue.
pub trait PacketSender {
    type Error: core::fmt::Debug;

    /// Send a single packet.
    fn send(&mut self, data: &[u8]) -> impl core::future::Future<Output = Result<(), Self::Error>>;
}

/// Error returned by [`PacketRxTxWorker::run`].
#[derive(Debug)]
pub enum PacketWorkerError<RxE: core::fmt::Debug, TxE: core::fmt::Debug> {
    /// The receiver returned an error.
    Rx(RxE),
    /// The sender returned an error.
    Tx(TxE),
}

/// Generic combined RX/TX worker for packet-based transports.
///
/// Multiplexes between receiving frames from the transport and sending
/// serialized frames from the bbqueue. Optionally tracks liveness and
/// notifies observers on interface state changes.
pub struct PacketRxTxWorker<N, Rx, Tx, Q, P>
where
    N: NetStackHandle,
    Rx: PacketReceiver,
    Tx: PacketSender,
    Q: BbqHandle,
    Q::Notifier: AsyncNotifier,
    P: FrameProcessor<N>,
{
    link: Link<N>,
    receiver: Rx,
    sender: Tx,
    processor: P,
    consumer: FramedConsumer<Q>,
    #[cfg(time_sleep)]
    liveness: Option<LivenessConfig>,
    /// When the last frame arrived, once one has (with liveness enabled).
    #[cfg(time_sleep)]
    last_rx: Option<Instant>,
}

impl<N, Rx, Tx, Q, P> PacketRxTxWorker<N, Rx, Tx, Q, P>
where
    N: NetStackHandle,
    Rx: PacketReceiver,
    Tx: PacketSender,
    Q: BbqHandle,
    Q::Notifier: AsyncNotifier,
    P: FrameProcessor<N>,
{
    /// Create a new packet worker.
    pub fn new(
        nsh: N,
        receiver: Rx,
        sender: Tx,
        processor: P,
        ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
        consumer: FramedConsumer<Q>,
    ) -> Self {
        Self {
            link: Link::new(nsh, ident),
            receiver,
            sender,
            processor,
            consumer,
            #[cfg(time_sleep)]
            liveness: None,
            #[cfg(time_sleep)]
            last_rx: None,
        }
    }

    /// Enable liveness tracking (needs a [time backend](crate::time)).
    ///
    /// When enabled, the worker transitions the interface to
    /// [`InterfaceState::Inactive`] if no frames are received within
    /// `config.timeout_ms`. The timer only starts after the first frame, and
    /// counts from the last received one: transmitting does not postpone it
    /// (a send in progress when it expires delays the transition until the
    /// send completes). Recovery is automatic — when frames resume, the
    /// processor transitions back to `Active`.
    #[cfg(time_sleep)]
    pub fn with_liveness(mut self, config: LivenessConfig) -> Self {
        self.liveness = Some(config);
        self
    }

    /// Set a [`WaitQueue`] woken whenever this worker changes its
    /// interface's state (a frame activates it, a liveness timeout or the
    /// worker stopping takes it down). Changes made elsewhere, such as the
    /// bus address claim, do not reach it; wait with
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

    /// Run the combined RX/TX loop.
    ///
    /// Sets `initial_state` on the interface before entering the loop.
    /// On exit (transport error or drop), the interface transitions to
    /// [`InterfaceState::Down`].
    pub async fn run(
        &mut self,
        initial_state: InterfaceState,
        scratch: &mut [u8],
    ) -> Result<(), PacketWorkerError<Rx::Error, Tx::Error>> {
        self.link.set_state(initial_state);
        let res = self.run_inner(scratch).await;
        self.link.set_down();
        res
    }

    async fn run_inner(
        &mut self,
        scratch: &mut [u8],
    ) -> Result<(), PacketWorkerError<Rx::Error, Tx::Error>> {
        loop {
            // The liveness deadline, armed once a frame has arrived. It runs
            // from that frame, so sends in between do not push it back.
            let timeout = async {
                #[cfg(time_sleep)]
                if let (Some(config), Some(last_rx)) = (&self.liveness, self.last_rx) {
                    return sleep_until(last_rx + Duration::from_millis(config.timeout_ms)).await;
                }
                pending().await
            };
            match select3(
                self.receiver.recv(scratch),
                self.consumer.wait_read(),
                timeout,
            )
            .await
            {
                Either3::First(recv_result) => {
                    let used = recv_result.map_err(PacketWorkerError::Rx)?;
                    trace!("packet rx: {} bytes", used);
                    let data = &scratch[..used];
                    let changed =
                        self.processor
                            .process_frame(data, &self.link.nsh, self.link.ident.clone());
                    #[cfg(time_sleep)]
                    if self.liveness.is_some() {
                        self.last_rx = Some(Instant::now());
                    }
                    if changed {
                        self.link.notify();
                    }
                }
                Either3::Second(grant) => {
                    trace!("packet tx: {} bytes", grant.len());
                    self.sender
                        .send(&grant)
                        .await
                        .map_err(PacketWorkerError::Tx)?;
                    grant.release();
                }
                Either3::Third(()) => {
                    warn!("Liveness timeout — interface inactive");
                    self.link.deactivate(false);
                    self.processor.reset();
                    #[cfg(time_sleep)]
                    {
                        self.last_rx = None;
                    }
                }
            }
        }
    }
}
