#![no_std]
#![allow(async_fn_in_trait)]

use core::marker::PhantomData;

use bbq2::{
    prod_cons::framed::{FramedConsumer, FramedProducer},
    queue::BBQueue,
    traits::{
        bbqhdl::BbqHandle,
        notifier::{maitake::MaiNotSpsc, AsyncNotifier},
        storage::Inline,
    },
};
use defmt::{debug, warn};
use embassy_futures::yield_now;
use embassy_rp::uart::{self, UartRx, UartTx};
use ergot::{
    interface_manager::{
        profiles::direct_edge::{DirectEdge, CENTRAL_NODE_ID, EDGE_NODE_ID},
        utils::framed_stream,
        Interface, InterfaceSendError, InterfaceState, Profile,
    },
    net_stack::NetStackHandle,
    wire_frames::de_frame,
    Header, NetStack, ProtocolError,
};
use mutex::ScopedRawMutex;
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

pub struct IdleUartInterface<Q: BbqHandle + 'static> {
    _pd: PhantomData<Q>,
}
pub type Queue<const N: usize, C> = BBQueue<Inline<N>, C, MaiNotSpsc>;
pub type Consumer<const N: usize, C> = FramedConsumer<&'static Queue<N, C>, u16>;
pub type IdleUartSink<Q> = framed_stream::Sink<Q>;

impl<Q: BbqHandle + 'static> Interface for IdleUartInterface<Q> {
    type Sink = IdleUartSink<Q>;
}

pub struct PairedUartProfile<Q: BbqHandle + 'static> {
    inner: DirectEdge<IdleUartInterface<Q>>,
}

impl<Q: BbqHandle + 'static> PairedUartProfile<Q> {
    pub const fn new_controller_stack<R: ScopedRawMutex + mutex::ConstInit>(
        producer: FramedProducer<Q, u16>,
        mtu: u16,
    ) -> NetStack<R, Self> {
        NetStack::new_with_profile(Self {
            inner: DirectEdge::new_controller(
                framed_stream::Sink::new(producer, mtu),
                InterfaceState::Down,
            ),
        })
    }

    pub const fn new_target_stack<R: ScopedRawMutex + mutex::ConstInit>(
        producer: FramedProducer<Q, u16>,
        mtu: u16,
    ) -> NetStack<R, Self> {
        NetStack::new_with_profile(Self {
            inner: DirectEdge::new_target(framed_stream::Sink::new(producer, mtu)),
        })
    }
}

impl<Q: BbqHandle + 'static> Profile for PairedUartProfile<Q> {
    type InterfaceIdent = ();

    fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError> {
        self.inner.send(hdr, data)
    }

    fn send_err(
        &mut self,
        hdr: &Header,
        err: ProtocolError,
        source: Option<Self::InterfaceIdent>,
    ) -> Result<(), InterfaceSendError> {
        self.inner.send_err(hdr, err, source)
    }

    fn send_raw(
        &mut self,
        hdr: &Header,
        hdr_raw: &[u8],
        data: &[u8],
        source: Self::InterfaceIdent,
    ) -> Result<(), InterfaceSendError> {
        self.inner.send_raw(hdr, hdr_raw, data, source)
    }

    fn interface_state(&mut self, ident: Self::InterfaceIdent) -> Option<InterfaceState> {
        self.inner.interface_state(ident)
    }

    fn set_interface_state(
        &mut self,
        ident: Self::InterfaceIdent,
        state: InterfaceState,
    ) -> Result<(), ergot::interface_manager::SetStateError> {
        self.inner.set_interface_state(ident, state)
    }
}

pub struct TxWorker<Q, N, T>
where
    N: NetStackHandle<Profile = PairedUartProfile<Q>>,
    Q: BbqHandle + 'static,
    T: TxIdle,
{
    nsh: N,
    rx: FramedConsumer<Q, u16>,
    tx: T,
}

pub struct RxWorker<'a, Q, N, R>
where
    N: NetStackHandle<Profile = PairedUartProfile<Q>>,
    Q: BbqHandle + 'static,
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
    N: NetStackHandle<Profile = PairedUartProfile<Q>>,
    Q: BbqHandle + 'static,
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
                nsh.stack().manage_profile(|im| {
                    _ = im.set_interface_state(
                        (),
                        InterfaceState::Active {
                            net_id: frame.hdr.dst.network_id,
                            node_id: if *is_controller {
                                CENTRAL_NODE_ID
                            } else {
                                EDGE_NODE_ID
                            },
                        },
                    );
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
                Ok(body) => nsh.stack().send_raw(&hdr, frame.hdr_raw, body, ()),
                Err(e) => nsh.stack().send_err(&hdr, e, Some(())),
            };
            match res {
                Ok(()) => {}
                Err(e) => {
                    // TODO: match on error, potentially try to send NAK?
                    warn!("send error: {}", e);
                }
            }
        }
    }
}

impl<Q, N, T> TxWorker<Q, N, T>
where
    N: NetStackHandle<Profile = PairedUartProfile<Q>>,
    Q: BbqHandle + 'static,
    Q::Notifier: AsyncNotifier,
    T: TxIdle,
{
    pub fn new_controller(net: N, q: Q, tx: T) -> Result<Self, T> {
        let res = net.stack().manage_profile(|mgr| {
            mgr.set_interface_state(
                (),
                InterfaceState::Active {
                    net_id: 1,
                    node_id: CENTRAL_NODE_ID,
                },
            )
        });

        if res.is_ok() {
            Ok(Self {
                nsh: net,
                rx: q.framed_consumer(),
                tx,
            })
        } else {
            Err(tx)
        }
    }

    pub fn new_target(net: N, q: Q, tx: T) -> Result<Self, T> {
        let res = net
            .stack()
            .manage_profile(|mgr| mgr.set_interface_state((), InterfaceState::Inactive));

        if res.is_ok() {
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
    N: NetStackHandle<Profile = PairedUartProfile<Q>>,
    Q: BbqHandle + 'static,
    T: TxIdle,
{
    fn drop(&mut self) {
        _ = self
            .nsh
            .stack()
            .manage_profile(|mgr| mgr.set_interface_state((), InterfaceState::Down));
    }
}
