//! Generic tokio COBS stream transport.
//!
//! Provides generic RxWorker, TxWorker, and profile-specific registration
//! functions for any `AsyncRead`/`AsyncWrite` COBS-framed transport
//! (TCP, serial, generic streams).
//!
//! [`FrameProcessor`]: crate::interface_manager::FrameProcessor

use std::sync::Arc;

use crate::{
    interface_manager::{
        FrameProcessor, Interface, InterfaceState, LivenessConfig, Profile,
        utils::std::{
            ReceiverError, StdQueue,
            acc::{CobsAccumulator, FeedResult},
        },
    },
    logging::{error, info, trace, warn},
    net_stack::NetStackHandle,
};
use bbqueue::{prod_cons::stream::StreamConsumer, traits::bbqhdl::BbqHandle};
use maitake_sync::WaitQueue;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    select,
};

// ---------------------------------------------------------------------------
// RxWorker
// ---------------------------------------------------------------------------

/// A generic COBS stream RxWorker for tokio-based transports.
pub struct CobsStreamRxWorker<N, R, P>
where
    N: NetStackHandle,
    R: AsyncReadExt + Unpin,
    P: FrameProcessor<N>,
{
    pub nsh: N,
    pub reader: R,
    pub closer: Arc<WaitQueue>,
    pub processor: P,
    pub ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
    pub liveness: Option<LivenessConfig>,
    pub state_notify: Option<Arc<WaitQueue>>,
    pub cobs_buf_size: usize,
}

impl<N, R, P> CobsStreamRxWorker<N, R, P>
where
    N: NetStackHandle,
    R: AsyncReadExt + Unpin,
    P: FrameProcessor<N>,
{
    fn notify(&self) {
        if let Some(notify) = &self.state_notify {
            notify.wake_all();
        }
    }

    pub async fn run(&mut self) -> ReceiverError {
        let mut cobs_buf = CobsAccumulator::new(self.cobs_buf_size);
        let mut raw_buf = vec![0u8; 4096].into_boxed_slice();
        let mut have_received = false;

        loop {
            let ct = match self
                .read_or_timeout(&mut raw_buf, &mut have_received, &mut cobs_buf)
                .await
            {
                Ok(ct) => ct,
                Err(e) => return e,
            };

            let buf = &mut raw_buf[..ct];
            let mut window = buf;

            'cobs: while !window.is_empty() {
                window = match cobs_buf.feed_raw(window) {
                    FeedResult::Consumed => break 'cobs,
                    FeedResult::OverFull(new_wind) => new_wind,
                    FeedResult::DecodeError(new_wind) => new_wind,
                    FeedResult::Success { data, remaining }
                    | FeedResult::SuccessInput { data, remaining } => {
                        let changed =
                            self.processor
                                .process_frame(data, &self.nsh, self.ident.clone());
                        have_received = true;
                        if changed {
                            self.notify();
                        }
                        remaining
                    }
                };
            }
        }
    }

    async fn read_or_timeout(
        &mut self,
        buf: &mut [u8],
        have_received: &mut bool,
        cobs_buf: &mut CobsAccumulator,
    ) -> Result<usize, ReceiverError> {
        loop {
            let rd = self.reader.read(buf);
            let close = self.closer.wait();

            let liveness_active = self.liveness.is_some() && *have_received;

            let ct = if liveness_active {
                let liveness = self.liveness.as_ref().unwrap();
                let timeout =
                    tokio::time::sleep(tokio::time::Duration::from_millis(liveness.timeout_ms));

                select! {
                    r = rd => {
                        match r {
                            Ok(0) | Err(_) => {
                                warn!("recv run closed");
                                return Err(ReceiverError::SocketClosed);
                            }
                            Ok(ct) => ct,
                        }
                    }
                    _c = close => {
                        return Err(ReceiverError::SocketClosed);
                    }
                    _ = timeout => {
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
                            warn!("Liveness timeout — interface inactive");
                            self.notify();
                        }
                        self.processor.reset();
                        *have_received = false;
                        *cobs_buf = CobsAccumulator::new(self.cobs_buf_size);
                        continue;
                    }
                }
            } else {
                select! {
                    r = rd => {
                        match r {
                            Ok(0) | Err(_) => {
                                warn!("recv run closed");
                                return Err(ReceiverError::SocketClosed);
                            }
                            Ok(ct) => ct,
                        }
                    }
                    _c = close => {
                        return Err(ReceiverError::SocketClosed);
                    }
                }
            };

            return Ok(ct);
        }
    }
}

// ---------------------------------------------------------------------------
// TxWorker
// ---------------------------------------------------------------------------

