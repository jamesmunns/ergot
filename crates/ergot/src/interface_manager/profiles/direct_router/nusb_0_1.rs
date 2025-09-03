//! An NUSB based DirectRouter
//!
//! This implementation can be used to connect to a number of direct edge USB devices.

use bbq2::{prod_cons::framed::FramedConsumer, traits::bbqhdl::BbqHandle};
use log::{debug, error, info, trace, warn};
use maitake_sync::WaitQueue;
use nusb::transfer::{Queue, RequestBuffer, TransferError};
use std::sync::Arc;
use tokio::select;

use crate::{
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::nusb_bulk::{NewDevice, NusbBulk},
        profiles::direct_router::{DirectRouter, process_frame},
        utils::{
            framed_stream::Sink,
            std::{ReceiverError, StdQueue, new_std_queue},
        },
    },
    net_stack::NetStackHandle,
};
/// How many in-flight requests at once - allows nusb to keep pulling frames
/// even if we haven't processed them host-side yet.
pub(crate) const IN_FLIGHT_REQS: usize = 4;
/// How many consecutive IN errors will we try to recover from before giving up?
pub(crate) const MAX_STALL_RETRIES: usize = 3;

#[derive(Debug, PartialEq)]
pub enum Error {
    OutOfNetIds,
}

struct TxWorker {
    net_id: u16,
    boq: Queue<Vec<u8>>,
    rx: FramedConsumer<StdQueue>,
    closer: Arc<WaitQueue>,
    max_usb_frame_size: Option<usize>,
}

struct RxWorker<N>
where
    N: NetStackHandle<Profile = DirectRouter<NusbBulk>>,
    N: Send + 'static,
{
    interface_id: u64,
    net_id: u16,
    nsh: N,
    biq: Queue<RequestBuffer>,
    closer: Arc<WaitQueue>,
    mtu: u16,
    consecutive_errs: usize,
}

impl TxWorker {
    async fn run(mut self) {
        self.run_inner().await;
        warn!("Closing interface {}", self.net_id);
        self.closer.close();
    }

    async fn run_inner(&mut self) {
        info!("Started tx_worker for net_id {}", self.net_id);
        loop {
            let rxf = self.rx.wait_read();
            let clf = self.closer.wait();

            let frame = select! {
                r = rxf => r,
                _c = clf => {
                    return;
                }
            };

            let len = frame.len();
            debug!("sending pkt len:{} on net_id {}", len, self.net_id);

            let needs_zlp = if let Some(mps) = &self.max_usb_frame_size {
                (len % mps) == 0
            } else {
                true
            };

            self.boq.submit(frame.to_vec());

            // Append ZLP if we are a multiple of max packet
            if needs_zlp {
                self.boq.submit(vec![]);
            }

            let send_res = self.boq.next_complete().await;
            if let Err(e) = send_res.status {
                error!("Output Queue Error: {e:?}");
                return;
            }

            if needs_zlp {
                let send_res = self.boq.next_complete().await;
                if let Err(e) = send_res.status {
                    error!("Output Queue Error: {e:?}");
                    return;
                }
            }

            frame.release();
        }
    }
}

