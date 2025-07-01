use core::{
    any::TypeId,
    cell::UnsafeCell,
    marker::PhantomData,
    pin::Pin,
    ptr::{NonNull, addr_of},
    task::{Context, Poll, Waker},
};

use cordyceps::list::Links;
use mutex::ScopedRawMutex;
use serde::{Serialize, de::DeserializeOwned};

use crate::{HeaderSeq, Key, NetStack, ProtocolError, interface_manager::InterfaceManager};

use super::{Attributes, OwnedMessage, Response, SocketHeader, SocketSendError, SocketVTable};

#[derive(Debug, PartialEq)]
pub struct StorageFull;

pub trait Storage<T: 'static>: 'static {
    fn is_full(&self) -> bool;
    fn is_empty(&self) -> bool;
    fn push(&mut self, t: T) -> Result<(), StorageFull>;
    fn try_pop(&mut self) -> Option<T>;
}

// Owned Socket
#[repr(C)]
pub struct Socket<S, T, R, M>
where
    S: Storage<Response<T>>,
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    // LOAD BEARING: must be first
    hdr: SocketHeader,
    pub(crate) net: &'static NetStack<R, M>,
    // TODO: just a single item, we probably want a more ring-buffery
    // option for this.
    inner: UnsafeCell<StoreBox<S, Response<T>>>,
}

pub struct SocketHdl<'a, S, T, R, M>
where
    S: Storage<Response<T>>,
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub(crate) ptr: NonNull<Socket<S, T, R, M>>,
    _lt: PhantomData<Pin<&'a mut Socket<S, T, R, M>>>,
    port: u8,
}

pub struct Recv<'a, 'b, S, T, R, M>
where
    S: Storage<Response<T>>,
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    hdl: &'a mut SocketHdl<'b, S, T, R, M>,
}

struct StoreBox<S: Storage<T>, T: 'static> {
    wait: Option<Waker>,
    sto: S,
    _pd: PhantomData<fn() -> T>,
}

// ---- impls ----

// impl OwnedMessage

// ...

// impl OwnedSocket

impl<S, T, R, M> Socket<S, T, R, M>
where
    S: Storage<Response<T>>,
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub const fn new(net: &'static NetStack<R, M>, key: Key, attrs: Attributes, sto: S) -> Self {
        Self {
            hdr: SocketHeader {
                links: Links::new(),
                vtable: const { &Self::vtable() },
                port: 0,
                attrs,
                key,
            },
            inner: UnsafeCell::new(StoreBox::new(sto)),
            net,
        }
    }

    pub fn attach<'a>(self: Pin<&'a mut Self>) -> SocketHdl<'a, S, T, R, M> {
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

    pub fn attach_broadcast<'a>(self: Pin<&'a mut Self>) -> SocketHdl<'a, S, T, R, M> {
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
            // TODO: We probably COULD support this, but I'm pretty sure it
            // would require serializing, copying to a buffer, then later
            // deserializing. I really don't know if we WANT this.
            recv_bor: None,
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
        let mutitem: &mut StoreBox<S, Response<T>> = unsafe { &mut *this.inner.get() };

        let msg = Err(OwnedMessage { hdr, t: err });
        if mutitem.sto.push(msg).is_ok() {
            if let Some(w) = mutitem.wait.take() {
                w.wake();
            }
        }
    }

    fn recv_owned(
        this: NonNull<()>,
        that: NonNull<()>,
        hdr: HeaderSeq,
        ty: &TypeId,
    ) -> Result<(), SocketSendError> {
        if &TypeId::of::<T>() != ty {
            debug_assert!(false, "Type Mismatch!");
            return Err(SocketSendError::TypeMismatch);
        }
        let that: NonNull<T> = that.cast();
        let that: &T = unsafe { that.as_ref() };
        let this: NonNull<Self> = this.cast();
        let this: &Self = unsafe { this.as_ref() };
        let mutitem: &mut StoreBox<S, Response<T>> = unsafe { &mut *this.inner.get() };

        let msg = Ok(OwnedMessage {
            hdr,
            t: that.clone(),
        });

        match mutitem.sto.push(msg) {
            Ok(()) => {
                if let Some(w) = mutitem.wait.take() {
                    w.wake();
                }
                Ok(())
            }
            Err(StorageFull) => Err(SocketSendError::NoSpace),
        }
    }

    // fn send_bor(
    //     this: NonNull<()>,
    //     that: NonNull<()>,
    //     src: Address,
    //     dst: Address,
    // ) -> Result<(), ()> {
    //     // I don't think we can support this?
    //     Err(())
    // }

    fn recv_raw(this: NonNull<()>, that: &[u8], hdr: HeaderSeq) -> Result<(), SocketSendError> {
        let this: NonNull<Self> = this.cast();
        let this: &Self = unsafe { this.as_ref() };
        let mutitem: &mut StoreBox<S, Response<T>> = unsafe { &mut *this.inner.get() };

        if mutitem.sto.is_full() {
            return Err(SocketSendError::NoSpace);
        }

        if let Ok(t) = postcard::from_bytes::<T>(that) {
            let msg = Ok(OwnedMessage { hdr, t });
            let _ = mutitem.sto.push(msg);
            if let Some(w) = mutitem.wait.take() {
                w.wake();
            }
            Ok(())
        } else {
            Err(SocketSendError::DeserFailed)
        }
    }
}

