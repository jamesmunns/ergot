#![no_std]
#![allow(async_fn_in_trait)]

use bbq2::{
    prod_cons::framed::FramedConsumer,
    traits::{bbqhdl::BbqHandle, notifier::AsyncNotifier},
};
use defmt::{debug, trace, warn};
use embassy_futures::yield_now;
use embassy_rp::uart::{self, UartRx, UartTx};
use ergot::{
    ergot_base::{
        net_stack::NetStackHandle,
        wire_frames::{de_frame, CommonHeader},
        Header, NetStackSendError, ProtocolError,
    },
    interface_manager::{
        framed_stream::Interface, ConstInit, InterfaceManager, InterfaceSendError,
    },
};
use serde::Serialize;

/// A transmitter that guarantees that at least MIN_IDLE_TIME has passed between
/// finishing one call of `send_all` and the start of sending data on the next
/// call to `send_all`. Implementers should remember the time of the last end
/// of tx, and wait at least MIN_IDLE_TIME before starting the next send.
pub trait TxIdle {
    type Error;
    async fn send_all(&mut self, data: &[u8]) -> Result<(), Self::Error>;
}

// Note: this uses "serial breaks" instead of of idle, because it works
// better with the RP2040.
impl TxIdle for UartTx<'_, uart::Async> {
    type Error = uart::Error;

    async fn send_all(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.write(data).await?;
        // see https://github.com/embassy-rs/embassy/issues/2464
        while self.busy() {
            yield_now().await;
        }
        self.send_break(20).await;
        Ok(())
    }
}

// Note: this uses "serial breaks" instead of of idle, because it works
// better with the RP2040.
impl RxIdle for UartRx<'_, uart::Async> {
    type Error = uart::ReadToBreakError;

    async fn recv_until_idle<'a>(
        &mut self,
        buf: &'a mut [u8],
    ) -> Result<&'a mut [u8], Self::Error> {
        let got = self.read_to_break(buf).await?;
        Ok(&mut buf[..got])
    }
}

/// A receiver that frames messages based on the line going idle. It should wait
/// until the first byte is received, then return once either the buffer has
/// overflowed or the necessary idle time has been met.
pub trait RxIdle {
    type Error;
    async fn recv_until_idle<'a>(&mut self, buf: &'a mut [u8])
        -> Result<&'a mut [u8], Self::Error>;
}

pub struct PairedInterfaceManager<Q: BbqHandle> {
    inner: Option<PairedInterfaceManagerInner<Q>>,
}

pub struct PairedInterfaceManagerInner<Q: BbqHandle> {
    tx_q: Interface<Q>,
    net_id: u16,
    is_controller: bool,
    seq_no: u16,
}

impl<Q: BbqHandle> ConstInit for PairedInterfaceManager<Q> {
    const INIT: Self = Self { inner: None };
}

pub struct TxWorker<Q, N, T>
where
    N: NetStackHandle<Interface = PairedInterfaceManager<Q>>,
    Q: BbqHandle,
    T: TxIdle,
{
    nsh: N,
    rx: FramedConsumer<Q, u16>,
    tx: T,
}

pub struct RxWorker<'a, Q, N, R>
where
    N: NetStackHandle<Interface = PairedInterfaceManager<Q>>,
    Q: BbqHandle,
    R: RxIdle,
{
    nsh: N,
    rx: R,
    frame: &'a mut [u8],
    is_controller: bool,
    net_id: Option<u16>,
}

