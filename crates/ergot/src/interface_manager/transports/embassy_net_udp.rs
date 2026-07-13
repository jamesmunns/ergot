//! Generic embassy-net UDP combined RX/TX worker.
//!
//! Works with any [`FrameProcessor`]. UDP datagrams are treated as
//! complete frames (no COBS encoding). RX and TX share a single
//! async task and embassy-net UDP socket.
//!
//! [`FrameProcessor`]: crate::interface_manager::FrameProcessor

use crate::interface_manager::{FrameProcessor, InterfaceState, Profile};
use crate::logging::{error, trace, warn};
use crate::net_stack::NetStackHandle;
use crate::wire_frames::MAX_HDR_ENCODED_SIZE;
use bbqueue::BBQueue;
use bbqueue::prod_cons::framed::FramedConsumer;
use bbqueue::traits::coordination::Coord;
use bbqueue::traits::notifier::maitake::MaiNotSpsc;
use bbqueue::traits::storage::Inline;
use embassy_futures::select::{Either, select};
use embassy_net_0_7::udp::{RecvError, SendError, UdpMetadata, UdpSocket};

pub const UDP_OVER_ETH_ERGOT_FRAME_SIZE_MAX: usize = 1500 - 8 - 20;
pub const UDP_OVER_ETH_ERGOT_PAYLOAD_SIZE_MAX: usize =
    UDP_OVER_ETH_ERGOT_FRAME_SIZE_MAX - MAX_HDR_ENCODED_SIZE;

#[derive(Debug, PartialEq)]
pub struct SocketAlreadyActive;

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum RxTxError {
    TxError(SendError),
    RxError(RecvError),
}

/// A generic embassy-net UDP combined RX/TX worker.
///
/// The caller sets up the initial interface state before calling
/// [`run`](Self::run). On exit (or drop), the interface transitions
/// to [`InterfaceState::Down`].
pub struct RxTxWorker<const NN: usize, N, C, P>
where
    N: NetStackHandle,
    C: Coord + 'static,
    P: FrameProcessor<N>,
{
    nsh: N,
    socket: UdpSocket<'static>,
    processor: P,
    ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
    consumer: FramedConsumer<&'static BBQueue<Inline<NN>, C, MaiNotSpsc>>,
    remote_endpoint: UdpMetadata,
}

impl<const NN: usize, N, C, P> RxTxWorker<NN, N, C, P>
where
    N: NetStackHandle,
    C: Coord,
    P: FrameProcessor<N>,
{
    /// Create a new combined RX/TX worker.
    pub fn new<EP>(
        nsh: N,
        socket: UdpSocket<'static>,
        processor: P,
        ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
        consumer: FramedConsumer<&'static BBQueue<Inline<NN>, C, MaiNotSpsc>>,
        remote_endpoint: EP,
    ) -> Self
    where
        EP: Into<UdpMetadata>,
    {
        Self {
            nsh,
            socket,
            processor,
            ident,
            consumer,
            remote_endpoint: remote_endpoint.into(),
        }
    }

    /// Run the combined RX/TX loop with a given initial state.
    ///
    /// Sets `initial_state` before entering the loop. On exit (or drop),
    /// the interface transitions to [`InterfaceState::Down`].
    pub async fn run(
        &mut self,
        initial_state: InterfaceState,
        scratch: &mut [u8],
    ) -> Result<(), RxTxError> {
        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state(self.ident.clone(), initial_state))
            .inspect_err(|err| {
                error!("Error setting interface state: {:?}", err);
            });

        let res = self.run_inner(scratch).await;
        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state(self.ident.clone(), InterfaceState::Down));
        res
    }

    async fn run_inner(&mut self, scratch: &mut [u8]) -> Result<(), RxTxError> {
        let Self {
            nsh,
            socket,
            processor,
            ident,
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
                    let (used, metadata) = match recv_result {
                        Ok(v) => v,
                        // The only possible recv error is a datagram larger than
                        // `scratch`. Dropping one oversized datagram must not take
                        // the interface down — otherwise a remote peer could
                        // silence it with a single packet — so log and continue,
                        // mirroring the tokio UDP transport.
                        Err(RecvError::Truncated) => {
                            warn!("dropping oversized UDP datagram (larger than RX buffer)");
                            continue;
                        }
                    };
                    trace!(
                        "Received data from socket. used: {}, metadata: {:?}",
                        used, metadata
                    );

                    let data = &scratch[..used];
                    processor.process_frame(data, nsh, ident.clone());
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

impl<const NN: usize, N, C, P> Drop for RxTxWorker<NN, N, C, P>
where
    N: NetStackHandle,
    C: Coord,
    P: FrameProcessor<N>,
{
    fn drop(&mut self) {
        self.nsh.stack().manage_profile(|im| {
            _ = im.set_interface_state(self.ident.clone(), InterfaceState::Down);
        })
    }
}