// impl OwnedSocketHdl

// TODO: impl drop, remove waker, remove socket
impl<'a, S, T, R, M> SocketHdl<'a, S, T, R, M>
where
    S: Storage<Response<T>>,
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub fn port(&self) -> u8 {
        self.port
    }

    pub fn stack(&self) -> &'static NetStack<R, M> {
        unsafe { *addr_of!((*self.ptr.as_ptr()).net) }
    }

    // TODO: This future is !Send? I don't fully understand why, but rustc complains
    // that since `NonNull<OwnedSocket<E>>` is !Sync, then this future can't be Send,
    // BUT impl'ing Sync unsafely on OwnedSocketHdl + OwnedSocket doesn't seem to help.
    pub fn recv<'b>(&'b mut self) -> Recv<'b, 'a, S, T, R, M> {
        Recv { hdl: self }
    }
}

impl<S, T, R, M> Drop for Socket<S, T, R, M>
where
    S: Storage<Response<T>>,
    T: Serialize + Clone + DeserializeOwned + 'static,
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

unsafe impl<S, T, R, M> Send for SocketHdl<'_, S, T, R, M>
where
    S: Storage<Response<T>>,
    T: Send,
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
}

unsafe impl<S, T, R, M> Sync for SocketHdl<'_, S, T, R, M>
where
    S: Storage<Response<T>>,
    T: Send,
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
}

// impl Recv

impl<S, T, R, M> Future for Recv<'_, '_, S, T, R, M>
where
    S: Storage<Response<T>>,
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    type Output = Response<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let net: &'static NetStack<R, M> = self.hdl.stack();
        let f = || {
            let this_ref: &Socket<S, T, R, M> = unsafe { self.hdl.ptr.as_ref() };
            let box_ref: &mut StoreBox<S, Response<T>> = unsafe { &mut *this_ref.inner.get() };

            if let Some(resp) = box_ref.sto.try_pop() {
                return Some(resp);
            }

            let new_wake = cx.waker();
            if let Some(w) = box_ref.wait.take() {
                if !w.will_wake(new_wake) {
                    w.wake();
                }
            }
            // NOTE: Okay to register waker AFTER checking, because we
            // have an exclusive lock
            box_ref.wait = Some(new_wake.clone());
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

unsafe impl<S, T, R, M> Sync for Recv<'_, '_, S, T, R, M>
where
    S: Storage<Response<T>>,
    T: Send,
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
}

// impl OneBox

impl<S: Storage<T>, T: 'static> StoreBox<S, T> {
    const fn new(sto: S) -> Self {
        Self {
            wait: None,
            sto,
            _pd: PhantomData,
        }
    }
}