impl<'a, Q, N, R> RxWorker<'a, Q, N, R>
where
    N: NetStackHandle<Interface = PairedInterfaceManager<Q>>,
    Q: BbqHandle,
    Q::Notifier: AsyncNotifier,
    R: RxIdle,
{
    pub fn new_controller(net: N, rx: R, frame: &'a mut [u8]) -> Self {
        Self {
            nsh: net,
            rx,
            frame,
            is_controller: true,
            net_id: Some(1),
        }
    }

    pub fn new_target(net: N, rx: R, frame: &'a mut [u8]) -> Self {
        Self {
            nsh: net,
            rx,
            frame,
            is_controller: false,
            net_id: None,
        }
    }

    #[inline]
    pub fn own_node_id(&self) -> u8 {
        if self.is_controller {
            1
        } else {
            2
        }
    }

    pub async fn run(&mut self) -> ! {
        let own_node_id = self.own_node_id();

        let Self {
            nsh,
            rx,
            frame,
            is_controller,
            net_id,
        } = self;
        loop {
            let Ok(f) = rx.recv_until_idle(frame).await else {
                warn!("recv error");
                continue;
            };

            let Some(mut frame) = de_frame(f) else {
                warn!(
                    "Decode error! Ignoring frame on net_id {}",
                    net_id.unwrap_or(0)
                );
                continue;
            };

            debug!("Got Frame!");

            let take_net = !*is_controller
                && (net_id.is_none()
                    || net_id.is_some_and(|n| {
                        frame.hdr.dst.network_id != 0 && n != frame.hdr.dst.network_id
                    }));

            if take_net {
                nsh.stack().with_interface_manager(|im| {
                    if let Some(i) = im.inner.as_mut() {
                        // i am, whoever you say i am
                        warn!("Taking net {=u16}", frame.hdr.dst.network_id);
                        i.net_id = frame.hdr.dst.network_id;
                    } else {
                        warn!("Whatttt");
                    }
                    // else: uhhhhhh
                });
                *net_id = Some(frame.hdr.dst.network_id);
            }

            // If the message comes in and has a src net_id of zero,
            // we should rewrite it so it isn't later understood as a
            // local packet.
            //
            // TODO: accept any packet if we don't have a net_id yet?
            if let Some(net) = net_id.as_ref() {
                if frame.hdr.src.network_id == 0 {
                    assert_ne!(frame.hdr.src.node_id, 0, "we got a local packet remotely?");
                    assert_ne!(
                        frame.hdr.src.node_id, own_node_id,
                        "someone is pretending to be us?"
                    );

                    frame.hdr.src.network_id = *net;
                }
            }

            // TODO: if the destination IS self.net_id, we could rewrite the
            // dest net_id as zero to avoid a pass through the interface manager.
            //
            // If the dest is 0, should we rewrite the dest as self.net_id? This
            // is the opposite as above, but I dunno how that will work with responses
            let hdr = frame.hdr.clone();
            let hdr: Header = hdr.into();
            let res = match frame.body {
                Ok(body) => nsh.stack().send_raw(&hdr, frame.hdr_raw, body),
                Err(e) => nsh.stack().send_err(&hdr, e),
            };
            match res {
                Ok(()) => {}
                Err(e) => {
                    // TODO: match on error, potentially try to send NAK?
                    match e {
                        NetStackSendError::SocketSend(_) => {
                            warn!("SocketSend(SocketSendError");
                        }
                        NetStackSendError::InterfaceSend(_) => {
                            warn!("InterfaceSend(InterfaceSendError");
                        }
                        NetStackSendError::NoRoute => {
                            warn!("NoRoute");
                        }
                        NetStackSendError::AnyPortMissingKey => {
                            warn!("AnyPortMissingKey");
                        }
                        NetStackSendError::WrongPortKind => {
                            warn!("WrongPortKind");
                        }
                        NetStackSendError::AnyPortNotUnique => {
                            warn!("AnyPortNotUnique");
                        }
                        NetStackSendError::AllPortMissingKey => {
                            warn!("AllPortMissingKey");
                        }
                        _ => warn!("OTHER"),
                    }
                }
            }
        }
    }
}

impl<Q, N, T> TxWorker<Q, N, T>
where
    N: NetStackHandle<Interface = PairedInterfaceManager<Q>>,
    Q: BbqHandle,
    Q::Notifier: AsyncNotifier,
    T: TxIdle,
{
    pub fn new_controller(net: N, q: Q, tx: T, mtu: u16) -> Result<Self, T> {
        let interface = Interface::new(q.framed_producer(), mtu);
        let res = net.stack().with_interface_manager(|mgr| {
            if mgr.inner.is_some() {
                return false;
            }
            mgr.inner = Some(PairedInterfaceManagerInner {
                tx_q: interface,
                net_id: 1,
                is_controller: true,
                seq_no: 0,
            });
            true
        });

        if res {
            Ok(Self {
                nsh: net,
                rx: q.framed_consumer(),
                tx,
            })
        } else {
            Err(tx)
        }
    }

    pub fn new_target(net: N, q: Q, tx: T, mtu: u16) -> Result<Self, T> {
        let interface = Interface::new(q.framed_producer(), mtu);
        let res = net.stack().with_interface_manager(|mgr| {
            if mgr.inner.is_some() {
                return false;
            }
            mgr.inner = Some(PairedInterfaceManagerInner {
                tx_q: interface,
                net_id: 0,
                is_controller: false,
                seq_no: 0,
            });
            true
        });

        if res {
            Ok(Self {
                nsh: net,
                rx: q.framed_consumer(),
                tx,
            })
        } else {
            Err(tx)
        }
    }

    pub async fn run_until_err(&mut self) -> T::Error {
        loop {
            let rx = self.rx.wait_read().await;
            let res = self.tx.send_all(&rx).await;
            rx.release();
            if let Err(e) = res {
                return e;
            }
        }
    }
}

