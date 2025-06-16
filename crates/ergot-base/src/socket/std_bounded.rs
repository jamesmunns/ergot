use std::{
    any::TypeId,
    cell::UnsafeCell,
    collections::VecDeque,
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
pub struct StdBoundedSocket<H, T, R, M>
where
    H: SocketHeader,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    // LOAD BEARING: must be first
    hdr: H,
    net: &'static NetStack<R, M>,
    // TODO: just a single item, we probably want a more ring-buffery
    // option for this.
    inner: UnsafeCell<BoundedQueue<T>>,
}

pub struct StdBoundedSocketHdl<'a, H, T, R, M>
where
    H: SocketHeader,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub(crate) ptr: NonNull<StdBoundedSocket<H, T, R, M>>,
    _lt: PhantomData<Pin<&'a mut StdBoundedSocket<H, T, R, M>>>,
    port: u8,
}

pub struct Recv<'a, 'b, H, T, R, M>
where
    H: SocketHeader,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    hdl: &'a mut StdBoundedSocketHdl<'b, H, T, R, M>,
}

struct BoundedQueue<T: 'static> {
    wait: Option<Waker>,
    // TODO: We could probably do better than a VecDeque with a boxed slice
    // and a ringbuffer, which could maybe also be shared with the std
    // inline buffer, but for now this is fine.
    queue: VecDeque<OwnedMessage<T>>,
    max_len: usize,
}

// ---- impls ----

// impl OwnedMessage

// ...

// impl StdBoundedSocket

impl<T, R, M> StdBoundedSocket<SocketHeaderTopicIn, T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub fn new_topic_in<U: Topic>(net: &'static NetStack<R, M>, bound: usize) -> Self {
        Self {
            hdr: SocketHeaderTopicIn {
                links: Links::new(),
                vtable: const { &Self::vtable() },
                port: 0,
                data: const { &TopicData::for_topic::<U>() },
            },
            inner: UnsafeCell::new(BoundedQueue::new(bound)),
            net,
        }
    }
}

impl<T, R, M> StdBoundedSocket<SocketHeaderEndpointReq, T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub fn new_endpoint_req<E: Endpoint>(net: &'static NetStack<R, M>, bound: usize) -> Self {
        Self {
            hdr: SocketHeaderEndpointReq {
                links: Links::new(),
                vtable: const { &Self::vtable() },
                port: 0,
                data: const { &EndpointData::for_endpoint::<E>() },
            },
            inner: UnsafeCell::new(BoundedQueue::new(bound)),
            net,
        }
    }
}

impl<T, R, M> StdBoundedSocket<SocketHeaderEndpointResp, T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub fn new_endpoint_resp<E: Endpoint>(net: &'static NetStack<R, M>, bound: usize) -> Self {
        Self {
            hdr: SocketHeaderEndpointResp {
                links: Links::new(),
                vtable: const { &Self::vtable() },
                port: 0,
                data: const { &EndpointData::for_endpoint::<E>() },
            },
            inner: UnsafeCell::new(BoundedQueue::new(bound)),
            net,
        }
    }
}

impl<H, T, R, M> StdBoundedSocket<H, T, R, M>
where
    H: SocketHeader,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub fn stack(&self) -> &'static NetStack<R, M> {
        self.net
    }

    pub fn attach<'a>(self: Pin<&'a mut Self>) -> StdBoundedSocketHdl<'a, H, T, R, M> {
        let stack = self.net;
        let ptr_self: NonNull<Self> = NonNull::from(unsafe { self.get_unchecked_mut() });
        let ptr_erase: NonNull<H> = ptr_self.cast();
        let port = unsafe { H::attach(ptr_erase, stack).unwrap() };
        StdBoundedSocketHdl {
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
        let mutitem: &mut BoundedQueue<T> = unsafe { &mut *this.inner.get() };

        if mutitem.queue.len() >= mutitem.max_len {
            return Err(SocketSendError::NoSpace);
        }

        mutitem.queue.push_back(OwnedMessage {
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
        let mutitem: &mut BoundedQueue<T> = unsafe { &mut *this.inner.get() };

        if mutitem.queue.len() >= mutitem.max_len {
            return Err(SocketSendError::NoSpace);
        }

        if let Ok(t) = postcard::from_bytes::<T>(that) {
            mutitem.queue.push_back(OwnedMessage { hdr, t });
            if let Some(w) = mutitem.wait.take() {
                w.wake();
            }
            Ok(())
        } else {
            Err(SocketSendError::DeserFailed)
        }
    }
}

// impl StdBoundedSocketHdl

// TODO: impl drop, remove waker, remove socket
impl<'a, H, T, R, M> StdBoundedSocketHdl<'a, H, T, R, M>
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
    // that since `NonNull<StdBoundedSocket<E>>` is !Sync, then this future can't be Send,
    // BUT impl'ing Sync unsafely on StdBoundedSocketHdl + StdBoundedSocket doesn't seem to help.
    pub fn recv<'b>(&'b mut self) -> Recv<'b, 'a, H, T, R, M> {
        Recv { hdl: self }
    }
}

impl<H, T, R, M> Drop for StdBoundedSocket<H, T, R, M>
where
    H: SocketHeader,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    fn drop(&mut self) {
        println!("Dropping StdBoundedSocket!");
        unsafe {
            let net = self.net;
            let this = NonNull::from(&self.hdr);
            H::detach(this, net);
        }
    }
}

unsafe impl<H, T, R, M> Send for StdBoundedSocketHdl<'_, H, T, R, M>
where
    H: SocketHeader,
    T: Send,
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
}

unsafe impl<H, T, R, M> Sync for StdBoundedSocketHdl<'_, H, T, R, M>
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
        let net = self.hdl.stack();
        let f = || {
            let this_ref: &StdBoundedSocket<H, T, R, M> = unsafe { self.hdl.ptr.as_ref() };
            let box_ref: &mut BoundedQueue<T> = unsafe { &mut *this_ref.inner.get() };
            if let Some(t) = box_ref.queue.pop_front() {
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

// impl BoundedQueue

impl<T: 'static> BoundedQueue<T> {
    fn new(bound: usize) -> Self {
        Self {
            wait: None,
            queue: VecDeque::new(),
            max_len: bound,
        }
    }
}
