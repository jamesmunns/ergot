//! Generic nusb USB bulk RxWorker.
//!
//! Works with any [`FrameProcessor`]. USB bulk transfers are
//! treated as complete frames (no COBS encoding). Includes
//! stall recovery logic for USB error conditions.
//!
//! [`FrameProcessor`]: crate::interface_manager::FrameProcessor

use std::sync::Arc;

use crate::{
    interface_manager::{
        FrameProcessor, Profile,
        utils::std::{ReceiverError, StdQueue},
    },
    logging::{debug, error, info, trace, warn},
    net_stack::NetStackHandle,
};
use bbqueue::prod_cons::framed::FramedConsumer;
use maitake_sync::WaitQueue;
use nusb::transfer::{Queue, RequestBuffer, TransferError};
use tokio::select;

/// How many in-flight requests at once — allows nusb to keep pulling
/// frames even if we haven't processed them host-side yet.
pub const IN_FLIGHT_REQS: usize = 4;

/// How many consecutive IN errors will we try to recover from before
/// giving up?
pub const MAX_STALL_RETRIES: usize = 3;

/// A generic nusb USB bulk RxWorker.
///
/// The caller is responsible for cleanup after [`run`](Self::run)
/// returns.
pub struct NusbRxWorker<N, P>
where
    N: NetStackHandle,
    P: FrameProcessor<N>,
{
    pub nsh: N,
    pub biq: Queue<RequestBuffer>,
    pub closer: Arc<WaitQueue>,
    pub processor: P,
    pub ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
    pub mtu: u16,
    pub state_notify: Option<Arc<WaitQueue>>,
}

impl<N, P> NusbRxWorker<N, P>
where
    N: NetStackHandle,
    P: FrameProcessor<N>,
{
    fn notify(&self) {
        if let Some(notify) = &self.state_notify {
            notify.wake_all();
        }
    }

    /// Run the receive loop with stall recovery.
    ///
    /// Returns `ReceiverError` when a fatal USB error occurs or
    /// the closer fires.
    pub async fn run(&mut self) -> ReceiverError {
        let mut consecutive_errs: usize = 0;

        loop {
            // Rehydrate the queue
            let pending = self.biq.pending();
            for _ in 0..(IN_FLIGHT_REQS.saturating_sub(pending)) {
                self.biq.submit(RequestBuffer::new(self.mtu as usize));
            }

            let close = self.closer.clone();

            let res = select! {
                r = self.biq.next_complete() => r,
                _clf = close.wait() => {
                    return ReceiverError::SocketClosed;
                }
            };

            if let Err(e) = res.status {
                consecutive_errs += 1;

                error!(
                    "In Worker error: {:?}, consecutive: {}",
                    e, consecutive_errs,
                );

                let recoverable = match e {
                    TransferError::Stall | TransferError::Unknown => {
                        consecutive_errs <= MAX_STALL_RETRIES
                    }
                    TransferError::Cancelled => false,
                    TransferError::Disconnected => false,
                    TransferError::Fault => false,
                };

                let fatal = if recoverable {
                    warn!("Attempting stall recovery!");

                    self.biq.cancel_all();
                    info!("Cancelled all in-flight requests");

                    for _ in 0..(IN_FLIGHT_REQS - 1) {
                        let res = self.biq.next_complete().await;
                        info!("Drain state: {:?}", res.status);
                    }

                    match self.biq.clear_halt() {
                        Ok(()) => false,
                        Err(e) => {
                            error!("Failed to clear stall: {:?}, Fatal.", e);
                            true
                        }
                    }
                } else {
                    error!(
                        "Giving up after {} errors in a row, final error: {:?}",
                        e, consecutive_errs,
                    );
                    true
                };

                if fatal {
                    error!("Fatal Error, exiting");
                    return ReceiverError::SocketClosed;
                } else {
                    info!("Potential recovery, resuming");
                    continue;
                }
            }

            if consecutive_errs != 0 {
                info!("Clearing consecutive error counter after good frame");
                consecutive_errs = 0;
            }

            trace!("Got message len {}", res.data.len());
            let changed = self
                .processor
                .process_frame(&res.data, &self.nsh, self.ident.clone());
            if changed {
                self.notify();
            }
        }
    }
}

/// A generic nusb USB bulk TxWorker.
///
/// Reads serialized frames from a [`FramedConsumer`] and submits them
/// to a nusb bulk OUT queue. Handles ZLP (zero-length packet) when
/// the frame size is a multiple of the USB max packet size.
///
/// On exit, calls `closer.close()` to ensure the RxWorker also
/// shuts down.
pub struct NusbTxWorker {
    pub boq: Queue<Vec<u8>>,
    pub consumer: FramedConsumer<StdQueue>,
    pub closer: Arc<WaitQueue>,
    pub max_usb_frame_size: Option<usize>,
}