impl<Q, N, T> Drop for TxWorker<Q, N, T>
where
    N: NetStackHandle<Interface = PairedInterfaceManager<Q>>,
    Q: BbqHandle,
    T: TxIdle,
{
    fn drop(&mut self) {
        self.nsh.stack().with_interface_manager(|mgr| {
            mgr.inner.take();
        });
    }
}

impl<Q: BbqHandle> PairedInterfaceManager<Q> {
    fn common_send<'b>(
        &'b mut self,
        ihdr: &Header,
    ) -> Result<(&'b mut Interface<Q>, CommonHeader), InterfaceSendError> {
        let Some(inner) = self.inner.as_mut() else {
            return Err(InterfaceSendError::NoRouteToDest);
        };
        inner.common_send(ihdr)
    }
}

impl<Q: BbqHandle> PairedInterfaceManagerInner<Q> {
    #[inline]
    pub fn own_node_id(&self) -> u8 {
        if self.is_controller {
            1
        } else {
            2
        }
    }

    #[inline]
    pub fn other_node_id(&self) -> u8 {
        if self.is_controller {
            2
        } else {
            1
        }
    }

    fn common_send<'b>(
        &'b mut self,
        ihdr: &Header,
    ) -> Result<(&'b mut Interface<Q>, CommonHeader), InterfaceSendError> {
        trace!("common_send header: {:?}", ihdr);

        if self.net_id == 0 {
            debug!("Attempted to send via interface before we have been assigned a net ID");
            // No net_id yet, don't allow routing (todo: maybe broadcast?)
            return Err(InterfaceSendError::NoRouteToDest);
        }
        // todo: we could probably keep a routing table of some kind, but for
        // now, we treat this as a "default" route, all packets go

        // TODO: a LOT of this is copy/pasted from the router, can we make this
        // shared logic, or handled by the stack somehow?
        if ihdr.dst.network_id == self.net_id && ihdr.dst.node_id == self.own_node_id() {
            return Err(InterfaceSendError::DestinationLocal);
        }

        // Now that we've filtered out "dest local" checks, see if there is
        // any TTL left before we send to the next hop
        let mut hdr = ihdr.clone();
        hdr.decrement_ttl()?;

        // If the source is local, rewrite the source using this interface's
        // information so responses can find their way back here
        if hdr.src.net_node_any() {
            // todo: if we know the destination is EXACTLY this network,
            // we could leave the network_id local to allow for shorter
            // addresses
            hdr.src.network_id = self.net_id;
            hdr.src.node_id = self.own_node_id();
        }

        // If this is a broadcast message, update the destination, ignoring
        // whatever was there before
        if hdr.dst.port_id == 255 {
            hdr.dst.network_id = self.net_id;
            hdr.dst.node_id = self.other_node_id();
        }

        let seq_no = self.seq_no;
        self.seq_no = self.seq_no.wrapping_add(1);

        let header = CommonHeader {
            src: hdr.src,
            dst: hdr.dst,
            seq_no,
            kind: hdr.kind,
            ttl: hdr.ttl,
        };
        if [0, 255].contains(&hdr.dst.port_id) && ihdr.any_all.is_none() {
            return Err(InterfaceSendError::AnyPortMissingKey);
        }

        Ok((&mut self.tx_q, header))
    }
}

impl<Q: BbqHandle> InterfaceManager for PairedInterfaceManager<Q> {
    fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;

        let res = intfc.send_ty(&header, hdr.any_all.as_ref(), data);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_err(&mut self, hdr: &Header, err: ProtocolError) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;

        let res = intfc.send_err(&header, err);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_raw(
        &mut self,
        hdr: &Header,
        hdr_raw: &[u8],
        data: &[u8],
    ) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;

        let res = intfc.send_raw(&header, hdr_raw, data);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }
}
