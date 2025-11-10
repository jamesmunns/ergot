//! A std+udp edge device profile
//!
//! This is useful for std based devices/applications that can directly connect to a DirectRouter
//! using a udp connection.

use std::sync::Arc;

use crate::{
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::tokio_udp::TokioUdpInterface,
        profiles::direct_edge::{DirectEdge, process_frame},
        utils::std::{ReceiverError, StdQueue},
    },
    net_stack::NetStackHandle,
};

use crate::interface_manager::profiles::direct_edge::{CENTRAL_NODE_ID, EDGE_NODE_ID};
use crate::logging::{error, info, trace, warn};
use bbq2::prod_cons::framed::FramedConsumer;
use bbq2::traits::bbqhdl::BbqHandle;
use maitake_sync::WaitQueue;
use tokio::{net::UdpSocket, select};

pub type StdUdpClientIm = DirectEdge<TokioUdpInterface>;

pub struct RxWorker<N: NetStackHandle> {
    stack: N,
    skt: Arc<UdpSocket>,
    closer: Arc<WaitQueue>,
    net_id: u16,
}

// ---- impls ----

impl<N> RxWorker<N>
where
    N: NetStackHandle<Profile = DirectEdge<TokioUdpInterface>>,
{
    pub async fn run(mut self) -> Result<(), ReceiverError> {
        info!("Started rx_worker");

        let res = self.run_inner().await;

        info!("Finished rx_worker");

        self.stack.stack().manage_profile(|im| {
            _ = im.set_interface_state((), InterfaceState::Down);
        });
        res
    }

    pub async fn run_inner(&mut self) -> Result<(), ReceiverError> {
        let mut raw_buf = [0u8; 4096];
        let mut net_id = Some(self.net_id);

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
                            warn!("receiver error, retrying. error: {}", e);
                            continue
                            //return Err(ReceiverError::SocketClosed)
                        },
                        Ok((ct, remote_address)) => {
                            // TODO ensure the remote address is allowed to connect to this edge
                            //      this implementation blindly accepts all connections
                            trace!("received {} bytes from {}", ct, remote_address);
                            ct
                        },
                    }
                }
                _c = close => {
                    return Err(ReceiverError::SocketClosed);
                }
            };

            let buf = &mut raw_buf[..ct];
            process_frame(&mut net_id, buf, &self.stack, ());
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct SocketAlreadyActive;

// Helper functions

pub enum InterfaceKind {
    Target,
    Controller,
}

pub async fn register_interface<N>(
    stack: N,
    socket: UdpSocket,
    queue: StdQueue,
    interface_kind: InterfaceKind,
    ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
) -> Result<(), SocketAlreadyActive>
where
    N: NetStackHandle<Profile = DirectEdge<TokioUdpInterface>>,
    N: Send + 'static,
{
    let arc_socket = Arc::new(socket);
    let (rx, tx) = (arc_socket.clone(), arc_socket);

    let net_id = 1_u16;

    let closer = Arc::new(WaitQueue::new());
    stack.stack().manage_profile(|im| {
        match im.interface_state(()) {
            Some(InterfaceState::Down) => {}
            Some(InterfaceState::Inactive) => return Err(SocketAlreadyActive),
            Some(InterfaceState::ActiveLocal { .. }) => return Err(SocketAlreadyActive),
            Some(InterfaceState::Active { .. }) => return Err(SocketAlreadyActive),
            None => {}
        }

        match interface_kind {
            InterfaceKind::Controller => {
                trace!("UDP controller is active");
                im.set_interface_state(
                    ident,
                    InterfaceState::Active {
                        net_id,
                        node_id: CENTRAL_NODE_ID,
                    },
                )
            }
            InterfaceKind::Target => {
                trace!("UDP target is active");
                im.set_interface_state(
                    ident,
                    InterfaceState::Active {
                        net_id,
                        node_id: EDGE_NODE_ID,
                    },
                )
            }
        }
        .map_err(|_| SocketAlreadyActive)?;

        Ok(())
    })?;
    let rx_worker = RxWorker {
        stack,
        skt: rx,
        closer: closer.clone(),
        net_id,
    };
    // TODO: spawning in a non-async context!
    tokio::task::spawn(tx_worker(tx, queue.framed_consumer(), closer.clone()));
    tokio::task::spawn(rx_worker.run());
    Ok(())
}

async fn tx_worker(tx: Arc<UdpSocket>, rx: FramedConsumer<StdQueue>, closer: Arc<WaitQueue>) {
    info!("Started tx_worker");
    loop {
        let rxf = rx.wait_read();
        let clf = closer.wait();

        let frame = select! {
            r = rxf => r,
            _c = clf => {
                break;
            }
        };

        let len = frame.len();
        trace!("sending pkt len:{}", len);
        let res = tx.send(&frame).await;
        frame.release();
        if let Err(e) = res {
            error!("Err: {e:?}");
            break;
        }
    }
    // TODO: GC waker?
    warn!("Closing interface");
}
