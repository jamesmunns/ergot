// use core::marker::PhantomData;
use core::cell::UnsafeCell;
use std::{
    any::TypeId,
    marker::PhantomData,
    pin::Pin,
    ptr::NonNull,
    task::{Context, Poll, Waker},
};

use cordyceps::list::Links;
use mutex::ScopedRawMutex;
use postcard_rpc::Key;
use serde::{Serialize, de::DeserializeOwned};

use crate::{Address, NetStack, interface_manager::InterfaceManager};

use super::{SocketHeader, SocketSendError, SocketVTable};

#[derive(Debug, PartialEq)]
pub struct OwnedMessage<T: 'static> {
    pub src: Address,
    pub dst: Address,
    pub t: T,
}

struct OneBox<T: 'static> {
    wait: Option<Waker>,
    t: Option<OwnedMessage<T>>,
}

impl<T: 'static> OneBox<T> {
    const fn new() -> Self {
        Self {
            wait: None,
            t: None,
        }
    }
}

impl<T: 'static> Default for OneBox<T> {
    fn default() -> Self {
        Self::new()
    }
}

// Owned Endpoint Server Socket
#[repr(C)]
pub struct OwnedSocket<T>
where
    T: Serialize + DeserializeOwned + 'static,
{
    // LOAD BEARING: must be first
    hdr: SocketHeader,
    // TODO: just a single item, we probably want a more ring-buffery
    // option for this.
    inner: UnsafeCell<OneBox<T>>,
}

pub struct OwnedSocketHdl<'a, T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub(crate) ptr: NonNull<OwnedSocket<T>>,
    _lt: PhantomData<Pin<&'a mut OwnedSocket<T>>>,
    pub(crate) net: &'static NetStack<R, M>,
    port: u8,
}

unsafe impl<T, R, M> Send for OwnedSocketHdl<'_, T, R, M>
where
    T: Send,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
}

unsafe impl<T, R, M> Sync for OwnedSocketHdl<'_, T, R, M>
where
    T: Send,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
}

unsafe impl<T, R, M> Sync for Recv<'_, '_, T, R, M>
where
    T: Send,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
}

pub struct Recv<'a, 'b, T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    hdl: &'a mut OwnedSocketHdl<'b, T, R, M>,
}

impl<T, R, M> Future for Recv<'_, '_, T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    type Output = OwnedMessage<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let res = self.hdl.net.inner.with_lock(|_net| {
            let this_ref: &OwnedSocket<T> = unsafe { self.hdl.ptr.as_ref() };
            let box_ref: &mut OneBox<T> = unsafe { &mut *this_ref.inner.get() };
            if let Some(t) = box_ref.t.take() {
                Some(t)
            } else {
                // todo
                assert!(box_ref.wait.is_none());
                // NOTE: Okay to register waker AFTER checking, because we
                // have an exclusive lock
                box_ref.wait = Some(cx.waker().clone());
                None
            }
        });
        if let Some(t) = res {
            Poll::Ready(t)
        } else {
            Poll::Pending
        }
    }
}

// TODO: impl drop, remove waker, remove socket
impl<'a, T, R, M> OwnedSocketHdl<'a, T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub fn port(&self) -> u8 {
        self.port
    }

    // TODO: This future is !Send? I don't fully understand why, but rustc complains
    // that since `NonNull<OwnedSocket<E>>` is !Sync, then this future can't be Send,
    // BUT impl'ing Sync unsafely on OwnedSocketHdl + OwnedSocket doesn't seem to help.
    pub fn recv<'b>(&'b mut self) -> Recv<'b, 'a, T, R, M> {
        Recv { hdl: self }
    }
}

impl<T, R, M> Drop for OwnedSocketHdl<'_, T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    fn drop(&mut self) {
        // first things first, remove the item from the list
        self.net.inner.with_lock(|net| {
            let node: NonNull<OwnedSocket<T>> = self.ptr;
            let node: NonNull<SocketHeader> = node.cast();
            unsafe {
                net.sockets.remove(node);
            }
        });
        // now that we are NOT in the list, we can soundly drop the item storage
        unsafe {
            let mut node: NonNull<OwnedSocket<T>> = self.ptr;
            let node_mut: &mut OwnedSocket<T> = node.as_mut();
            core::ptr::drop_in_place(node_mut.inner.get());
        }
    }
}

impl<T> OwnedSocket<T>
where
    T: Serialize + DeserializeOwned + 'static,
{
    pub const fn new(key: Key) -> Self {
        Self {
            hdr: SocketHeader {
                links: Links::new(),
                vtable: Self::vtable(),
                key,
                port: UnsafeCell::new(0),
            },
            inner: UnsafeCell::new(OneBox::new()),
        }
    }

    pub fn attach<'a, R: ScopedRawMutex + 'static, M: InterfaceManager + 'static>(
        self: Pin<&'a mut Self>,
        stack: &'static NetStack<R, M>,
    ) -> OwnedSocketHdl<'a, T, R, M> {
        let ptr_self: NonNull<Self> = NonNull::from(unsafe { self.get_unchecked_mut() });
        let ptr_erase: NonNull<SocketHeader> = ptr_self.cast();
        let port = unsafe { stack.attach_socket(ptr_erase) };
        OwnedSocketHdl {
            ptr: ptr_self,
            _lt: PhantomData,
            net: stack,
            port,
        }
        // TODO: once-check?
    }

    const fn vtable() -> SocketVTable {
        SocketVTable {
            send_owned: Some(Self::send_owned),
            // TODO: We probably COULD support this, but I'm pretty sure it
            // would require serializing, copying to a buffer, then later
            // deserializing. I really don't know if we WANT this.
            send_bor: None,
            send_raw: Self::send_raw,
        }
    }

    fn send_owned(
        this: NonNull<()>,
        that: NonNull<()>,
        ty: &TypeId,
        src: Address,
        dst: Address,
    ) -> Result<(), SocketSendError> {
        if &TypeId::of::<T>() != ty {
            debug_assert!(false, "Type Mismatch!");
            return Err(SocketSendError::TypeMismatch);
        }
        let that: NonNull<T> = that.cast();
        let this: NonNull<Self> = this.cast();
        let this: &Self = unsafe { this.as_ref() };
        let mutitem: &mut OneBox<T> = unsafe { &mut *this.inner.get() };

        if mutitem.t.is_some() {
            return Err(SocketSendError::NoSpace);
        }

        mutitem.t = Some(OwnedMessage {
            src,
            dst,
            t: unsafe { that.read() },
        });
        if let Some(w) = mutitem.wait.take() {
            w.wake();
        }

        Ok(())
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

    fn send_raw(
        this: NonNull<()>,
        that: &[u8],
        src: Address,
        dst: Address,
    ) -> Result<(), SocketSendError> {
        let this: NonNull<Self> = this.cast();
        let this: &Self = unsafe { this.as_ref() };
        let mutitem: &mut OneBox<T> = unsafe { &mut *this.inner.get() };

        if mutitem.t.is_some() {
            return Err(SocketSendError::NoSpace);
        }

        if let Ok(t) = postcard::from_bytes::<T>(that) {
            mutitem.t = Some(OwnedMessage { src, dst, t });
            if let Some(w) = mutitem.wait.take() {
                w.wake();
            }
            Ok(())
        } else {
            Err(SocketSendError::DeserFailed)
        }
    }
}
