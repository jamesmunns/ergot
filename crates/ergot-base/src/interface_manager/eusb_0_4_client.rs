// I need an interface manager that can have 0 or 1 interfaces
// it needs to be able to be const init'd (empty)
// at runtime we can attach the client (and maybe re-attach?)
//
// In normal setups, we'd probably want some way to "announce" we
// are here, but in point-to-point

use crate::{
    Header, Key, NetStack,
    interface_manager::{
        ConstInit, InterfaceManager, InterfaceSendError,
        framed_stream::{self, Interface},
        wire_frames::{CommonHeader, de_frame},
    },
};
use bbq2::{
    prod_cons::framed::FramedConsumer,
    queue::BBQueue,
    traits::{coordination::cas::AtomicCoord, notifier::maitake::MaiNotSpsc, storage::Inline},
};
use embassy_futures::select::{Either, select};
use embassy_time::Timer;
use embassy_usb::driver::{Driver, Endpoint, EndpointError, EndpointIn, EndpointOut};
use log::{debug, info, warn};
use mutex::ScopedRawMutex;

pub enum ReceiverError {
    ReceivedMessageTooLarge,
    ConnectionClosed,
}

pub enum TransmitError {
    ConnectionClosed,
    Timeout,
}

#[derive(Default)]
pub struct EmbassyUsbManager<const N: usize> {
    inner: Option<EmbassyUsbManagerInner<N>>,
    seq_no: u16,
}

struct EmbassyUsbManagerInner<const N: usize> {
    interface: ProducerHandle<N>,
    net_id: u16,
}

#[derive(Debug, PartialEq)]
pub enum ClientError {
    SocketAlreadyActive,
}

pub struct Receiver<R: ScopedRawMutex + 'static, D: Driver<'static>, const N: usize> {
    bbq: &'static BBQueue<Inline<N>, AtomicCoord, MaiNotSpsc>,
    stack: &'static NetStack<R, EmbassyUsbManager<N>>,
    rx: D::EndpointOut,
    net_id: Option<u16>,
}

impl<R: ScopedRawMutex + 'static, D: Driver<'static>, const N: usize> Receiver<R, D, N> {
    pub fn new(
        q: &'static BBQueue<Inline<N>, AtomicCoord, MaiNotSpsc>,
        stack: &'static NetStack<R, EmbassyUsbManager<N>>,
        rx: D::EndpointOut,
    ) -> Self {
        Self {
            bbq: q,
            stack,
            rx,
            net_id: None,
        }
    }
}

struct ProducerHandle<const N: usize> {
    skt_tx: framed_stream::Interface<&'static BBQueue<Inline<N>, AtomicCoord, MaiNotSpsc>>,
}

// ---- impls ----

impl<const N: usize> EmbassyUsbManager<N> {
    pub const fn new() -> Self {
        Self {
            inner: None,
            seq_no: 0,
        }
    }
}

impl<const N: usize> ConstInit for EmbassyUsbManager<N> {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = Self::new();
}

impl<const N: usize> EmbassyUsbManager<N> {
    fn common_send<'a, 'b>(
        &'b mut self,
        ihdr: &'a Header,
    ) -> Result<
        (
            &'b mut EmbassyUsbManagerInner<N>,
            CommonHeader,
            Option<&'a Key>,
        ),
        InterfaceSendError,
    > {
        let intfc = match self.inner.take() {
            None => return Err(InterfaceSendError::NoRouteToDest),
            // TODO: Closed flag?
            // Some(intfc) if intfc.closer.is_closed() => {
            //     drop(intfc);
            //     return Err(InterfaceSendError::NoRouteToDest);
            // }
            Some(intfc) => self.inner.insert(intfc),
        };

        if intfc.net_id == 0 {
            // No net_id yet, don't allow routing (todo: maybe broadcast?)
            return Err(InterfaceSendError::NoRouteToDest);
        }
        // todo: we could probably keep a routing table of some kind, but for
        // now, we treat this as a "default" route, all packets go

        // TODO: a LOT of this is copy/pasted from the router, can we make this
        // shared logic, or handled by the stack somehow?
        //
        // TODO: Assumption: "we" are always node_id==2
        if ihdr.dst.network_id == intfc.net_id && ihdr.dst.node_id == 2 {
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
            hdr.src.network_id = intfc.net_id;
            hdr.src.node_id = 2;
        }

        // If this is a broadcast message, update the destination, ignoring
        // whatever was there before
        if hdr.dst.port_id == 255 {
            hdr.dst.network_id = intfc.net_id;
            hdr.dst.node_id = 1;
        }

        let seq_no = self.seq_no;
        self.seq_no = self.seq_no.wrapping_add(1);

        let header = CommonHeader {
            src: hdr.src.as_u32(),
            dst: hdr.dst.as_u32(),
            seq_no,
            kind: hdr.kind.0,
            ttl: hdr.ttl,
        };
        let key = if [0, 255].contains(&hdr.dst.port_id) {
            Some(ihdr.key.as_ref().unwrap())
        } else {
            None
        };

        Ok((intfc, header, key))
    }
}