/// A generic COBS stream TxWorker for tokio-based transports.
///
/// On exit, calls `closer.close()` to ensure the RxWorker also shuts down.
pub struct CobsStreamTxWorker<W: AsyncWriteExt + Unpin> {
    pub writer: W,
    pub consumer: StreamConsumer<StdQueue>,
    pub closer: Arc<WaitQueue>,
}

impl<W: AsyncWriteExt + Unpin> CobsStreamTxWorker<W> {
    pub async fn run(mut self) {
        info!("Started COBS stream tx_worker");
        loop {
            let rxf = self.consumer.wait_read();
            let clf = self.closer.wait();

            let frame = select! {
                r = rxf => r,
                _c = clf => {
                    break;
                }
            };

            let len = frame.len();
            trace!("sending pkt len:{}", len);
            let res = self.writer.write_all(&frame).await;
            frame.release(len);
            if let Err(e) = res {
                error!("Tx Error: {:?}", e);
                break;
            }
        }
        warn!("Closing COBS stream tx_worker");
        self.closer.close();
    }
}

// ---------------------------------------------------------------------------
// Registration: DirectEdge
// ---------------------------------------------------------------------------

use crate::interface_manager::profiles::direct_edge::{DirectEdge, EdgeFrameProcessor};

/// Registration error for DirectEdge.
#[derive(Debug, PartialEq)]
pub struct EdgeRegistrationError;

