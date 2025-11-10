//! A UDP based DirectRouter
//!
//! This implementation can be used to connect to a number of direct edge UDP devices.

use crate::logging::{debug, error, info, trace, warn};
use bbq2::{prod_cons::framed::FramedConsumer, traits::bbqhdl::BbqHandle};
use maitake_sync::WaitQueue;
use std::sync::Arc;
use tokio::{net::UdpSocket, select};

use crate::{
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::tokio_udp::TokioUdpInterface,
        profiles::direct_router::{DirectRouter, process_frame},
        utils::{
            framed_stream::Sink,
            std::{ReceiverError, StdQueue, new_std_queue},
        },
    },
    net_stack::NetStackHandle,
};

#[derive(Debug, PartialEq)]
pub enum Error {
    OutOfNetIds,
}

struct TxWorker {
    net_id: u16,
    tx: Arc<UdpSocket>,
    rx: FramedConsumer<StdQueue>,
    closer: Arc<WaitQueue>,
}

struct RxWorker<N>
where
    N: NetStackHandle<Profile = DirectRouter<TokioUdpInterface>>,
    N: Send + 'static,
{
    interface_id: u64,
    net_id: u16,
    nsh: N,
    skt: Arc<UdpSocket>,
    closer: Arc<WaitQueue>,
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
                    break;
                }
            };

            let len = frame.len();
            debug!("sending pkt len:{} on net_id {}", len, self.net_id);
            let res = self.tx.send(&frame).await;
            frame.release();
            if let Err(e) = res {
                error!("Err: {e:?}");
                break;
            }
        }
    }
}

impl<N> RxWorker<N>
where
    N: NetStackHandle<Profile = DirectRouter<TokioUdpInterface>>,
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
        let mut raw_buf = vec![0u8; 4096].into_boxed_slice();

        loop {
            let rd = self.skt.recv_from(&mut raw_buf);
            let close = self.closer.wait();

            let ct = select! {
                r = rd => {
                    match r {
                        Ok((0, _)) => {
                            warn!("received nothing, retrying");
                            continue
                        },
                        Err(e) => {
                            warn!("receiver error, retrying. error: {}, kind: {}", e, e.kind());
                            continue
                        },
                        Ok((ct, remote_address)) => {
                            // TODO ensure the remote address is allowed to connect to this router
                            //      this implementation blindly accepts all connections
                            trace!("received {} bytes from {}", ct, remote_address);
                            ct
                        },
                    }
                }
                _c = close => {
                    return ReceiverError::SocketClosed;
                }
            };

            let buf = &mut raw_buf[..ct];
            process_frame(self.net_id, buf, &self.nsh, self.interface_id);
        }
    }
}

pub async fn register_interface<N>(
    stack: N,
    socket: UdpSocket,
    max_ergot_packet_size: u16,
    outgoing_buffer_size: usize,
) -> Result<u64, Error>
where
    N: NetStackHandle<Profile = DirectRouter<TokioUdpInterface>>,
    N: Send + 'static,
{
    let arc_socket = Arc::new(socket);
    let (rx, tx) = (arc_socket.clone(), arc_socket);

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
        skt: rx,
        closer: closer.clone(),
        interface_id: ident,
        net_id,
    };
    let tx_worker = TxWorker {
        net_id,
        tx,
        rx: <StdQueue as BbqHandle>::framed_consumer(&q),
        closer,
    };

    tokio::task::spawn(rx_worker.run());
    tokio::task::spawn(tx_worker.run());

    Ok(ident)
}
