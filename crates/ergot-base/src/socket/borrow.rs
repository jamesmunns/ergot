//! "Borrow" sockets
//!
//! Borrow sockets use a `bbq2` queue to store the serialized form of messages.
//!
//! This allows for sending and receiving borrowed types like `&str` or `&[u8]`,
//! or messages that contain borrowed types. This is achieved by serializing
//! messages into the bbq2 ring buffer when inserting into the socket, and
//! deserializing when removing from the socket.
//!
//! Although you can use borrowed sockets for types that are fully owned, e.g.
//! `T: 'static`, you should prefer the [`owned`](crate::socket::owned) socket
//! variants when possible, as they store messages more efficiently and may be
//! able to fully skip a ser/de round trip when sending messages locally.

use core::{
    any::TypeId,
    cell::UnsafeCell,
    marker::PhantomData,
    ops::Deref,
    pin::Pin,
    ptr::{NonNull, addr_of},
    task::{Context, Poll, Waker},
};

use bbq2::{
    prod_cons::framed::{FramedConsumer, FramedGrantR},
    traits::bbqhdl::BbqHandle,
};
use cordyceps::list::Links;
use mutex::ScopedRawMutex;
use postcard::ser_flavors;
use serde::{Deserialize, Serialize};

use crate::{
    HeaderSeq, Key, NetStack, ProtocolError,
    interface_manager::{
        BorrowedFrame, InterfaceManager,
        wire_frames::{self, CommonHeader, de_frame},
    },
};

use super::{Attributes, HeaderMessage, Response, SocketHeader, SocketSendError, SocketVTable};

struct QueueBox<Q: BbqHandle> {
    q: Q,
    waker: Option<Waker>,
}

#[repr(C)]
pub struct Socket<Q, T, R, M>
where
    Q: BbqHandle,
    T: Serialize + Clone,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    // LOAD BEARING: must be first
    hdr: SocketHeader,
    pub(crate) net: &'static NetStack<R, M>,
    inner: UnsafeCell<QueueBox<Q>>,
    mtu: u16,
    _pd: PhantomData<fn() -> T>,
}

pub struct SocketHdl<'a, Q, T, R, M>
where
    Q: BbqHandle,
    T: Serialize + Clone,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub(crate) ptr: NonNull<Socket<Q, T, R, M>>,
    _lt: PhantomData<Pin<&'a mut Socket<Q, T, R, M>>>,
    port: u8,
}

pub struct Recv<'a, 'b, Q, T, R, M>
where
    Q: BbqHandle,
    T: Serialize + Clone,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    hdl: &'a mut SocketHdl<'b, Q, T, R, M>,
}

// ---- impls ----

// impl Socket

