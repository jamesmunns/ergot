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

use crate::interface_manager::{FrameProcessor, InterfaceState, Profile};
use crate::logging::trace;
#[cfg(feature = "embassy-time")]
use crate::logging::warn;
use crate::net_stack::NetStackHandle;
use bbqueue::prod_cons::framed::FramedConsumer;
use bbqueue::traits::bbqhdl::BbqHandle;
use bbqueue::traits::notifier::AsyncNotifier;
use embassy_futures::select::{Either, select};

#[cfg(feature = "embassy-time")]
use crate::interface_manager::LivenessConfig;
#[cfg(feature = "embassy-time")]
use maitake_sync::WaitQueue;

/// Receive one complete frame from the transport.
///
/// Implementations fill `buf` with a single frame and return the number of
/// bytes written. The slice `&buf[..n]` is passed directly to
/// [`FrameProcessor::process_frame`].
pub trait PacketReceiver {
    type Error: core::fmt::Debug;

    /// Receive a single packet into `buf`. Returns the number of bytes received.
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
    nsh: N,
    receiver: Rx,
    sender: Tx,
    processor: P,
    ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
    consumer: FramedConsumer<Q>,
    #[cfg(feature = "embassy-time")]
    liveness: Option<LivenessConfig>,
    #[cfg(feature = "embassy-time")]
    state_notify: Option<&'static WaitQueue>,
    #[cfg(feature = "embassy-time")]
    have_received: bool,
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
            nsh,
            receiver,
            sender,
            processor,
            ident,
            consumer,
            #[cfg(feature = "embassy-time")]
            liveness: None,
            #[cfg(feature = "embassy-time")]
            state_notify: None,
            #[cfg(feature = "embassy-time")]
            have_received: false,
        }
    }

    /// Enable liveness tracking.
    ///
    /// When enabled, the worker transitions the interface to
    /// [`InterfaceState::Inactive`] if no frames are received within
    /// `config.timeout_ms`. The timer only starts after the first frame.
    /// Recovery is automatic — when frames resume, the processor
    /// transitions back to `Active`.
    #[cfg(feature = "embassy-time")]
    pub fn with_liveness(mut self, config: LivenessConfig) -> Self {
        self.liveness = Some(config);
        self
    }

    /// Set a [`WaitQueue`] to be notified on interface state transitions.
    #[cfg(feature = "embassy-time")]
    pub fn with_state_notify(mut self, notify: &'static WaitQueue) -> Self {
        self.state_notify = Some(notify);
        self
    }

    #[cfg(feature = "embassy-time")]
    fn notify(&self) {
        if let Some(notify) = self.state_notify {
            notify.wake_all();
        }
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
        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state(self.ident.clone(), initial_state))
            .inspect_err(|_e| {
                crate::logging::error!("Error setting interface state: {:?}", _e);
            });
        #[cfg(feature = "embassy-time")]
        self.notify();

        let res = self.run_inner(scratch).await;

        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state(self.ident.clone(), InterfaceState::Down));
        #[cfg(feature = "embassy-time")]
        self.notify();

        res
    }

    #[cfg(not(feature = "embassy-time"))]
    async fn run_inner(
        &mut self,
        scratch: &mut [u8],
    ) -> Result<(), PacketWorkerError<Rx::Error, Tx::Error>> {
        loop {
            let rx_fut = self.receiver.recv(scratch);
            let tx_fut = self.consumer.wait_read();

            match select(rx_fut, tx_fut).await {
                Either::First(recv_result) => {
                    let used = recv_result.map_err(PacketWorkerError::Rx)?;
                    trace!("packet rx: {} bytes", used);
                    let data = &scratch[..used];
                    self.processor
                        .process_frame(data, &self.nsh, self.ident.clone());
                }
                Either::Second(grant) => {
                    trace!("packet tx: {} bytes", grant.len());
                    self.sender
                        .send(&grant)
                        .await
                        .map_err(PacketWorkerError::Tx)?;
                    grant.release();
                }
            }
        }
    }

    #[cfg(feature = "embassy-time")]
    async fn run_inner(
        &mut self,
        scratch: &mut [u8],
    ) -> Result<(), PacketWorkerError<Rx::Error, Tx::Error>> {
        use embassy_futures::select::{Either3, select3};

        loop {
            let rx_fut = self.receiver.recv(scratch);
            let tx_fut = self.consumer.wait_read();

            let liveness_active = self.liveness.is_some() && self.have_received;

            if liveness_active {
                let timeout_ms = self.liveness.as_ref().unwrap().timeout_ms;
                let timer = embassy_time::Timer::after_millis(timeout_ms);

                match select3(rx_fut, tx_fut, timer).await {
                    Either3::First(recv_result) => {
                        let used = recv_result.map_err(PacketWorkerError::Rx)?;
                        trace!("packet rx: {} bytes", used);
                        let data = &scratch[..used];
                        let changed =
                            self.processor
                                .process_frame(data, &self.nsh, self.ident.clone());
                        self.have_received = true;
                        if changed {
                            self.notify();
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
                        let changed = self.nsh.stack().manage_profile(|im| {
                            if matches!(
                                im.interface_state(self.ident.clone()),
                                Some(InterfaceState::Active { .. })
                            ) {
                                _ = im.set_interface_state(
                                    self.ident.clone(),
                                    InterfaceState::Inactive,
                                );
                                true
                            } else {
                                false
                            }
                        });
                        if changed {
                            self.notify();
                        }
                        self.processor.reset();
                        self.have_received = false;
                    }
                }
            } else {
                match select(rx_fut, tx_fut).await {
                    Either::First(recv_result) => {
                        let used = recv_result.map_err(PacketWorkerError::Rx)?;
                        trace!("packet rx: {} bytes", used);
                        let data = &scratch[..used];
                        let changed =
                            self.processor
                                .process_frame(data, &self.nsh, self.ident.clone());
                        self.have_received = true;
                        if changed {
                            self.notify();
                        }
                    }
                    Either::Second(grant) => {
                        trace!("packet tx: {} bytes", grant.len());
                        self.sender
                            .send(&grant)
                            .await
                            .map_err(PacketWorkerError::Tx)?;
                        grant.release();
                    }
                }
            }
        }
    }
}

impl<N, Rx, Tx, Q, P> Drop for PacketRxTxWorker<N, Rx, Tx, Q, P>
where
    N: NetStackHandle,
    Rx: PacketReceiver,
    Tx: PacketSender,
    Q: BbqHandle,
    Q::Notifier: AsyncNotifier,
    P: FrameProcessor<N>,
{
    fn drop(&mut self) {
        let needs_down = self.nsh.stack().manage_profile(|im| {
            !matches!(
                im.interface_state(self.ident.clone()),
                Some(InterfaceState::Down) | None
            )
        });
        if needs_down {
            self.nsh.stack().manage_profile(|im| {
                _ = im.set_interface_state(self.ident.clone(), InterfaceState::Down);
            });
            #[cfg(feature = "embassy-time")]
            self.notify();
        }
    }
}
