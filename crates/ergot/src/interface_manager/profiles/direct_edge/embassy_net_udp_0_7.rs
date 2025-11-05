use bbq2::prod_cons::framed::FramedConsumer;
use bbq2::queue::BBQueue;
use bbq2::traits::coordination::Coord;
use bbq2::traits::notifier::maitake::MaiNotSpsc;
use bbq2::traits::storage::Inline;
use defmt::{error, trace};
use embassy_futures::select::{Either, select};
use embassy_net_0_7::udp::{RecvError, SendError, UdpMetadata, UdpSocket};
use crate::interface_manager::profiles::direct_edge::{CENTRAL_NODE_ID, EDGE_NODE_ID, process_frame};
use crate::interface_manager::{InterfaceState, Profile};
use crate::net_stack::NetStackHandle;
use crate::wire_frames::MAX_HDR_ENCODED_SIZE;

pub const UDP_OVER_ETH_ERGOT_FRAME_SIZE_MAX: usize = 1500 - 8 - 20;
pub const UDP_OVER_ETH_ERGOT_PAYLOAD_SIZE_MAX: usize = UDP_OVER_ETH_ERGOT_FRAME_SIZE_MAX - MAX_HDR_ENCODED_SIZE;
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
    consumer: FramedConsumer<&'static BBQueue<Inline<NN>, C, MaiNotSpsc>>,
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
        consumer: FramedConsumer<&'static BBQueue<Inline<NN>, C, MaiNotSpsc>>,
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
        consumer: FramedConsumer<&'static BBQueue<Inline<NN>, C, MaiNotSpsc>>,
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

    pub async fn run(&mut self, scratch: &mut [u8]) -> Result<(), RxTxError> {
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

        let res = self.run_inner(scratch).await;
        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state(self.ident.clone(), InterfaceState::Down));
        res
    }

    pub async fn run_inner(&mut self, scratch: &mut [u8]) -> Result<(), RxTxError> {
        let Self {
            nsh,
            socket,
            net_id,
            ident,
            is_controller: _,
            consumer: rx,
            remote_endpoint,
        } = self;
        loop {
            trace!("Waiting for data from socket or tx queue");
            let a = socket.recv_from(scratch);
            let b = rx.wait_read();

            match select(a, b).await {
                Either::First(recv_result) => {
                    trace!("Socket future");
                    // TODO compare the metadata.endpoint to self.remote_endpoint and possibly reject
                    let (used, metadata) = recv_result.map_err(RxTxError::RxError)?;
                    trace!("Received data from socket. used: {}, metadata: {:?}", used, metadata);

                    let data = &scratch[..used];

                    process_frame(net_id, data, nsh, ident.clone());
                }
                Either::Second(data) => {
                    trace!("Tx queue future");
                    socket
                        .send_to(&data, *remote_endpoint)
                        .await
                        .map_err(RxTxError::TxError)?;
                    trace!("Sent data to socket");
                    data.release();
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