impl<const N: usize> InterfaceManager for EmbassyUsbManager<N> {
    fn send<T: serde::Serialize>(
        &mut self,
        hdr: &Header,
        data: &T,
    ) -> Result<(), InterfaceSendError> {
        let (intfc, header, key) = self.common_send(hdr)?;
        let res = intfc.interface.skt_tx.send_ty(&header, key, data);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_raw(&mut self, hdr: &Header, data: &[u8]) -> Result<(), InterfaceSendError> {
        let (intfc, header, key) = self.common_send(hdr)?;
        let res = intfc.interface.skt_tx.send_raw(&header, key, data);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_err(
        &mut self,
        hdr: &Header,
        err: crate::ProtocolError,
    ) -> Result<(), InterfaceSendError> {
        let (intfc, header, _key) = self.common_send(hdr)?;
        let res = intfc.interface.skt_tx.send_err(&header, err);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }
}

impl<R: ScopedRawMutex + 'static, D: Driver<'static>, const N: usize> Receiver<R, D, N> {
    pub async fn run(mut self, frame: &mut [u8]) {
        loop {
            self.rx.wait_enabled().await;
            self.stack.with_interface_manager(|im| {
                im.inner.replace(EmbassyUsbManagerInner {
                    interface: ProducerHandle {
                        skt_tx: Interface {
                            prod: self.bbq.framed_producer(),
                            mtu: frame.len() as u16,
                        },
                    },
                    net_id: 0,
                })
            });
            self.one_conn(frame).await;
            self.stack.with_interface_manager(|im| {
                im.inner.take();
            });
        }
    }

    pub async fn one_conn(&mut self, frame: &mut [u8]) {
        loop {
            match self.one_frame(frame).await {
                Ok(f) => {
                    self.process_frame(f);
                }
                Err(ReceiverError::ConnectionClosed) => break,
                Err(_e) => {
                    continue;
                }
            }
        }
    }

    pub fn process_frame(&mut self, data: &mut [u8]) {
        let Some(mut frame) = de_frame(data) else {
            warn!(
                "Decode error! Ignoring frame on net_id {}",
                self.net_id.unwrap_or(0)
            );
            return;
        };

        debug!("Got Frame!");

        let take_net = self.net_id.is_none()
            || self
                .net_id
                .is_some_and(|n| frame.hdr.dst.network_id != 0 && n != frame.hdr.dst.network_id);

        if take_net {
            self.stack.with_interface_manager(|im| {
                if let Some(i) = im.inner.as_mut() {
                    // i am, whoever you say i am
                    i.net_id = frame.hdr.dst.network_id;
                }
                // else: uhhhhhh
            });
            self.net_id = Some(frame.hdr.dst.network_id);
        }

        // If the message comes in and has a src net_id of zero,
        // we should rewrite it so it isn't later understood as a
        // local packet.
        //
        // TODO: accept any packet if we don't have a net_id yet?
        if let Some(net) = self.net_id.as_ref() {
            if frame.hdr.src.network_id == 0 {
                assert_ne!(frame.hdr.src.node_id, 0, "we got a local packet remotely?");
                assert_ne!(frame.hdr.src.node_id, 2, "someone is pretending to be us?");

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
            Ok(body) => self.stack.send_raw(&hdr, body),
            Err(e) => self.stack.send_err(&hdr, e),
        };
        match res {
            Ok(()) => {}
            Err(e) => {
                // TODO: match on error, potentially try to send NAK?
                panic!("recv->send error: {e:?}");
            }
        }
    }

    pub async fn one_frame<'a>(
        &mut self,
        frame: &'a mut [u8],
    ) -> Result<&'a mut [u8], ReceiverError> {
        let buflen = frame.len();
        let mut window = &mut frame[..];

        while !window.is_empty() {
            let n = match self.rx.read(window).await {
                Ok(n) => n,
                Err(EndpointError::BufferOverflow) => {
                    return Err(ReceiverError::ReceivedMessageTooLarge);
                }
                Err(EndpointError::Disabled) => return Err(ReceiverError::ConnectionClosed),
            };

            let (_now, later) = window.split_at_mut(n);
            window = later;
            if n != 64 {
                // We now have a full frame! Great!
                let wlen = window.len();
                let len = buflen - wlen;
                let frame = &mut frame[..len];

                return Ok(frame);
            }
        }

        // If we got here, we've run out of space. That's disappointing. Accumulate to the
        // end of this packet
        loop {
            match self.rx.read(frame).await {
                Ok(64) => {}
                Ok(_) => return Err(ReceiverError::ReceivedMessageTooLarge),
                Err(EndpointError::BufferOverflow) => {
                    return Err(ReceiverError::ReceivedMessageTooLarge);
                }
                Err(EndpointError::Disabled) => return Err(ReceiverError::ConnectionClosed),
            };
        }
    }
}

pub async fn tx_worker<D: Driver<'static>, const N: usize>(
    ep_in: &mut D::EndpointIn,
    rx: FramedConsumer<&'static BBQueue<Inline<N>, AtomicCoord, MaiNotSpsc>>,
    timeout_ms_per_frame: usize,
) {
    info!("Started tx_worker");
    let mut pending = false;
    loop {
        ep_in.wait_enabled().await;
        loop {
            let frame = rx.wait_read().await;
            let res = send_all::<D>(ep_in, &frame, &mut pending, timeout_ms_per_frame).await;
            frame.release();
            match res {
                Ok(()) => {}
                Err(TransmitError::Timeout) => {}
                Err(TransmitError::ConnectionClosed) => break,
            }
        }
    }
}

