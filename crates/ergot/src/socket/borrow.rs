//! "Borrow" sockets
//!
//! Borrow sockets use a `bbqueue` queue to store the serialized form of messages.
//!
//! This allows for sending and receiving borrowed types like `&str` or `&[u8]`,
//! or messages that contain borrowed types. This is achieved by serializing
//! messages into the bbqueue ring buffer when inserting into the socket, and
//! deserializing when removing from the socket.
//!
//! Although you can use borrowed sockets for types that are fully owned, e.g.
//! `T: 'static`, you should prefer the [`owned`](crate::socket::owned) socket
//! variants when possible, as they store messages more efficiently and may be
//! able to fully skip a ser/de round trip when sending messages locally.

use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    ops::Deref,
    pin::Pin,
    ptr::{NonNull, addr_of, addr_of_mut},
    task::{Context, Poll, Waker},
};

use bbqueue::{
    prod_cons::framed::{FramedConsumer, FramedGrantR},
    traits::bbqhdl::BbqHandle,
};
use cordyceps::list::Links;
use postcard::{
    Serializer,
    ser_flavors::{self, Flavor, Slice},
};
use serde::{Deserialize, Serialize};

use crate::{
    HeaderSeq, Key, ProtocolError,
    nash::NameHash,
    net_stack::NetStackHandle,
    socket::{
        Attributes, BorSerFn, HeaderMessage, Response, SocketHeader, SocketSendError, SocketVTable,
    },
    wire_frames::{self, BorrowedFrame, MAX_HDR_ENCODED_SIZE, de_frame, encode_frame_hdr},
};

#[repr(C)]
pub struct Socket<Q, T, N>
where
    Q: BbqHandle,
    T: Serialize,
    N: NetStackHandle,
{
    // LOAD BEARING: must be first
    hdr: SocketHeader,
    pub(crate) net: N::Target,
    inner: UnsafeCell<QueueBox<Q>>,
    mtu: u16,
    _pd: PhantomData<fn() -> T>,
}

pub struct SocketHdl<'a, Q, T, N>
where
    Q: BbqHandle,
    T: Serialize,
    N: NetStackHandle,
{
    pub(crate) ptr: NonNull<Socket<Q, T, N>>,
    _lt: PhantomData<Pin<&'a mut Socket<Q, T, N>>>,
    port: u8,
}

pub struct Recv<'a, 'b, Q, T, N>
where
    Q: BbqHandle,
    T: Serialize,
    N: NetStackHandle,
{
    hdl: &'a mut SocketHdl<'b, Q, T, N>,
}

pub struct ResponseGrant<'a, Q: BbqHandle, T> {
    pub hdr: HeaderSeq,
    inner: ResponseGrantInner<Q, T>,
    // Ties the grant to the `&mut` borrow taken by `recv()`: a borrow socket holds
    // at most one read grant, so while this grant is alive the socket handle stays
    // mutably borrowed and cannot be `recv()`d again. Invariant in `'a` (as `&mut`
    // is), which is what we want.
    _brw: PhantomData<&'a mut ()>,
}

struct QueueBox<Q: BbqHandle> {
    q: Q,
    waker: Option<Waker>,
}

enum ResponseGrantInner<Q: BbqHandle, T> {
    Ok {
        grant: FramedGrantR<Q, u16>,
        offset: usize,
        deser_erased: PhantomData<fn() -> T>,
    },
    Err(ProtocolError),
}

// ---- impls ----

// impl Socket