impl NusbTxWorker {
    pub async fn run(mut self) {
        info!("Started nusb tx_worker");
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
            debug!("sending USB pkt len:{}", len);

            let needs_zlp = if let Some(mps) = &self.max_usb_frame_size {
                (len % mps) == 0
            } else {
                true
            };

            self.boq.submit(frame.to_vec());

            if needs_zlp {
                self.boq.submit(vec![]);
            }

            let send_res = self.boq.next_complete().await;
            if let Err(e) = send_res.status {
                error!("Output Queue Error: {:?}", e);
                break;
            }

            if needs_zlp {
                let send_res = self.boq.next_complete().await;
                if let Err(e) = send_res.status {
                    error!("Output Queue Error: {:?}", e);
                    break;
                }
            }

            frame.release();
        }
        warn!("Closing nusb tx_worker");
        self.closer.close();
    }
}

// ---------------------------------------------------------------------------
// Registration: DirectEdge
// ---------------------------------------------------------------------------

use crate::interface_manager::Interface;
use crate::interface_manager::InterfaceState;
use crate::interface_manager::interface_impls::nusb_bulk::NewDevice;
use crate::interface_manager::profiles::direct_edge::{DirectEdge, EdgeFrameProcessor};
use bbqueue::traits::bbqhdl::BbqHandle;

/// Registration error for DirectEdge.
#[derive(Debug, PartialEq)]
pub struct EdgeRegistrationError;

/// Register a nusb USB bulk transport on a [`DirectEdge`] profile.
///
/// `initial_state` and `processor` control target vs controller mode:
/// - Target: `InterfaceState::Inactive` with `EdgeFrameProcessor::new()`
/// - Controller: `InterfaceState::Active { net_id: 1, node_id: 1 }` with
///   `EdgeFrameProcessor::new_controller(1)`
pub async fn register_edge<N, I>(
    stack: N,
    device: NewDevice,
    queue: StdQueue,
    processor: EdgeFrameProcessor,
    initial_state: InterfaceState,
    max_ergot_packet_size: u16,
    state_notify: Option<Arc<WaitQueue>>,
) -> Result<(), EdgeRegistrationError>
where
    I: Interface,
    N: NetStackHandle<Profile = DirectEdge<I>> + Send + 'static,
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

    let mut rx_worker = NusbRxWorker {
        nsh: stack,
        biq: device.biq,
        closer: closer.clone(),
        processor,
        ident: (),
        mtu: max_ergot_packet_size,
        state_notify,
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
        NusbTxWorker {
            boq: device.boq,
            consumer: <StdQueue as BbqHandle>::framed_consumer(&queue),
            closer: closer.clone(),
            max_usb_frame_size: device.max_packet_size,
        }
        .run(),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Registration: Router
// ---------------------------------------------------------------------------

use crate::interface_manager::profiles::router::{Router, RouterFrameProcessor};
use crate::interface_manager::utils::framed_stream::Sink;
use crate::interface_manager::utils::std::new_std_queue;
use rand_core::RngCore;

/// Registration error for Router.
#[derive(Debug, PartialEq)]
pub struct RouterRegistrationError;

/// Register a nusb USB bulk transport on a [`Router`] profile.
pub async fn register_router<N, I, Rng, const M: usize, const SS: usize>(
    stack: N,
    device: NewDevice,
    max_ergot_packet_size: u16,
    outgoing_buffer_size: usize,
    state_notify: Option<Arc<WaitQueue>>,
) -> Result<u8, RouterRegistrationError>
where
    I: Interface<Sink = Sink<StdQueue>>,
    Rng: RngCore + Send + 'static,
    N: NetStackHandle<Profile = Router<I, Rng, M, SS>> + Send + 'static,
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

    let notify_clone = state_notify.clone();
    let nsh_clone = stack.clone();

    let mut rx_worker = NusbRxWorker {
        nsh: stack.clone(),
        biq: device.biq,
        closer: closer.clone(),
        processor: RouterFrameProcessor::new(net_id),
        ident,
        mtu: max_ergot_packet_size,
        state_notify,
    };

    stack.stack().manage_profile(|im| {
        im.set_interface_closer(ident, closer.clone());
    });

    tokio::task::spawn(async move {
        let close = rx_worker.closer.clone();
        select! {
            _run = rx_worker.run() => {
                close.close();
            },
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
        NusbTxWorker {
            boq: device.boq,
            consumer: <StdQueue as BbqHandle>::framed_consumer(&q),
            closer: closer.clone(),
            max_usb_frame_size: device.max_packet_size,
        }
        .run(),
    );

    Ok(ident)
}