impl<Q, T, R, M> Socket<Q, T, R, M>
where
    Q: BbqHandle,
    T: Serialize + Clone,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub const fn new(
        net: &'static NetStack<R, M>,
        key: Key,
        attrs: Attributes,
        sto: Q,
        mtu: u16,
    ) -> Self {
        Self {
            hdr: SocketHeader {
                links: Links::new(),
                vtable: const { &Self::vtable() },
                port: 0,
                attrs,
                key,
            },
            inner: UnsafeCell::new(QueueBox {
                q: sto,
                waker: None,
            }),
            net,
            _pd: PhantomData,
            mtu,
        }
    }

    pub fn attach<'a>(self: Pin<&'a mut Self>) -> SocketHdl<'a, Q, T, R, M> {
        let stack = self.net;
        let ptr_self: NonNull<Self> = NonNull::from(unsafe { self.get_unchecked_mut() });
        let ptr_erase: NonNull<SocketHeader> = ptr_self.cast();
        let port = unsafe { stack.attach_socket(ptr_erase) };
        SocketHdl {
            ptr: ptr_self,
            _lt: PhantomData,
            port,
        }
    }

    pub fn attach_broadcast<'a>(self: Pin<&'a mut Self>) -> SocketHdl<'a, Q, T, R, M> {
        let stack = self.net;
        let ptr_self: NonNull<Self> = NonNull::from(unsafe { self.get_unchecked_mut() });
        let ptr_erase: NonNull<SocketHeader> = ptr_self.cast();
        unsafe { stack.attach_broadcast_socket(ptr_erase) };
        SocketHdl {
            ptr: ptr_self,
            _lt: PhantomData,
            port: 255,
        }
    }

    const fn vtable() -> SocketVTable {
        SocketVTable {
            recv_owned: Some(Self::recv_owned),
            recv_bor: Some(Self::recv_bor),
            recv_raw: Self::recv_raw,
            recv_err: Some(Self::recv_err),
        }
    }

    pub fn stack(&self) -> &'static NetStack<R, M> {
        self.net
    }

    fn recv_err(this: NonNull<()>, hdr: HeaderSeq, err: ProtocolError) {
        let this: NonNull<Self> = this.cast();
        let this: &Self = unsafe { this.as_ref() };
        let qbox: &mut QueueBox<Q> = unsafe { &mut *this.inner.get() };
        let qref = qbox.q.bbq_ref();
        let prod = qref.framed_producer();

        // TODO: we could probably use a smaller grant here than the MTU,
        // allowing more grants to succeed.
        let Ok(mut wgr) = prod.grant(this.mtu) else {
            return;
        };

        let ser = ser_flavors::Slice::new(&mut wgr);

        let chdr = CommonHeader {
            src: hdr.src.as_u32(),
            dst: hdr.dst.as_u32(),
            seq_no: hdr.seq_no,
            kind: hdr.kind.0,
            ttl: hdr.ttl,
        };

        if let Ok(used) = wire_frames::encode_frame_err(ser, &chdr, err) {
            let len = used.len() as u16;
            wgr.commit(len);
            if let Some(wake) = qbox.waker.take() {
                wake.wake();
            }
        }
    }

    fn recv_owned(
        this: NonNull<()>,
        that: NonNull<()>,
        hdr: HeaderSeq,
        // We can't use TypeId here because mismatched lifetimes have different
        // type ids!
        _ty: &TypeId,
    ) -> Result<(), SocketSendError> {
        let that: NonNull<T> = that.cast();
        let that: &T = unsafe { that.as_ref() };
        let this: NonNull<Self> = this.cast();
        let this: &Self = unsafe { this.as_ref() };
        let qbox: &mut QueueBox<Q> = unsafe { &mut *this.inner.get() };
        let qref = qbox.q.bbq_ref();
        let prod = qref.framed_producer();

        let Ok(mut wgr) = prod.grant(this.mtu) else {
            return Err(SocketSendError::NoSpace);
        };
        let ser = ser_flavors::Slice::new(&mut wgr);

        let chdr = CommonHeader {
            src: hdr.src.as_u32(),
            dst: hdr.dst.as_u32(),
            seq_no: hdr.seq_no,
            kind: hdr.kind.0,
            ttl: hdr.ttl,
        };

        let Ok(used) = wire_frames::encode_frame_ty(ser, &chdr, hdr.key.as_ref(), that) else {
            return Err(SocketSendError::NoSpace);
        };

        let len = used.len() as u16;
        wgr.commit(len);

        if let Some(wake) = qbox.waker.take() {
            wake.wake();
        }

        Ok(())
    }

    fn recv_bor(
        this: NonNull<()>,
        that: NonNull<()>,
        hdr: HeaderSeq,
    ) -> Result<(), SocketSendError> {
        let this: NonNull<Self> = this.cast();
        let this: &Self = unsafe { this.as_ref() };
        let that: NonNull<T> = that.cast();
        let that: &T = unsafe { that.as_ref() };
        let qbox: &mut QueueBox<Q> = unsafe { &mut *this.inner.get() };
        let qref = qbox.q.bbq_ref();
        let prod = qref.framed_producer();

        let Ok(mut wgr) = prod.grant(this.mtu) else {
            return Err(SocketSendError::NoSpace);
        };
        let ser = ser_flavors::Slice::new(&mut wgr);

        let chdr = CommonHeader {
            src: hdr.src.as_u32(),
            dst: hdr.dst.as_u32(),
            seq_no: hdr.seq_no,
            kind: hdr.kind.0,
            ttl: hdr.ttl,
        };

        let Ok(used) = wire_frames::encode_frame_ty(ser, &chdr, hdr.key.as_ref(), that) else {
            return Err(SocketSendError::NoSpace);
        };

        let len = used.len() as u16;
        wgr.commit(len);

        if let Some(wake) = qbox.waker.take() {
            wake.wake();
        }

        Ok(())
    }

    fn recv_raw(
        this: NonNull<()>,
        that: &[u8],
        _hdr: HeaderSeq,
        hdr_raw: &[u8],
    ) -> Result<(), SocketSendError> {
        let this: NonNull<Self> = this.cast();
        let this: &Self = unsafe { this.as_ref() };
        let qbox: &mut QueueBox<Q> = unsafe { &mut *this.inner.get() };
        let qref = qbox.q.bbq_ref();
        let prod = qref.framed_producer();

        let Ok(needed) = u16::try_from(that.len() + hdr_raw.len()) else {
            return Err(SocketSendError::NoSpace);
        };

        let Ok(mut wgr) = prod.grant(needed) else {
            return Err(SocketSendError::NoSpace);
        };
        let (hdr, body) = wgr.split_at_mut(hdr_raw.len());
        hdr.copy_from_slice(hdr_raw);
        body.copy_from_slice(that);
        wgr.commit(needed);

        if let Some(wake) = qbox.waker.take() {
            wake.wake();
        }

        Ok(())
    }
}