impl<N> RxWorker<N>
where
    N: NetStackHandle<Profile = DirectRouter<NusbBulk>>,
    N: Send + 'static,
{
    async fn run(mut self) {
        let close = self.closer.clone();

        // Wait for the receiver to encounter an error, or wait for
        // the transmitter to signal that it observed an error
        select! {
            run = self.run_inner() => {
                // Halt the TX worker
                self.closer.close();
                error!("Receive Error: {run:?}");
            },
            _clf = close.wait() => {},
        }

        // Remove this interface from the list
        self.nsh.stack().manage_profile(|im| {
            _ = im.deregister_interface(self.interface_id);
        });
    }

    pub async fn run_inner(&mut self) -> ReceiverError {
        loop {
            // Rehydrate the queue
            let pending = self.biq.pending();
            for _ in 0..(IN_FLIGHT_REQS.saturating_sub(pending)) {
                self.biq.submit(RequestBuffer::new(self.mtu as usize));
            }

            let res = self.biq.next_complete().await;

            if let Err(e) = res.status {
                self.consecutive_errs += 1;

                error!(
                    "In Worker error: {e:?}, consecutive: {}",
                    self.consecutive_errs
                );

                // Docs only recommend this for Stall, but it seems to work with
                // UNKNOWN on MacOS as well, todo: look into why!
                //
                // Update: This stall condition seems to have been due to an errata in the
                // STM32F4 USB hardware. See https://github.com/embassy-rs/embassy/pull/2823
                //
                // It is now questionable whether we should be doing this stall recovery at all,
                // as it likely indicates an issue with the connected USB device
                let recoverable = match e {
                    TransferError::Stall | TransferError::Unknown => {
                        self.consecutive_errs <= MAX_STALL_RETRIES
                    }
                    TransferError::Cancelled => false,
                    TransferError::Disconnected => false,
                    TransferError::Fault => false,
                };

                let fatal = if recoverable {
                    warn!("Attempting stall recovery!");

                    // Stall recovery shouldn't be used with in-flight requests, so
                    // cancel them all. They'll still pop out of next_complete.
                    self.biq.cancel_all();
                    info!("Cancelled all in-flight requests");

                    // Now we need to join all in flight requests
                    for _ in 0..(IN_FLIGHT_REQS - 1) {
                        let res = self.biq.next_complete().await;
                        info!("Drain state: {:?}", res.status);
                    }

                    // Now we can mark the stall as clear
                    match self.biq.clear_halt() {
                        Ok(()) => false,
                        Err(e) => {
                            error!("Failed to clear stall: {e:?}, Fatal.");
                            true
                        }
                    }
                } else {
                    error!(
                        "Giving up after {} errors in a row, final error: {e:?}",
                        self.consecutive_errs
                    );
                    true
                };

                if fatal {
                    error!("Fatal Error, exiting");
                    // When we close the channel, all pending receivers and subscribers
                    // will be notified
                    return ReceiverError::SocketClosed;
                } else {
                    info!("Potential recovery, resuming NusbWireRx::recv_inner");
                    continue;
                }
            }

            // If we get a good decode, clear the error flag
            if self.consecutive_errs != 0 {
                info!("Clearing consecutive error counter after good header decode");
                self.consecutive_errs = 0;
            }

            trace!("Got message len {}", res.data.len());
            process_frame(self.net_id, &res.data, &self.nsh, self.interface_id);
        }
    }
}

pub async fn register_interface<N>(
    stack: N,
    device: NewDevice,
    max_ergot_packet_size: u16,
    outgoing_buffer_size: usize,
) -> Result<u64, Error>
where
    N: NetStackHandle<Profile = DirectRouter<NusbBulk>>,
    N: Send + 'static,
{
    let q: StdQueue = new_std_queue(outgoing_buffer_size);
    let res = stack.stack().manage_profile(|im| {
        let ident =
            im.register_interface(Sink::new_from_handle(q.clone(), max_ergot_packet_size))?;
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
        return Err(Error::OutOfNetIds);
    };
    let closer = Arc::new(WaitQueue::new());
    let rx_worker = RxWorker {
        nsh: stack.clone(),
        closer: closer.clone(),
        mtu: max_ergot_packet_size,
        interface_id: ident,
        net_id,
        biq: device.biq,
        consecutive_errs: 0,
    };
    let tx_worker = TxWorker {
        net_id,
        rx: <StdQueue as BbqHandle>::framed_consumer(&q),
        closer,
        boq: device.boq,
        max_usb_frame_size: device.max_packet_size,
    };

    tokio::task::spawn(rx_worker.run());
    tokio::task::spawn(tx_worker.run());

    Ok(ident)
}