impl<Q, T, N> Socket<Q, T, N>
where
    Q: BbqHandle,
    T: Serialize,
    N: NetStackHandle,
{
    pub const fn new(
        net: N::Target,
        key: Key,
        attrs: Attributes,
        sto: Q,
        mtu: u16,
        name: Option<&str>,
    ) -> Self {
        Self {
            hdr: SocketHeader {
                links: Links::new(),
                vtable: const { &Self::vtable() },
                port: 0,
                attrs,
                key,
                nash: if let Some(n) = name {
                    Some(NameHash::new(n))
                } else {
                    None
                },
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

    pub fn attach<'a>(self: Pin<&'a mut Self>) -> SocketHdl<'a, Q, T, N> {
        let stack = self.net.clone();
        let ptr_self: NonNull<Self> = NonNull::from(unsafe { self.get_unchecked_mut() });
        let ptr_erase: NonNull<SocketHeader> = ptr_self.cast();
        let port = unsafe { stack.attach_socket(ptr_erase) };
        SocketHdl {
            ptr: ptr_self,
            _lt: PhantomData,
            port,
        }
    }

    pub fn attach_broadcast<'a>(self: Pin<&'a mut Self>) -> SocketHdl<'a, Q, T, N> {
        let stack = self.net.clone();
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
            // Borrow sockets deliberately do NOT provide `recv_owned`: it would have
            // to reinterpret the sender's value as this socket's message type with no
            // `TypeId` check (borrowed types pun across lifetimes, so a check is not
            // possible), which is unsound for a mismatched sender. Owned sends to a
            // borrow socket are instead serialized at the sender's type via `recv_bor`.
            recv_owned: None,
            recv_bor: Some(Self::recv_bor),
            recv_raw: Self::recv_raw,
            recv_err: Some(Self::recv_err),
        }
    }

    pub fn stack(&self) -> N::Target {
        self.net.clone()
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

        if let Ok(used) = wire_frames::encode_frame_err(ser, &hdr, err) {
            let len = used.len() as u16;
            wgr.commit(len);
            if let Some(wake) = qbox.waker.take() {
                wake.wake();
            }
        }
    }

    fn recv_bor(
        this: NonNull<()>,
        that: NonNull<()>,
        hdr: HeaderSeq,
        serfn: BorSerFn,
    ) -> Result<(), SocketSendError> {
        let this: NonNull<Self> = this.cast();
        let this: &Self = unsafe { this.as_ref() };
        let qbox: &mut QueueBox<Q> = unsafe { &mut *this.inner.get() };
        let qref = qbox.q.bbq_ref();
        let prod = qref.framed_producer();

        let Ok(mut wgr) = prod.grant(this.mtu) else {
            return Err(SocketSendError::NoSpace);
        };

        let used = serfn(that, hdr, &mut wgr)?;
        let len = used as u16;
        wgr.commit(len);

        if let Some(wake) = qbox.waker.take() {
            wake.wake();
        }

        Ok(())
    }

    fn recv_raw(this: NonNull<()>, that: &[u8], hdr: HeaderSeq) -> Result<(), SocketSendError> {
        let this: NonNull<Self> = this.cast();
        let this: &Self = unsafe { this.as_ref() };
        let qbox: &mut QueueBox<Q> = unsafe { &mut *this.inner.get() };
        let qref = qbox.q.bbq_ref();
        let prod = qref.framed_producer();

        // Re-encode the header
        let mut buf = [0u8; MAX_HDR_ENCODED_SIZE];
        let mut ser = Serializer {
            output: Slice::new(&mut buf),
        };
        let Ok(()) = encode_frame_hdr(&mut ser, &hdr) else {
            // If this fails, it likely means MAX_HDR_ENCODED_SIZE is being incorrectly calculaed
            log::error!("Encoding of HeaderSeq should never fail. This is a bug.");
            return Err(SocketSendError::WhatTheHell);
        };
        let Ok(hdr_used) = ser.output.finalize() else {
            // Slice flavor finalization should never fail
            unreachable!("Slice finalization should never error");
        };

        let Ok(needed) = u16::try_from(that.len() + hdr_used.len()) else {
            return Err(SocketSendError::NoSpace);
        };

        let Ok(mut wgr) = prod.grant(needed) else {
            return Err(SocketSendError::NoSpace);
        };
        let (hdr, body) = wgr.split_at_mut(hdr_used.len());
        hdr.copy_from_slice(hdr_used);
        body.copy_from_slice(that);
        wgr.commit(needed);

        if let Some(wake) = qbox.waker.take() {
            wake.wake();
        }

        Ok(())
    }
}

// impl SocketHdl

impl<'a, Q, T, N> SocketHdl<'a, Q, T, N>
where
    Q: BbqHandle,
    T: Serialize,
    N: NetStackHandle,
{
    pub fn port(&self) -> u8 {
        self.port
    }

    pub fn stack(&self) -> N::Target {
        unsafe { (*addr_of!((*self.ptr.as_ptr()).net)).clone() }
    }

    /// Await the next frame, returning a [`ResponseGrant`] that borrows the
    /// socket's queue.
    ///
    /// The returned [`ResponseGrant`] MUST be dropped before calling `recv()`
    /// again on this socket. A borrow socket holds at most one outstanding read
    /// grant, so a `recv()` issued while a previous `ResponseGrant` is still alive
    /// cannot make progress and will block indefinitely — access the grant (e.g.
    /// via [`ResponseGrant::try_access`]) and drop it, then `recv()` again.
    pub fn recv<'b>(&'b mut self) -> Recv<'b, 'a, Q, T, N> {
        Recv { hdl: self }
    }
}