// impl SocketHdl

// TODO: impl drop, remove waker, remove socket
impl<'a, Q, T, R, M> SocketHdl<'a, Q, T, R, M>
where
    Q: BbqHandle,
    T: Serialize + Clone,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub fn port(&self) -> u8 {
        self.port
    }

    pub fn stack(&self) -> &'static NetStack<R, M> {
        unsafe { *addr_of!((*self.ptr.as_ptr()).net) }
    }

    pub fn recv<'b>(&'b mut self) -> Recv<'b, 'a, Q, T, R, M> {
        Recv { hdl: self }
    }
}

impl<Q, T, R, M> Drop for Socket<Q, T, R, M>
where
    Q: BbqHandle,
    T: Serialize + Clone,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    fn drop(&mut self) {
        unsafe {
            let this = NonNull::from(&self.hdr);
            self.net.detach_socket(this);
        }
    }
}

unsafe impl<Q, T, R, M> Send for SocketHdl<'_, Q, T, R, M>
where
    Q: BbqHandle,
    T: Serialize + Clone,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
}

unsafe impl<Q, T, R, M> Sync for SocketHdl<'_, Q, T, R, M>
where
    Q: BbqHandle,
    T: Serialize + Clone,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
}

// impl Recv

enum ResponseGrantInner<Q: BbqHandle, T> {
    Ok {
        grant: FramedGrantR<Q, u16>,
        offset: usize,
        deser_erased: PhantomData<fn() -> T>,
    },
    Err(ProtocolError),
}

pub struct ResponseGrant<Q: BbqHandle, T> {
    pub hdr: HeaderSeq,
    inner: ResponseGrantInner<Q, T>,
}

impl<Q: BbqHandle, T> Drop for ResponseGrant<Q, T> {
    fn drop(&mut self) {
        let old = core::mem::replace(
            &mut self.inner,
            ResponseGrantInner::Err(ProtocolError(u16::MAX)),
        );
        match old {
            ResponseGrantInner::Ok { grant, .. } => {
                grant.release();
            }
            ResponseGrantInner::Err(_) => {}
        }
    }
}