/// Register a COBS-framed stream transport on a [`DirectEdge`] profile.
///
/// `initial_state` controls target vs controller mode:
/// - Target: `InterfaceState::Inactive` with `EdgeFrameProcessor::new()`
/// - Controller: `InterfaceState::Active { net_id: 1, node_id: 1 }` with
///   `EdgeFrameProcessor::new_controller(1)`
#[allow(clippy::too_many_arguments)]
pub async fn register_edge<N, I, R, W>(
    stack: N,
    reader: R,
    writer: W,
    queue: StdQueue,
    processor: EdgeFrameProcessor,
    initial_state: InterfaceState,
    liveness: Option<LivenessConfig>,
    state_notify: Option<Arc<WaitQueue>>,
) -> Result<(), EdgeRegistrationError>
where
    I: Interface,
    N: NetStackHandle<Profile = DirectEdge<I>> + Send + 'static,
    R: AsyncReadExt + Unpin + Send + 'static,
    W: AsyncWriteExt + Unpin + Send + 'static,
{
    let closer = Arc::new(WaitQueue::new());
    stack.stack().manage_profile(|im| {
        match im.interface_state(()) {
            Some(InterfaceState::Down) | None => {}
            _ => return Err(EdgeRegistrationError),
        }
        im.set_closer(closer.clone());
        im.set_interface_state((), initial_state)
            .map_err(|_| EdgeRegistrationError)?;
        Ok(())
    })?;
    if let Some(notify) = &state_notify {
        notify.wake_all();
    }

    let notify_clone = state_notify.clone();
    let stack_clone = stack.clone();

    let mut rx_worker = CobsStreamRxWorker {
        nsh: stack,
        reader,
        closer: closer.clone(),
        processor,
        ident: (),
        liveness,
        state_notify,
        cobs_buf_size: 1024 * 1024,
    };

    tokio::task::spawn(async move {
        let close = rx_worker.closer.clone();
        select! {
            _run = rx_worker.run() => { close.close(); },
            _clf = close.wait() => {},
        }
        stack_clone.stack().manage_profile(|im| {
            _ = im.set_interface_state((), InterfaceState::Down);
        });
        if let Some(notify) = &notify_clone {
            notify.wake_all();
        }
    });
    tokio::task::spawn(
        CobsStreamTxWorker {
            writer,
            consumer: <StdQueue as BbqHandle>::stream_consumer(&queue),
            closer: closer.clone(),
        }
        .run(),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Registration: Router
// ---------------------------------------------------------------------------

use crate::interface_manager::profiles::router::{Router, RouterFrameProcessor};
use crate::interface_manager::utils::cobs_stream::Sink;
use crate::interface_manager::utils::std::new_std_queue;
use rand_core::RngCore;

/// Registration error for Router.
#[derive(Debug, PartialEq)]
pub struct RouterRegistrationError;

/// Register a COBS-framed stream transport on a [`Router`] profile.
pub async fn register_router<N, I, Rng, R, W, const M: usize, const SS: usize>(
    stack: N,
    reader: R,
    writer: W,
    max_ergot_packet_size: u16,
    outgoing_buffer_size: usize,
    liveness: Option<LivenessConfig>,
    state_notify: Option<Arc<WaitQueue>>,
) -> Result<u8, RouterRegistrationError>
where
    I: Interface<Sink = Sink<StdQueue>>,
    Rng: RngCore + Send + 'static,
    N: NetStackHandle<Profile = Router<I, Rng, M, SS>> + Send + 'static,
    R: AsyncReadExt + Unpin + Send + 'static,
    W: AsyncWriteExt + Unpin + Send + 'static,
{
    let q: StdQueue = new_std_queue(outgoing_buffer_size);
    let res = stack.stack().manage_profile(|im| {
        let ident = im
            .register_interface(Sink::new_from_handle(q.clone(), max_ergot_packet_size))
            .ok()?;
        let state = im.interface_state(ident)?;
        match state {
            InterfaceState::Active { net_id, node_id: _ } => Some((ident, net_id)),
            _ => {
                _ = im.deregister_interface(ident);
                None
            }
        }
    });
    let Some((ident, net_id)) = res else {
        return Err(RouterRegistrationError);
    };
    let closer = Arc::new(WaitQueue::new());

    let overhead = cobs::max_encoding_overhead(max_ergot_packet_size as usize);
    let cobs_buf_size = max_ergot_packet_size as usize + overhead;

    let notify_clone = state_notify.clone();
    let nsh_clone = stack.clone();

    let mut rx_worker = CobsStreamRxWorker {
        nsh: stack.clone(),
        reader,
        closer: closer.clone(),
        processor: RouterFrameProcessor::new(net_id),
        ident,
        liveness,
        state_notify,
        cobs_buf_size,
    };

    stack.stack().manage_profile(|im| {
        im.set_interface_closer(ident, closer.clone());
    });

    tokio::task::spawn(async move {
        let close = rx_worker.closer.clone();
        select! {
            _run = rx_worker.run() => { close.close(); },
            _clf = close.wait() => {},
        }
        nsh_clone.stack().manage_profile(|im| {
            _ = im.deregister_interface(ident);
        });
        if let Some(notify) = &notify_clone {
            notify.wake_all();
        }
    });
    tokio::task::spawn(
        CobsStreamTxWorker {
            writer,
            consumer: <StdQueue as BbqHandle>::stream_consumer(&q),
            closer: closer.clone(),
        }
        .run(),
    );

    Ok(ident)
}

// ---------------------------------------------------------------------------
// Registration: Bridge upstream
// ---------------------------------------------------------------------------

use crate::interface_manager::profiles::router::UPSTREAM_IDENT;

/// Registration error for bridge upstream.
#[derive(Debug, PartialEq)]
pub struct BridgeUpstreamRegistrationError;

/// Register a COBS-framed stream as the upstream interface of a bridge [`Router`].
///
/// Uses [`EdgeFrameProcessor`] to discover the upstream net_id from
/// incoming frames and [`UPSTREAM_IDENT`] as the interface identifier.
/// The upstream starts in [`InterfaceState::Inactive`].
///
/// [`Router`]: crate::interface_manager::profiles::router::Router
#[allow(clippy::too_many_arguments)]
pub async fn register_bridge_upstream<N, R, W>(
    stack: N,
    reader: R,
    writer: W,
    queue: StdQueue,
    liveness: Option<LivenessConfig>,
    state_notify: Option<Arc<WaitQueue>>,
) -> Result<(), BridgeUpstreamRegistrationError>
where
    N: NetStackHandle + Send + 'static,
    <N::Profile as Profile>::InterfaceIdent: From<u8> + Send,
    R: AsyncReadExt + Unpin + Send + 'static,
    W: AsyncWriteExt + Unpin + Send + 'static,
{
    let closer = Arc::new(WaitQueue::new());

    stack
        .stack()
        .manage_profile(|im| {
            im.set_interface_state(UPSTREAM_IDENT.into(), InterfaceState::Inactive)
        })
        .map_err(|_| BridgeUpstreamRegistrationError)?;
    if let Some(notify) = &state_notify {
        notify.wake_all();
    }

    let notify_clone = state_notify.clone();
    let stack_clone = stack.clone();

    let mut rx_worker = CobsStreamRxWorker {
        nsh: stack,
        reader,
        closer: closer.clone(),
        processor: EdgeFrameProcessor::new(),
        ident: UPSTREAM_IDENT.into(),
        liveness,
        state_notify,
        cobs_buf_size: 1024 * 1024,
    };

    tokio::task::spawn(async move {
        let close = rx_worker.closer.clone();
        select! {
            _run = rx_worker.run() => { close.close(); },
            _clf = close.wait() => {},
        }
        stack_clone.stack().manage_profile(|im| {
            _ = im.set_interface_state(UPSTREAM_IDENT.into(), InterfaceState::Down);
        });
        if let Some(notify) = &notify_clone {
            notify.wake_all();
        }
    });
    tokio::task::spawn(
        CobsStreamTxWorker {
            writer,
            consumer: <StdQueue as BbqHandle>::stream_consumer(&queue),
            closer: closer.clone(),
        }
        .run(),
    );
    Ok(())
}
