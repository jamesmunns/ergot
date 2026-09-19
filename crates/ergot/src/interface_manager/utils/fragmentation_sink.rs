use core::{fmt::Debug, marker::PhantomData, pin::pin};

use bbqueue::{
    prod_cons::framed::{FramedConsumer, FramedProducer},
    traits::{bbqhdl::BbqHandle, notifier::AsyncNotifier},
};
use embassy_futures::select::Either;
use postcard::{
    Serializer,
    ser_flavors::{Flavor, Slice},
};
use serde::{Deserialize, Serialize};

use crate::{
    Address, AnyAllAppendix, HeaderSeq,
    interface_manager::{
        FragmentError, FragmentPacketError, FragmentRequestError, Interface, InterfaceSink, Profile,
    },
    logging::{error, trace, warn},
    net_stack::NetStackHandle,
    well_known::{
        ErgotFragmentPacketEndpoint, ErgotFragmentRequestEndpoint, FragmentPacket, FragmentRequest,
        FragmentRequestResponse,
    },
    wire_frames::{self, CommonHeader, de_frame, encode_frame_hdr},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FragmentationHeader {
    max_inner_size: u16,
    dst: Address,
}

/// How the fragmentation service should handle a non-fatal issue that has come up
pub enum FragmentationIssueResolution {
    Retry,
    Drop,
    RaiseError,
}

// REGISTRY_SIZE should be the MTU divided and rounded up (div_ceil) by the package size (BAR_SIZE in FragmentationConfig) and then divided by eight
// Without nightly there is currently no way to calculate that automatically
#[derive(Debug, Clone, Copy)]
pub struct FragmentationBuffer<const MTU: usize, const REGISTRY_SIZE: usize> {
    // To know (in combination with received_size) when the message is fully received
    pub(crate) complete_size: u16,
    pub(crate) received_size: u16,
    pub(crate) packet_size: u16,
    pub(crate) buffer: [u8; MTU],
    pub(crate) reserved: bool,
    // The registry is used to make sure we don't apply and count a package twice.
    pub(crate) registry: [u8; REGISTRY_SIZE],
}

impl<const MTU: usize, const REGISTRY_SIZE: usize> FragmentationBuffer<MTU, REGISTRY_SIZE> {
    pub const fn new() -> Self {
        Self {
            complete_size: 0,
            received_size: 0,
            packet_size: 0,
            reserved: false,
            buffer: [0; MTU],
            registry: [0; REGISTRY_SIZE],
        }
    }
}

impl<const MTU: usize, const REGISTRY_SIZE: usize> Default
    for FragmentationBuffer<MTU, REGISTRY_SIZE>
{
    fn default() -> Self {
        Self::new()
    }
}

pub trait FragmentationIssueHandler: Clone {
    fn handle_issue(
        &self,
        error: FragmentError,
        addr: Address,
    ) -> impl Future<Output = FragmentationIssueResolution>;
    fn handle_error(&self, err: FragmentError);
}

#[derive(Clone)]
pub struct DefaultFragmentationIssueHandler;

impl FragmentationIssueHandler for DefaultFragmentationIssueHandler {
    async fn handle_issue(
        &self,
        _error: FragmentError,
        _addr: Address,
    ) -> FragmentationIssueResolution {
        FragmentationIssueResolution::Drop
    }

    fn handle_error(&self, _err: FragmentError) {}
}

pub struct FragmentationConfig<
    Q,
    H: FragmentationIssueHandler,
    const MTU: usize,
    const N: usize,
    const BAR_SIZE: usize,
    const REGISTRY_SIZE: usize,
> where
    Q: BbqHandle,
{
    pub(crate) cons: FramedConsumer<Q>,
    pub(crate) handler: H,
    pub(crate) receive_buffer: [FragmentationBuffer<MTU, REGISTRY_SIZE>; N],
}

pub struct FragmentationSinkInterface<I, Q> {
    _s: PhantomData<I>,
    _q: PhantomData<Q>,
}

impl<I, Q> Interface for FragmentationSinkInterface<I, Q>
where
    I: Interface,
    Q: BbqHandle,
{
    type Sink = Sink<I::Sink, Q>;
}

pub struct Sink<S, Q>
where
    S: InterfaceSink,
    Q: BbqHandle,
{
    pub(crate) inner_sink: S,
    pub(crate) mtu: u16,
    pub(crate) prod: FramedProducer<Q>,
    pub(crate) max_inner_msg_size: usize,
    pub(crate) any_handling: bool,
}

impl<S, Q> InterfaceSink for Sink<S, Q>
where
    Q: BbqHandle,
    S: InterfaceSink,
{
    const MAX_HEADER_SIZE: usize = 4;

    fn mtu(&self) -> u16 {
        self.mtu
    }

    fn send_ty<T: serde::Serialize>(&mut self, hdr: &HeaderSeq, body: &T) -> Result<(), ()> {
        let msg_size = size_of::<T>();

        let max_inner_msg_size = if self.any_handling && hdr.any_all.is_none() {
            self.max_inner_msg_size + size_of::<AnyAllAppendix>()
        } else {
            self.max_inner_msg_size
        };

        if msg_size > max_inner_msg_size {
            if hdr.dst.port_id == 255 {
                error!("The fragmentation sink does not support the fragmentation of broadcasts");
                return Err(());
            }
            let mut wgr = self
                .prod
                .grant(self.mtu() + Self::MAX_HEADER_SIZE as u16)
                .map_err(drop)?;
            let mut serializer = Serializer {
                output: Slice::new(&mut wgr),
            };

            let footer = FragmentationHeader {
                dst: hdr.dst,
                max_inner_size: self.max_inner_msg_size as u16,
            };
            footer.serialize(&mut serializer).map_err(drop)?;

            wire_frames::encode_frame_hdr(&mut serializer, hdr).map_err(drop)?;

            body.serialize(&mut serializer).map_err(drop)?;
            let used = serializer.output.finalize().map_err(drop)?;
            let len = used.len() as u16;
            wgr.commit(len);
            Ok(())
        } else {
            self.inner_sink.send_ty(hdr, body)
        }
    }

    fn send_err(
        &mut self,
        hdr: &crate::prelude::HeaderSeq,
        err: crate::prelude::ProtocolError,
    ) -> Result<(), ()> {
        let msg_size = size_of::<crate::prelude::ProtocolError>();

        let max_inner_msg_size = if self.any_handling && hdr.any_all.is_none() {
            self.max_inner_msg_size + size_of::<AnyAllAppendix>()
        } else {
            self.max_inner_msg_size
        };

        if msg_size > max_inner_msg_size {
            if hdr.dst.port_id == 255 {
                error!("The fragmentation sink does not support the fragmentation of broadcasts");
                return Err(());
            }
            let mut wgr = self
                .prod
                .grant(self.mtu() + Self::MAX_HEADER_SIZE as u16)
                .map_err(drop)?;
            let mut serializer = Serializer {
                output: Slice::new(&mut wgr),
            };

            let footer = FragmentationHeader {
                dst: hdr.dst,
                max_inner_size: self.max_inner_msg_size as u16,
            };
            footer.serialize(&mut serializer).map_err(drop)?;

            let chdr: CommonHeader = hdr.into();
            chdr.serialize(&mut serializer).map_err(drop)?;
            err.serialize(&mut serializer).map_err(drop)?;

            let used = serializer.output.finalize().map_err(drop)?;
            let len = used.len() as u16;
            wgr.commit(len);
            Ok(())
        } else {
            self.inner_sink.send_err(hdr, err)
        }
    }

    fn send_raw(&mut self, hdr: &crate::prelude::HeaderSeq, body: &[u8]) -> Result<(), ()> {
        let max_inner_msg_size = if self.any_handling && hdr.any_all.is_none() {
            self.max_inner_msg_size + size_of::<AnyAllAppendix>()
        } else {
            self.max_inner_msg_size
        };

        if body.len() > max_inner_msg_size {
            if hdr.dst.port_id == 255 {
                error!("The fragmentation sink does not support the fragmentation of broadcasts");
                return Err(());
            }
            let mut wgr = self
                .prod
                .grant(self.mtu() + Self::MAX_HEADER_SIZE as u16)
                .map_err(drop)?;
            let mut serializer = Serializer {
                output: Slice::new(&mut wgr),
            };
            let footer = FragmentationHeader {
                dst: hdr.dst,
                max_inner_size: self.max_inner_msg_size as u16,
            };
            footer.serialize(&mut serializer).map_err(drop)?;
            encode_frame_hdr(&mut serializer, hdr).map_err(drop)?;
            serializer.output.try_extend(body).map_err(drop)?;
            let len = serializer.output.finalize().map_err(drop)?.len();
            wgr.commit(len as u16);
            Ok(())
        } else {
            self.inner_sink.send_raw(hdr, body)
        }
    }
}

pub struct FragmentationSinkBuilder<
    S,
    Q,
    H,
    const MTU: usize,
    const N: usize,
    const BAR_SIZE: usize,
    const REGISTRY_SIZE: usize,
> where
    S: InterfaceSink,
    Q: BbqHandle,
    H: FragmentationIssueHandler,
{
    inner_sink: Option<S>,
    queue_handle: Option<Q>,
    handler: H,
    any_handling: bool,
}

impl<S, Q, H, const MTU: usize, const N: usize, const BAR_SIZE: usize, const REGISTRY_SIZE: usize>
    FragmentationSinkBuilder<S, Q, H, MTU, N, BAR_SIZE, REGISTRY_SIZE>
where
    S: InterfaceSink,
    Q: BbqHandle,
    H: FragmentationIssueHandler,
{
    /// Creates a new [`FragmentationSinkBuilder`] to create a fragmentation sink.
    ///
    /// Receives a [`FragmentationIssueHandler`]. If it's not used [`DefaultFragmentationIssueHandler`] can be passed
    pub fn new(handler: H) -> Self {
        Self {
            queue_handle: None,
            inner_sink: None,
            handler,
            any_handling: true,
        }
    }

    /// The queue to store the messages in that get fragmented by the fragmentation service
    pub fn with_bbqueue(&mut self, queue: Option<Q>) -> &mut Self {
        self.queue_handle = queue;
        self
    }

    /// The inner sink the fragmentation sink should wrap
    pub fn with_sink(&mut self, sink: S) -> &mut Self {
        self.inner_sink = Some(sink);
        self
    }

    /// If the fragmentation sink should take the Any header appendix into account when checking if a message can fit into the underlying sink
    ///
    /// Needs to be set to false in case the inner sink does any special encoding of the Any header appendix
    pub fn with_any_handling(&mut self, handle_any_all: bool) -> &mut Self {
        self.any_handling = handle_any_all;
        self
    }

    /// Creates the fragmentation sink as well as a fragmentation config to pass to the fragmented_message_handler
    pub fn generate(
        self,
    ) -> (
        Sink<S, Q>,
        FragmentationConfig<Q, H, MTU, N, BAR_SIZE, REGISTRY_SIZE>,
    ) {
        let FragmentationSinkBuilder {
            inner_sink,
            queue_handle,
            handler,
            any_handling,
        } = self;

        let () = assert!(inner_sink.is_some(), "An inner Sink must be provided");
        let () = assert!(queue_handle.is_some(), "A queue handle must be provided");

        let inner_sink = inner_sink.expect("An inner Sink must be provided");
        let queue_handle = queue_handle.expect("A queue handle must be provided");
        let max_inner_msg_size = size_of::<FragmentPacket<BAR_SIZE>>();

        assert!(
            size_of::<FragmentPacket<0>>() < inner_sink.mtu() as usize,
            "The overhead of the fragmentation messages is larger as the MTU of the inner sink"
        );

        (
            Sink {
                inner_sink,
                prod: queue_handle.framed_producer(),
                max_inner_msg_size,
                mtu: MTU as u16,
                any_handling,
            },
            FragmentationConfig {
                cons: queue_handle.framed_consumer(),
                handler,
                receive_buffer: [FragmentationBuffer {
                    complete_size: 0,
                    packet_size: 0,
                    received_size: 0,
                    reserved: false,
                    buffer: [0; MTU],
                    registry: [0; REGISTRY_SIZE],
                }; N],
            },
        )
    }
}

/// The part of the fragmentation service that handles all incomming messages (fragmentation requests and fragment packets)
pub(crate) async fn handle_incomming_fragmentation_packets<
    Q,
    H,
    NS,
    const D: usize,
    const MTU: usize,
    const N: usize,
    const BAR_SIZE: usize,
    const REGISTRY_SIZE: usize,
>(
    ns_handle: NS,
    mut receive_buffers: [FragmentationBuffer<MTU, REGISTRY_SIZE>; N],
    ident: <<NS as NetStackHandle>::Profile as Profile>::InterfaceIdent,
    handler: H,
) where
    Q: BbqHandle,
    Q::Notifier: AsyncNotifier,
    NS: NetStackHandle,
    H: FragmentationIssueHandler,
{
    let stack = ns_handle.stack();
    let endpoints = stack.endpoints();

    let request = endpoints
        .clone()
        .bounded_server::<ErgotFragmentRequestEndpoint, D>(None);
    let request = pin!(request);
    let mut request_svr = request.attach();

    let packet = endpoints
        .clone()
        .bounded_server::<ErgotFragmentPacketEndpoint<BAR_SIZE>, D>(None);
    let packet = pin!(packet);
    let mut packet_svr = packet.attach();
    let prt = packet_svr.port();

    loop {
        let res =
            embassy_futures::select::select(request_svr.recv_manual(), packet_svr.recv_manual())
                .await;
        match res {
            Either::First(req_res) => {
                let Ok(msg) = req_res else {
                    continue;
                };
                let resp = if msg.t.complete_size > MTU as u16 {
                    Err(FragmentRequestError::MsgTooBig)
                } else {
                    let mut index = usize::MAX;
                    for (idx, receive_buffer) in receive_buffers.iter_mut().enumerate() {
                        if !receive_buffer.reserved {
                            receive_buffer.reserved = true;
                            receive_buffer.complete_size = msg.t.complete_size;
                            receive_buffer.packet_size = msg.t.packet_data_size;
                            receive_buffer.received_size = 0;
                            index = idx;
                            break;
                        }
                    }
                    if index != usize::MAX {
                        Ok(FragmentRequestResponse {
                            buffer_id: index as u8,
                            port: prt,
                        })
                    } else {
                        Err(FragmentRequestError::NoFreeSlot)
                    }
                };
                trace!(
                    "Received a fragment request {:?}, answering with {:?}",
                    msg, resp
                );
                _ = endpoints
                    .clone()
                    .respond_owned::<ErgotFragmentRequestEndpoint>(&msg.hdr, &resp);
            }
            Either::Second(pack_res) => {
                let Ok(msg) = pack_res else {
                    continue;
                };
                let resp = if msg.t.buffer_id >= N as u8 {
                    Err(FragmentPacketError::UnknownSlotId)
                } else if !receive_buffers[msg.t.buffer_id as usize].reserved {
                    Err(FragmentPacketError::SlotUnprepared)
                } else {
                    let buffer_info = &mut receive_buffers[msg.t.buffer_id as usize];
                    let addr = msg.t.packet_idx * buffer_info.packet_size;
                    let registry_idx = msg.t.packet_idx / 8;
                    let bit_idx = msg.t.packet_idx % 8;
                    let byte: u8 = 1 << bit_idx;
                    if addr > buffer_info.complete_size {
                        Err(FragmentPacketError::IndexTooLarge)
                    } else if buffer_info.registry[registry_idx as usize] & byte == 1 {
                        // We already received that package, so we just ignore it and move on
                        warn!(
                            "Fragmentation packet with index {} was received already and will be ignored",
                            msg.t.packet_idx
                        );
                        Ok(())
                    } else {
                        // Make sure that we don't copy over the end of the buffer
                        let len = u16::min(BAR_SIZE as u16, buffer_info.complete_size - addr);
                        buffer_info.buffer[(addr as usize)..(addr + len) as usize]
                            .copy_from_slice(&msg.t.data[..(len as usize)]);
                        // Set the bit in the registry so we can make sure that we don't process a package twice
                        buffer_info.registry[registry_idx as usize] |= byte;

                        buffer_info.received_size += len;
                        if buffer_info.received_size == buffer_info.complete_size {
                            let frame =
                                de_frame(&buffer_info.buffer[..buffer_info.complete_size as usize]);
                            if let Some(frame) = frame
                                && frame.body.is_ok()
                            {
                                let res = ns_handle.stack().send_raw(
                                    &frame.hdr,
                                    frame.body.expect(
                                        "We already checked that the frame body is not an error",
                                    ),
                                    ident.clone(),
                                );
                                if let Err(e) = res {
                                    error!("Fragmentation packet handler error: {:?}", e);
                                    handler.handle_error(FragmentError::NetStack(e));
                                }
                            } else {
                                handler.handle_error(FragmentError::DeserPacket);
                            }
                            buffer_info.reserved = false;
                            // Reset the registry
                            buffer_info.registry.fill(0);
                        }

                        Ok(())
                    }
                };
                let res = endpoints
                    .clone()
                    .respond_owned::<ErgotFragmentPacketEndpoint<BAR_SIZE>>(&msg.hdr, &resp);
                if let Err(e) = res {
                    handler.handle_error(FragmentError::NetStack(e));
                }
            }
        }
    }
}

/// The part of the fragmentation service that handles all outgoing messages (requesting a buffer and sending fragment packets)
pub(crate) async fn handle_outgoing_fragmentation_packets<
    Q,
    H,
    NS,
    const D: usize,
    const MTU: usize,
    const BAR_SIZE: usize,
>(
    ns_handle: NS,
    cons: FramedConsumer<Q>,
    handler: H,
) where
    Q: BbqHandle,
    Q::Notifier: AsyncNotifier,
    NS: NetStackHandle,
    H: FragmentationIssueHandler,
{
    let stack = ns_handle.stack();
    let endpoints = stack.endpoints();

    'outer: loop {
        let grant = cons.wait_read().await;
        let result = postcard::take_from_bytes::<FragmentationHeader>(&grant);
        let Ok((header, rem)) = result else {
            // Something went wrong when reading the header, we're dropping the packet
            error!("Could not read fragmentation header, dropping packet");
            handler.handle_error(FragmentError::ParseHeader(
                result.expect_err("We already checked that the result is an error"),
            ));
            grant.release();
            continue;
        };
        let complete_size = rem.len() as u16;
        let packet_data_size: u16 = BAR_SIZE as u16;

        let req: FragmentRequest = FragmentRequest {
            complete_size,
            packet_data_size,
        };

        // We don't know which port the `ErgotFragmentRequestEndpoint` has on the other side so we set the port to zero
        let mut dst = header.dst;
        dst.port_id = 0;

        let response: Result<FragmentRequestResponse, FragmentError> = {
            'buffer_grant: loop {
                let ans = endpoints
                    .clone()
                    .request::<ErgotFragmentRequestEndpoint>(dst, &req, None)
                    .await;
                if let Ok(res) = ans {
                    match res {
                        Ok(resp) => break Ok(resp),
                        Err(e) => {
                            let handle_as =
                                handler.handle_issue(FragmentError::Request(e), dst).await;
                            match handle_as {
                                FragmentationIssueResolution::Drop => continue 'outer, // Could not get a buffer_id to send to, so we're dropping the message
                                FragmentationIssueResolution::Retry => continue 'buffer_grant, // Retry
                                FragmentationIssueResolution::RaiseError => {
                                    break Err(FragmentError::HandlerRaised);
                                }
                            }
                        }
                    }
                } else {
                    break Err(FragmentError::Transport(
                        ans.expect_err("We already tested that the value is an error"),
                    ));
                }
            }
        };

        let Ok(FragmentRequestResponse { buffer_id, port }) = response else {
            handler.handle_error(
                response.expect_err("We already checked if the response is an error"),
            );
            grant.release();
            continue;
        };

        // We receive the port id so we don't send packets with any/all enabled, which would allow us to encode more data bytes per packet (see the comment in send_ty)
        dst.port_id = port;

        let packet_amount = complete_size.div_ceil(packet_data_size);

        let mut buf: [u8; BAR_SIZE] = [0; BAR_SIZE];

        for idx in 0..packet_amount {
            let start = (idx * packet_data_size) as usize;
            let len = usize::min(packet_data_size as usize, rem.len() - start);
            let end = start + len;
            buf[..len].copy_from_slice(&rem[start..end]);
            let req = FragmentPacket::<BAR_SIZE> {
                buffer_id,
                packet_idx: idx,
                data: buf,
            };
            let res = endpoints
                .clone()
                .request::<ErgotFragmentPacketEndpoint<BAR_SIZE>>(dst, &req, None)
                .await;
            if let Err(e) = res {
                let action = handler.handle_issue(FragmentError::Transport(e), dst).await;
                match action {
                    FragmentationIssueResolution::Drop => break,
                    FragmentationIssueResolution::RaiseError => {
                        handler.handle_error(FragmentError::HandlerRaised);
                        break;
                    }
                    FragmentationIssueResolution::Retry => continue,
                }
            }
        }
        grant.release();
    }
}

/// Calculates the `BAR_SIZE` constant
///
/// `BAR_SIZE` being the max size the Byte ARray in a [`FragmentPacket`] might have based on the underlying sink
pub const fn calc_barray_size<I: Interface, Q: BbqHandle, const MTU: usize>() -> usize
where
    I::Sink: InterfaceSink,
{
    MTU - I::Sink::MAX_HEADER_SIZE
        - <FragmentationSinkInterface<I, Q> as Interface>::Sink::MAX_HEADER_SIZE
}

/// Calculates the `REGISTRY_SIZE` constant
///
/// `REGISTRY_SIZE` being the number of [`FragmentPacket`]s that are needed to fully send a message of `MTU` size
pub const fn calc_registry_size<const MTU: usize, const BAR_SIZE: usize>() -> usize {
    MTU.div_ceil(BAR_SIZE).div_ceil(8)
}
