//! Generic tokio COBS stream transport.
//!
//! Thin wrapper around the runtime-agnostic [`futures_io`] transport:
//! the RX/TX loops, closer handling, and liveness timeout all live there.
//! This module adapts tokio I/O types via `tokio-util` compat, provides
//! `tokio::time::sleep` as the liveness sleeper, and offers
//! profile-specific registration functions that spawn tokio tasks.
//!
//! [`futures_io`]: super::futures_io

use std::sync::Arc;

use crate::{
    interface_manager::{
        Interface, InterfaceState, LivenessConfig, Profile,
        utils::std::StdQueue,
    },
    logging::{error, info, warn},
    net_stack::NetStackHandle,
};
use bbqueue::traits::bbqhdl::BbqHandle;
use maitake_sync::WaitQueue;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    select,
};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::futures_io::RxWorker;

/// The liveness sleeper for tokio-based transports.
fn tokio_sleeper(ms: u64) -> tokio::time::Sleep {
    tokio::time::sleep(tokio::time::Duration::from_millis(ms))
}

/// Run an [`RxWorker`] to completion, with or without a liveness timeout.
async fn run_rx_worker<N, R, P>(
    rx_worker: &mut RxWorker<N, R, P>,
    liveness: Option<LivenessConfig>,
    cobs_buf_size: usize,
) where
    N: NetStackHandle,
    R: futures_io::AsyncRead + Unpin,
    P: crate::interface_manager::FrameProcessor<N>,
{
    let mut frame = vec![0u8; cobs_buf_size].into_boxed_slice();
    let mut scratch = vec![0u8; 4096].into_boxed_slice();
    let res = match liveness {
        Some(cfg) => {
            rx_worker
                .run_with_liveness(&mut frame, &mut scratch, cfg, tokio_sleeper)
                .await
        }
        None => rx_worker.run(&mut frame, &mut scratch).await,
    };
    match res {
        Ok(end) => info!("rx_worker ended: {:?}", end),
        Err(e) => warn!("rx_worker ended with error: {:?}", e),
    }
}

// ---------------------------------------------------------------------------
// TxWorker
// ---------------------------------------------------------------------------

/// A generic COBS stream TxWorker for tokio-based transports.
///
/// Wraps the [`futures_io::tx_worker`](super::futures_io::tx_worker) with
/// closer support. On exit, calls `closer.close()` to ensure the RxWorker
/// also shuts down.
pub struct CobsStreamTxWorker<W: AsyncWriteExt + Unpin> {
    pub writer: W,
    pub consumer: bbqueue::prod_cons::stream::StreamConsumer<StdQueue>,
    pub closer: Arc<WaitQueue>,
}

impl<W: AsyncWriteExt + Unpin> CobsStreamTxWorker<W> {
    pub async fn run(self) {
        info!("Started COBS stream tx_worker");
        let mut compat_writer = self.writer.compat_write();

        select! {
            res = super::futures_io::tx_worker(&mut compat_writer, self.consumer) => {
                if let Err(e) = res {
                    error!("Tx Error: {:?}", e);
                }
            }
            _c = self.closer.wait() => {}
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
/// - Target: `InterfaceState::Active { net_id: 0, node_id: EDGE_NODE_ID }` with `EdgeFrameProcessor::new()`
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

    let mut rx_worker =
        RxWorker::new(stack, reader.compat(), processor, ()).with_closer(closer.clone());
    if let Some(notify) = state_notify {
        rx_worker = rx_worker.with_state_notify(notify);
    }

    let rx_closer = closer.clone();
    tokio::task::spawn(async move {
        run_rx_worker(&mut rx_worker, liveness, 1024 * 1024).await;
        // Ensure the TX worker also shuts down. The RxWorker itself sets
        // the interface Down and wakes the state notifier on exit.
        rx_closer.close();
    });
    tokio::task::spawn(
        CobsStreamTxWorker {
            writer,
            consumer: <StdQueue as BbqHandle>::stream_consumer(&queue),
            closer,
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
pub async fn register_router<N, I, Rng, R, W, const M: usize, const SS: usize, const CC: usize>(
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
    N: NetStackHandle<Profile = Router<I, Rng, M, SS, CC>> + Send + 'static,
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

    let nsh_clone = stack.clone();

    let mut rx_worker = RxWorker::new(
        stack.clone(),
        reader.compat(),
        RouterFrameProcessor::new(net_id),
        ident,
    )
    .with_closer(closer.clone());
    if let Some(notify) = state_notify.clone() {
        rx_worker = rx_worker.with_state_notify(notify);
    }

    stack.stack().manage_profile(|im| {
        im.set_interface_closer(ident, closer.clone());
    });

    let rx_closer = closer.clone();
    tokio::task::spawn(async move {
        run_rx_worker(&mut rx_worker, liveness, cobs_buf_size).await;
        rx_closer.close();
        nsh_clone.stack().manage_profile(|im| {
            _ = im.deregister_interface(ident);
        });
        if let Some(notify) = &state_notify {
            notify.wake_all();
        }
    });
    tokio::task::spawn(
        CobsStreamTxWorker {
            writer,
            consumer: <StdQueue as BbqHandle>::stream_consumer(&q),
            closer,
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
/// The upstream starts in [`InterfaceState::Active`] with link-local
/// addressing (`net_id = 0`), allowing the bridge to initiate contact
/// before receiving any frame from the root router.
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
            im.set_interface_state(
                UPSTREAM_IDENT.into(),
                InterfaceState::Active {
                    net_id: 0,
                    node_id: crate::interface_manager::edge_port::EDGE_NODE_ID,
                },
            )
        })
        .map_err(|_| BridgeUpstreamRegistrationError)?;
    if let Some(notify) = &state_notify {
        notify.wake_all();
    }

    let mut rx_worker = RxWorker::new(
        stack,
        reader.compat(),
        EdgeFrameProcessor::new(),
        UPSTREAM_IDENT.into(),
    )
    .with_closer(closer.clone());
    if let Some(notify) = state_notify {
        rx_worker = rx_worker.with_state_notify(notify);
    }

    let rx_closer = closer.clone();
    tokio::task::spawn(async move {
        run_rx_worker(&mut rx_worker, liveness, 1024 * 1024).await;
        rx_closer.close();
    });
    tokio::task::spawn(
        CobsStreamTxWorker {
            writer,
            consumer: <StdQueue as BbqHandle>::stream_consumer(&queue),
            closer,
        }
        .run(),
    );
    Ok(())
}
