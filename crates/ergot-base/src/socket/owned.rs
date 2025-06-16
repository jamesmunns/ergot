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
use postcard_rpc::{Endpoint, Topic};
use serde::{Serialize, de::DeserializeOwned};

use crate::{HeaderSeq, NetStack, interface_manager::InterfaceManager};

use super::{
    EndpointData, OwnedMessage, SocketHeader, SocketHeaderEndpointReq, SocketHeaderEndpointResp,
    SocketHeaderTopicIn, SocketSendError, SocketVTable, TopicData,
};

// Owned Socket
#[repr(C)]
pub struct OwnedSocket<H, T, R, M>
where
    H: SocketHeader,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    // LOAD BEARING: must be first
    hdr: H,
    pub(crate) net: &'static NetStack<R, M>,
    // TODO: just a single item, we probably want a more ring-buffery
    // option for this.
    inner: UnsafeCell<OneBox<T>>,
}

pub struct OwnedSocketHdl<'a, H, T, R, M>
where
    H: SocketHeader,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub(crate) ptr: NonNull<OwnedSocket<H, T, R, M>>,
    _lt: PhantomData<Pin<&'a mut OwnedSocket<H, T, R, M>>>,
    port: u8,
}

pub struct Recv<'a, 'b, H, T, R, M>
where
    H: SocketHeader,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    hdl: &'a mut OwnedSocketHdl<'b, H, T, R, M>,
}

struct OneBox<T: 'static> {
    wait: Option<Waker>,
    t: Option<OwnedMessage<T>>,
}

// ---- impls ----

// impl OwnedMessage

// ...

// impl OwnedSocket

impl<T, R, M> OwnedSocket<SocketHeaderTopicIn, T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub const fn new_topic_in<U: Topic<Message = T>>(net: &'static NetStack<R, M>) -> Self {
        Self {
            hdr: SocketHeaderTopicIn {
                links: Links::new(),
                vtable: const { &Self::vtable() },
                port: 0,
                data: const { &TopicData::for_topic::<U>() },
            },
            inner: UnsafeCell::new(OneBox::new()),
            net,
        }
    }
}

impl<T, R, M> OwnedSocket<SocketHeaderEndpointReq, T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub const fn new_endpoint_req<E: Endpoint<Request = T>>(net: &'static NetStack<R, M>) -> Self {
        Self {
            hdr: SocketHeaderEndpointReq {
                links: Links::new(),
                vtable: const { &Self::vtable() },
                port: 0,
                data: const { &EndpointData::for_endpoint::<E>() },
            },
            inner: UnsafeCell::new(OneBox::new()),
            net,
        }
    }
}

impl<T, R, M> OwnedSocket<SocketHeaderEndpointResp, T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub const fn new_endpoint_resp<E: Endpoint<Response = T>>(
        net: &'static NetStack<R, M>,
    ) -> Self {
        Self {
            hdr: SocketHeaderEndpointResp {
                links: Links::new(),
                vtable: const { &Self::vtable() },
                port: 0,
                data: const { &EndpointData::for_endpoint::<E>() },
            },
            inner: UnsafeCell::new(OneBox::new()),
            net,
        }
    }
}

impl<H, T, R, M> OwnedSocket<H, T, R, M>
where
    H: SocketHeader,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub fn attach<'a>(self: Pin<&'a mut Self>) -> OwnedSocketHdl<'a, H, T, R, M> {
        let stack = self.net;
        let ptr_self: NonNull<Self> = NonNull::from(unsafe { self.get_unchecked_mut() });
        let ptr_erase: NonNull<H> = ptr_self.cast();
        let port = unsafe { H::attach(ptr_erase, stack).unwrap() };
        OwnedSocketHdl {
            ptr: ptr_self,
            _lt: PhantomData,
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

    pub fn stack(&self) -> &'static NetStack<R, M> {
        self.net
    }

    fn send_owned(
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
        let this: NonNull<Self> = this.cast();
        let this: &Self = unsafe { this.as_ref() };
        let mutitem: &mut OneBox<T> = unsafe { &mut *this.inner.get() };

        if mutitem.t.is_some() {
            return Err(SocketSendError::NoSpace);
        }

        mutitem.t = Some(OwnedMessage {
            hdr,
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

    fn send_raw(this: NonNull<()>, that: &[u8], hdr: HeaderSeq) -> Result<(), SocketSendError> {
        let this: NonNull<Self> = this.cast();
        let this: &Self = unsafe { this.as_ref() };
        let mutitem: &mut OneBox<T> = unsafe { &mut *this.inner.get() };

        if mutitem.t.is_some() {
            return Err(SocketSendError::NoSpace);
        }

        if let Ok(t) = postcard::from_bytes::<T>(that) {
            mutitem.t = Some(OwnedMessage { hdr, t });
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
impl<'a, H, T, R, M> OwnedSocketHdl<'a, H, T, R, M>
where
    H: SocketHeader,
    T: Serialize + DeserializeOwned + 'static,
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
    pub fn recv<'b>(&'b mut self) -> Recv<'b, 'a, H, T, R, M> {
        Recv { hdl: self }
    }
}

impl<H, T, R, M> Drop for OwnedSocket<H, T, R, M>
where
    H: SocketHeader,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    fn drop(&mut self) {
        println!("Dropping OwnedSocket!");
        unsafe {
            let stack = self.stack();
            let this = NonNull::from(&mut self.hdr);
            H::detach(this, stack);
        }
    }
}

unsafe impl<H, T, R, M> Send for OwnedSocketHdl<'_, H, T, R, M>
where
    H: SocketHeader,
    T: Send,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
}

unsafe impl<H, T, R, M> Sync for OwnedSocketHdl<'_, H, T, R, M>
where
    H: SocketHeader,
    T: Send,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
}

// impl Recv

impl<H, T, R, M> Future for Recv<'_, '_, H, T, R, M>
where
    H: SocketHeader,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    type Output = OwnedMessage<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let net: &'static NetStack<R, M> = self.hdl.stack();
        let f = || {
            let this_ref: &OwnedSocket<H, T, R, M> = unsafe { self.hdl.ptr.as_ref() };
            let box_ref: &mut OneBox<T> = unsafe { &mut *this_ref.inner.get() };
            if let Some(t) = box_ref.t.take() {
                Some(t)
            } else {
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
            }
        };
        let res = unsafe { net.with_lock(f) };
        if let Some(t) = res {
            Poll::Ready(t)
        } else {
            Poll::Pending
        }
    }
}

unsafe impl<H, T, R, M> Sync for Recv<'_, '_, H, T, R, M>
where
    H: SocketHeader,
    T: Send,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
}

// impl OneBox

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
