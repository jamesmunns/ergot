//! Tokio serial transport.
//!
//! Thin wrapper around the COBS stream transport that handles serial port
//! opening and buffer clearing.

use std::sync::Arc;

use maitake_sync::WaitQueue;
use tokio_serial_v5::{ClearBuffer, SerialPort, SerialPortBuilderExt};

use crate::interface_manager::{
    Interface, InterfaceState, LivenessConfig,
    profiles::direct_edge::{DirectEdge, EdgeFrameProcessor},
    profiles::router::Router,
    utils::{cobs_stream::Sink, std::StdQueue},
};
use crate::logging::warn;
use crate::net_stack::NetStackHandle;
use rand_core::RngCore;

use super::tokio_cobs_stream;

/// Registration error for DirectEdge.
#[derive(Debug, PartialEq)]
pub enum EdgeRegistrationError {
    AlreadyActive,
    Serial(String),
}

/// Registration error for DirectRouter.
#[derive(Debug, PartialEq)]
pub enum RouterRegistrationError {
    OutOfNetIds,
    Serial(String),
}

/// Register a serial port transport on a [`DirectEdge`] profile.
///
/// Opens the serial port, clears buffers, and delegates to
/// [`tokio_cobs_stream::register_edge`].
#[allow(clippy::too_many_arguments)]
pub async fn register_edge<N, I>(
    stack: N,
    path: &str,
    baud: u32,
    queue: StdQueue,
    processor: EdgeFrameProcessor,
    initial_state: InterfaceState,
    liveness: Option<LivenessConfig>,
    state_notify: Option<Arc<WaitQueue>>,
) -> Result<(), EdgeRegistrationError>
where
    I: Interface,
    N: NetStackHandle<Profile = DirectEdge<I>> + Send + 'static,
{
    let mut port = tokio_serial_v5::new(path, baud)
        .open_native_async()
        .map_err(|e| EdgeRegistrationError::Serial(format!("Open Error: {:?}", e)))?;
    if let Err(e) = port.clear(ClearBuffer::All) {
        warn!("Failed to clear serial buffers: {:?}", e);
    }
    let _ = std::io::Write::write_all(&mut port, &[0]);
    let (rx, tx) = tokio::io::split(port);

    tokio_cobs_stream::register_edge::<N, I, _, _>(
        stack,
        rx,
        tx,
        queue,
        processor,
        initial_state,
        liveness,
        state_notify,
    )
    .await
    .map_err(|_| EdgeRegistrationError::AlreadyActive)
}

/// Register a serial port transport on a [`Router`] profile.
///
/// Opens the serial port, clears buffers, and delegates to
/// [`tokio_cobs_stream::register_router`].
pub async fn register_router<N, I, Rng, const M: usize, const SS: usize>(
    stack: N,
    path: &str,
    baud: u32,
    max_ergot_packet_size: u16,
    outgoing_buffer_size: usize,
    liveness: Option<LivenessConfig>,
    state_notify: Option<Arc<WaitQueue>>,
) -> Result<u8, RouterRegistrationError>
where
    I: Interface<Sink = Sink<StdQueue>>,
    Rng: RngCore + Send + 'static,
    N: NetStackHandle<Profile = Router<I, Rng, M, SS>> + Send + 'static,
{
    let mut port = tokio_serial_v5::new(path, baud)
        .open_native_async()
        .map_err(|e| RouterRegistrationError::Serial(format!("Open Error: {:?}", e)))?;
    if let Err(e) = port.clear(ClearBuffer::All) {
        warn!("Failed to clear serial buffers: {:?}", e);
    }
    let _ = std::io::Write::write_all(&mut port, &[0]);
    let (rx, tx) = tokio::io::split(port);

    tokio_cobs_stream::register_router::<N, I, Rng, _, _, M, SS>(
        stack,
        rx,
        tx,
        max_ergot_packet_size,
        outgoing_buffer_size,
        liveness,
        state_notify,
    )
    .await
    .map_err(|_| RouterRegistrationError::OutOfNetIds)
}
