use bbq2::prod_cons::stream::StreamConsumer;
use bbq2::queue::BBQueue;
use bbq2::traits::coordination::Coord;
use bbq2::traits::notifier::maitake::MaiNotSpsc;
use bbq2::traits::storage::Inline;
use cobs_acc::{CobsAccumulator, FeedResult};
use defmt::{error, trace};
use embassy_futures::select::{Either, select};
use embassy_net_0_7::udp::{RecvError, SendError, UdpMetadata, UdpSocket};

use crate::interface_manager::profiles::direct_edge::{CENTRAL_NODE_ID, EDGE_NODE_ID, process_frame};
use crate::interface_manager::{InterfaceState, Profile};
use crate::net_stack::NetStackHandle;

#[derive(Debug, PartialEq)]
pub struct SocketAlreadyActive;

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum RxTxError {
    TxError(SendError),
    RxError(RecvError),
}

pub struct RxTxWorker<const NN: usize, N, C>
where
    N: NetStackHandle,
    C: Coord + 'static,
{
    nsh: N,
    socket: UdpSocket<'static>,
    net_id: Option<u16>,
    ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
    is_controller: bool,
    consumer: StreamConsumer<&'static BBQueue<Inline<NN>, C, MaiNotSpsc>>,
    remote_endpoint: UdpMetadata,
}

impl<const NN: usize, N, C> RxTxWorker<NN, N, C>
where
    N: NetStackHandle,
    C: Coord,
{
    pub fn new_target<EP>(
        net: N,
        socket: UdpSocket<'static>,
        ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
        consumer: StreamConsumer<&'static BBQueue<Inline<NN>, C, MaiNotSpsc>>,
        remote_endpoint: EP,
    ) -> Self
    where
        EP: Into<UdpMetadata>,
    {
        Self {
            nsh: net,
            socket,
            net_id: None,
            ident,
            is_controller: false,
            consumer,
            remote_endpoint: remote_endpoint.into(),
        }
    }

    pub fn new_controller<EP>(
        net: N,
        socket: UdpSocket<'static>,
        ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
        consumer: StreamConsumer<&'static BBQueue<Inline<NN>, C, MaiNotSpsc>>,
        remote_endpoint: EP,
    ) -> Self
    where
        EP: Into<UdpMetadata>,
    {
        Self {
            nsh: net,
            socket,
            net_id: None,
            ident,
            is_controller: true,
            consumer,
            remote_endpoint: remote_endpoint.into(),
        }
    }

    pub async fn run(&mut self, frame: &mut [u8], scratch: &mut [u8]) -> Result<(), RxTxError> {
        // Mark the interface as established
        _ = self
            .nsh
            .stack()
            .manage_profile(|im| {
                if self.is_controller {
                    trace!("UDP controller is active");
                    self.net_id = Some(1);
                    im.set_interface_state(self.ident.clone(), InterfaceState::Active {
                        net_id: 1,
                        node_id: CENTRAL_NODE_ID,
                    })
                } else {
                    trace!("UDP target is active");
                    self.net_id = Some(1);
                    im.set_interface_state(self.ident.clone(), InterfaceState::Active {
                        net_id: 1,
                        node_id: EDGE_NODE_ID,
                    })
                }
            })
            .inspect_err(|err| error!("Error setting interface state: {:?}", err));

        let res = self.run_inner(frame, scratch).await;
        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state(self.ident.clone(), InterfaceState::Down));
        res
    }

    pub async fn run_inner(&mut self, frame: &mut [u8], scratch: &mut [u8]) -> Result<(), RxTxError> {
        let mut acc = CobsAccumulator::new(frame);
        let Self {
            nsh,
            socket,
            net_id,
            ident,
            is_controller: _,
            consumer: rx,
            remote_endpoint,
        } = self;
        'outer: loop {
            trace!("Waiting for data from socket or tx queue");
            let a = socket.recv_from(scratch);
            let b = rx.wait_read();

            match select(a, b).await {
                Either::First(recv_result) => {
                    trace!("Socket future");
                    // TODO compare the metadata.endpoint to self.remote_endpoint and possibly reject
                    let (used, metadata) = recv_result.map_err(|e| RxTxError::RxError(e))?;
                    trace!("Received data from socket. used: {}, metadata: {:?}", used, metadata);

                    let mut remain = &mut scratch[..used];

                    loop {
                        trace!("remaining: {}, bytes: {:?}", remain.len(), remain);
                        match acc.feed_raw(remain) {
                            FeedResult::Consumed => {
                                trace!("consumed");
                                continue 'outer;
                            }
                            FeedResult::OverFull(items) => {
                                trace!("overfull. items: {}", items);
                                remain = items;
                            }
                            FeedResult::DecodeError(items) => {
                                trace!("decode error. items: {}", items);
                                remain = items;
                            }
                            FeedResult::Success {
                                data,
                                remaining,
                            }
                            | FeedResult::SuccessInput {
                                data,
                                remaining,
                            } => {
                                trace!("success. data: {}, remaining: {}", data.len(), remaining.len());
                                process_frame(net_id, data, nsh, ident.clone());
                                remain = remaining;
                            }
                        }
                    }
                }
                Either::Second(data) => {
                    trace!("Tx queue future");
                    let size = data.len();
                    socket
                        .send_to(&data, *remote_endpoint)
                        .await
                        .map_err(|e| RxTxError::TxError(e))?;
                    trace!("Sent data to socket");
                    data.release(size);
                }
            }
        }
    }
}

impl<const NN: usize, N, C> Drop for RxTxWorker<NN, N, C>
where
    N: NetStackHandle,
    C: Coord,
{
    fn drop(&mut self) {
        // No receiver? Drop the interface.
        self.nsh.stack().manage_profile(|im| {
            _ = im.set_interface_state(self.ident.clone(), InterfaceState::Down);
        })
    }
}