#[inline]
async fn send_all<D>(
    ep_in: &mut D::EndpointIn,
    out: &[u8],
    pending_frame: &mut bool,
    timeout_ms_per_frame: usize,
) -> Result<(), TransmitError>
where
    D: Driver<'static>,
{
    if out.is_empty() {
        return Ok(());
    }

    // Calculate an estimated timeout based on the number of frames we need to send
    // For now, we use 2ms/frame by default, rounded UP
    let frames = out.len().div_ceil(64);
    let timeout_ms = frames * timeout_ms_per_frame;

    let send_fut = async {
        // If we left off a pending frame, send one now so we don't leave an unterminated
        // message
        if *pending_frame && ep_in.write(&[]).await.is_err() {
            return Err(TransmitError::ConnectionClosed);
        }
        *pending_frame = true;

        // write in segments of 64. The last chunk may
        // be 0 < len <= 64.
        for ch in out.chunks(64) {
            if ep_in.write(ch).await.is_err() {
                return Err(TransmitError::ConnectionClosed);
            }
        }
        // If the total we sent was a multiple of 64, send an
        // empty message to "flush" the transaction. We already checked
        // above that the len != 0.
        if (out.len() & (64 - 1)) == 0 && ep_in.write(&[]).await.is_err() {
            return Err(TransmitError::ConnectionClosed);
        }

        *pending_frame = false;
        Ok(())
    };

    match select(send_fut, Timer::after_millis(timeout_ms as u64)).await {
        Either::First(res) => res,
        Either::Second(()) => Err(TransmitError::Timeout),
    }
}