impl<Q, T, N> Drop for Socket<Q, T, N>
where
    Q: BbqHandle,
    T: Serialize,
    N: NetStackHandle,
{
    fn drop(&mut self) {
        unsafe {
            let this = NonNull::from(&self.hdr);
            self.net.detach_socket(this);
        }
    }
}

impl<Q, T, N> Drop for SocketHdl<'_, Q, T, N>
where
    Q: BbqHandle,
    T: Serialize,
    N: NetStackHandle,
{
    fn drop(&mut self) {
        // Detaching on handle drop is the fast path: it unlinks the socket as soon
        // as the handle goes away, so the same pinned socket can be re-`attach`ed.
        // `Socket::drop` is a backstop for a leaked handle; `detach_socket` is
        // idempotent, so running both in the normal case is safe.
        //
        // SAFETY: the handle borrows the socket for `'a`, so `self.ptr` is valid
        // here, and `detach_socket` takes the netstack lock internally.
        unsafe {
            let net = self.stack();
            let hdr_ptr: *mut SocketHeader = addr_of_mut!((*self.ptr.as_ptr()).hdr);
            net.detach_socket(NonNull::new_unchecked(hdr_ptr));
        }
    }
}

// Bounds are load-bearing. `N::Target: Send + Sync`: the handle can clone
// `N::Target` through the socket pointer, so a non-thread-safe target (e.g.
// `Rc<NetStack>`, or a `NetStack` behind a non-`Sync` `ScopedRawMutex`) must not
// become usable from two threads. `Q: Send`: `recv()` accesses and clones the
// queue handle on whatever thread the handle is moved to, so an `Rc`-backed
// `BbqHandle` (which safe downstream code may define) must not make this `Send`.
unsafe impl<Q, T, N> Send for SocketHdl<'_, Q, T, N>
where
    Q: BbqHandle + Send,
    T: Serialize,
    N: NetStackHandle,
    N::Target: Send + Sync,
{
}

unsafe impl<Q, T, N> Sync for SocketHdl<'_, Q, T, N>
where
    Q: BbqHandle + Send,
    T: Serialize,
    N: NetStackHandle,
    N::Target: Send + Sync,
{
}

// impl Recv

impl<'a, Q, T, N> Future for Recv<'a, '_, Q, T, N>
where
    Q: BbqHandle,
    T: Serialize,
    N: NetStackHandle,
{
    type Output = ResponseGrant<'a, Q, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let net: N::Target = self.hdl.stack();
        let f = || -> Option<ResponseGrant<'a, Q, T>> {
            let this_ref: &Socket<Q, T, N> = unsafe { self.hdl.ptr.as_ref() };
            let qbox: &mut QueueBox<Q> = unsafe { &mut *this_ref.inner.get() };
            let cons: FramedConsumer<Q, u16> = qbox.q.framed_consumer();

            // An outstanding read grant (an unreleased `ResponseGrant` from a
            // previous `recv()` on this socket) surfaces from `read()` as an `Err`,
            // so it parks here exactly like an empty queue. A `ResponseGrant` MUST be
            // dropped before the next `recv()` on the same socket — see `recv()` — so
            // this parked state is not reached by correct code; when it is, it stays
            // parked (the socket waker is woken only by a producer, never by a grant
            // release) rather than busy-looping.
            if let Ok(resp) = cons.read() {
                let sli: &[u8] = resp.deref();

                if let Some(frame) = de_frame(sli) {
                    let BorrowedFrame { hdr, body } = frame;
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
                                _brw: PhantomData,
                            });
                        }
                        Err(err) => {
                            resp.release();
                            return Some(ResponseGrant {
                                hdr,
                                inner: ResponseGrantInner::Err(err),
                                _brw: PhantomData,
                            });
                        }
                    }
                }
            }

            let new_wake = cx.waker();
            if let Some(w) = qbox.waker.take()
                && !w.will_wake(new_wake)
            {
                w.wake();
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

unsafe impl<Q, T, N> Sync for Recv<'_, '_, Q, T, N>
where
    Q: BbqHandle + Send,
    T: Serialize,
    N: NetStackHandle,
    N::Target: Send + Sync,
{
}

// impl ResponseGrant

impl<Q: BbqHandle, T> ResponseGrant<'_, Q, T> {
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

impl<Q: BbqHandle, T> Drop for ResponseGrant<'_, Q, T> {
    fn drop(&mut self) {
        let old = core::mem::replace(
            &mut self.inner,
            ResponseGrantInner::Err(ProtocolError::Reserved),
        );
        match old {
            ResponseGrantInner::Ok { grant, .. } => {
                grant.release();
            }
            ResponseGrantInner::Err(_) => {}
        }
    }
}