impl<Q: BbqHandle, T> ResponseGrant<Q, T> {
    // TODO: I don't want this being failable, but right now I can't figure out
    // how to make Recv::poll() do the checking without hitting awkward inner
    // lifetimes for deserialization. If you know how to make this less awkward,
    // please @ me somewhere about it.
    pub fn try_access<'de, 'me: 'de>(&'me self) -> Option<Response<T>>
    where
        T: Deserialize<'de>,
    {
        Some(match &self.inner {
            ResponseGrantInner::Ok {
                grant,
                deser_erased: _,
                offset,
            } => {
                // TODO: We could use something like Yoke to skip repeating deser
                let t = postcard::from_bytes::<T>(grant.get(*offset..)?).ok()?;
                Response::Ok(HeaderMessage {
                    hdr: self.hdr.clone(),
                    t,
                })
            }
            ResponseGrantInner::Err(protocol_error) => Response::Err(HeaderMessage {
                hdr: self.hdr.clone(),
                t: *protocol_error,
            }),
        })
    }
}

impl<'a, Q, T, R, M> Future for Recv<'a, '_, Q, T, R, M>
where
    Q: BbqHandle,
    T: Serialize + Clone,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    type Output = ResponseGrant<Q, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let net: &'static NetStack<R, M> = self.hdl.stack();
        let f = || -> Option<ResponseGrant<Q, T>> {
            let this_ref: &Socket<Q, T, R, M> = unsafe { self.hdl.ptr.as_ref() };
            let qbox: &mut QueueBox<Q> = unsafe { &mut *this_ref.inner.get() };
            let cons: FramedConsumer<Q, u16> = qbox.q.framed_consumer();

            if let Ok(resp) = cons.read() {
                let sli: &[u8] = resp.deref();

                if let Some(frame) = de_frame(sli) {
                    let BorrowedFrame {
                        hdr,
                        body,
                        hdr_raw: _,
                    } = frame;
                    match body {
                        Ok(body) => {
                            let sli: &[u8] = body;
                            // I want to be able to do something like this:
                            //
                            // if let Ok(_msg) = postcard::from_bytes::<T>(sli) {
                            //     let offset =
                            //         (sli.as_ptr() as usize) - (resp.deref().as_ptr() as usize);
                            //     return Some(ResponseGrant {
                            //         hdr,
                            //         inner: ResponseGrantInner::Ok {
                            //             grant: resp,
                            //             offset,
                            //             deser_erased: PhantomData,
                            //         },
                            //         _plt: PhantomData,
                            //     });
                            // } else {
                            //     resp.release();
                            // }
                            let offset = (sli.as_ptr() as usize) - (resp.deref().as_ptr() as usize);
                            return Some(ResponseGrant {
                                hdr,
                                inner: ResponseGrantInner::Ok {
                                    grant: resp,
                                    offset,
                                    deser_erased: PhantomData,
                                },
                            });
                        }
                        Err(err) => {
                            resp.release();
                            return Some(ResponseGrant {
                                hdr,
                                inner: ResponseGrantInner::Err(err),
                            });
                        }
                    }
                }
            }

            let new_wake = cx.waker();
            if let Some(w) = qbox.waker.take() {
                if !w.will_wake(new_wake) {
                    w.wake();
                }
            }
            // NOTE: Okay to register waker AFTER checking, because we
            // have an exclusive lock
            qbox.waker = Some(new_wake.clone());
            None
        };
        let res = unsafe { net.with_lock(f) };
        if let Some(t) = res {
            Poll::Ready(t)
        } else {
            Poll::Pending
        }
    }
}

unsafe impl<Q, T, R, M> Sync for Recv<'_, '_, Q, T, R, M>
where
    Q: BbqHandle,
    T: Serialize + Clone,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
}
