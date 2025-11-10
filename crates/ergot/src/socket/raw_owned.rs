//! "Raw Owned" sockets
//!
//! "Owned" sockets require `T: 'static`, and store messages in their deserialized `T` form,
//! rather as serialized bytes.
//!
//! "Raw Owned" sockets are generic over the [`Storage`] trait, which describes a basic
//! ring buffer. The [`owned`](crate::socket::owned) module contains variants of this
//! raw socket that use a specific kind of ring buffer impl, e.g. using std or stackful
//! storage.

use core::{
    any::TypeId,
    cell::UnsafeCell,
    marker::PhantomData,
    pin::Pin,
    ptr::{NonNull, addr_of},
    task::{Context, Poll, Waker},
};

use cordyceps::list::Links;
use serde::de::DeserializeOwned;

use super::{Attributes, HeaderMessage, Response, SocketHeader, SocketSendError, SocketVTable};
use crate::logging::trace;
use crate::{HeaderSeq, Key, ProtocolError, nash::NameHash, net_stack::NetStackHandle};

#[derive(Debug, PartialEq)]
pub struct StorageFull;

pub trait Storage<T: 'static>: 'static {
    fn is_full(&self) -> bool;
    fn is_empty(&self) -> bool;
    fn push(&mut self, t: T) -> Result<(), StorageFull>;
    fn try_pop(&mut self) -> Option<T>;
}

/// SocketPtr represents an equivalent ownership as Pin<&'a mut Socket<S, T, N>>,
/// however we use it to abstract over `Pin<&mut Socket<S, T, N>>` and
/// `Pin<Box<Socket<S, T, N>>>`.
///
/// This is achieved by having an `on_drop` function, which for borrowed items
/// is a no-op, but for owned/boxed items acts as a drop.
struct SocketPtr<'a, S, T, N>
where
    S: Storage<Response<T>>,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
    ptr: NonNull<Socket<S, T, N>>,
    on_drop: unsafe fn(NonNull<Socket<S, T, N>>),
    _pd: PhantomData<Pin<&'a mut Socket<S, T, N>>>,
}

impl<S, T, N> SocketPtr<'_, S, T, N>
where
    S: Storage<Response<T>>,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
    pub(crate) fn as_ptr(&self) -> NonNull<Socket<S, T, N>> {
        self.ptr
    }
}

impl<S, T, N> Drop for SocketPtr<'_, S, T, N>
where
    S: Storage<Response<T>>,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
    fn drop(&mut self) {
        unsafe { (self.on_drop)(self.ptr) }
    }
}

impl<S, T, N> From<Pin<&mut Socket<S, T, N>>> for SocketPtr<'_, S, T, N>
where
    S: Storage<Response<T>>,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
    fn from(value: Pin<&mut Socket<S, T, N>>) -> Self {
        let ptr_self: NonNull<Socket<S, T, N>> =
            NonNull::from(unsafe { value.get_unchecked_mut() });
        SocketPtr {
            ptr: ptr_self,
            on_drop: Socket::<S, T, N>::nop_drop,
            _pd: PhantomData,
        }
    }
}

#[cfg(feature = "std")]
impl<S, T, N> From<Pin<Box<Socket<S, T, N>>>> for SocketPtr<'static, S, T, N>
where
    S: Storage<Response<T>>,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
    fn from(value: Pin<Box<Socket<S, T, N>>>) -> Self {
        let box_self: Box<Socket<S, T, N>> = unsafe { Pin::into_inner_unchecked(value) };
        let ptr_self: NonNull<Socket<S, T, N>> =
            unsafe { NonNull::new_unchecked(Box::into_raw(box_self)) };
        SocketPtr {
            ptr: ptr_self,
            on_drop: Socket::<S, T, N>::pin_box_drop,
            _pd: PhantomData,
        }
    }
}

// Socket
#[repr(C)]
pub struct Socket<S, T, N>
where
    S: Storage<Response<T>>,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
    // LOAD BEARING: must be first
    hdr: UnsafeCell<SocketHeader>,
    pub(crate) net: N::Target,
    inner: UnsafeCell<StoreBox<S, Response<T>>>,
}

pub struct SocketHdl<'a, S, T, N>
where
    S: Storage<Response<T>>,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
    ptr: SocketPtr<'a, S, T, N>,
    port: u8,
}

pub struct Recv<'a, 'b, S, T, N>
where
    S: Storage<Response<T>>,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
    hdl: &'a mut SocketHdl<'b, S, T, N>,
}

struct StoreBox<S: Storage<T>, T: 'static> {
    wait: Option<Waker>,
    sto: S,
    _pd: PhantomData<fn() -> T>,
}

// ---- impls ----

// impl Socket

impl<S, T, N> Socket<S, T, N>
where
    S: Storage<Response<T>>,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
    unsafe fn nop_drop(_: NonNull<Self>) {}

    #[cfg(feature = "std")]
    unsafe fn pin_box_drop(this: NonNull<Self>) {
        _ = unsafe { Box::from_raw(this.as_ptr()) };
    }

    pub const fn new(
        net: N::Target,
        key: Key,
        attrs: Attributes,
        sto: S,
        name: Option<&str>,
    ) -> Self {
        Self {
            hdr: UnsafeCell::new(SocketHeader {
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
            }),
            inner: UnsafeCell::new(StoreBox::new(sto)),
            net,
        }
    }

    pub fn attach(self: Pin<&mut Self>) -> SocketHdl<'_, S, T, N> {
        let stack = self.net.clone();
        let sp: SocketPtr<'_, S, T, N> = self.into();
        let ptr_self: NonNull<Self> = sp.as_ptr();
        let ptr_erase: NonNull<SocketHeader> = ptr_self.cast();
        let port = unsafe { stack.attach_socket(ptr_erase) };
        SocketHdl { ptr: sp, port }
    }

    #[cfg(feature = "std")]
    pub fn attach_boxed(self: Pin<Box<Self>>) -> SocketHdl<'static, S, T, N> {
        let stack = self.net.clone();
        let sp: SocketPtr<S, T, N> = self.into();
        let ptr_self: NonNull<Self> = sp.as_ptr();
        let ptr_erase: NonNull<SocketHeader> = ptr_self.cast();
        let port = unsafe { stack.attach_socket(ptr_erase) };
        SocketHdl { ptr: sp, port }
    }

    pub fn attach_broadcast(self: Pin<&mut Self>) -> SocketHdl<'_, S, T, N> {
        let stack = self.net.clone();
        let sp: SocketPtr<'_, S, T, N> = self.into();
        let ptr_self: NonNull<Self> = sp.as_ptr();
        let ptr_erase: NonNull<SocketHeader> = ptr_self.cast();
        unsafe { stack.attach_broadcast_socket(ptr_erase) };
        SocketHdl { ptr: sp, port: 255 }
    }

    #[cfg(feature = "std")]
    pub fn attach_broadcast_boxed(self: Pin<Box<Self>>) -> SocketHdl<'static, S, T, N> {
        let stack = self.net.clone();
        let sp: SocketPtr<S, T, N> = self.into();
        let ptr_self: NonNull<Self> = sp.as_ptr();
        let ptr_erase: NonNull<SocketHeader> = ptr_self.cast();
        unsafe { stack.attach_broadcast_socket(ptr_erase) };
        SocketHdl { ptr: sp, port: 255 }
    }

    const fn vtable() -> SocketVTable {
        SocketVTable {
            recv_owned: Some(Self::recv_owned),
            // TODO: We probably COULD support this, but I'm pretty sure it
            // would require serializing, copying to a buffer, then later
            // deserializing. I really don't know if we WANT this.
            //
            // TODO: EXTRA danger: if the item is borrowed we can't use TypeId,
            // which makes it VERY DIFFICULT to do this soundly: The sender and receiver
            // sockets might not ACTUALLY be the same type, for example if the two
            // types pun to each other, e.g. `&str` and `String`. THERE BE EVEN MORE
            // DRAGONS HERE
            // Update: This is now somewhat better because send_bor passes a serializing fn
            recv_bor: None,
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
        let mutitem: &mut StoreBox<S, Response<T>> = unsafe { &mut *this.inner.get() };

        let msg = Err(HeaderMessage { hdr, t: err });
        if mutitem.sto.push(msg).is_ok()
            && let Some(w) = mutitem.wait.take()
        {
            w.wake();
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

        let msg = Ok(HeaderMessage {
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

    fn recv_raw(this: NonNull<()>, that: &[u8], hdr: HeaderSeq) -> Result<(), SocketSendError> {
        let this: NonNull<Self> = this.cast();
        let this: &Self = unsafe { this.as_ref() };
        let mutitem: &mut StoreBox<S, Response<T>> = unsafe { &mut *this.inner.get() };

        if mutitem.sto.is_full() {
            trace!("{}: recv_raw: StorageFull", hdr);
            return Err(SocketSendError::NoSpace);
        }

        if let Ok(t) = postcard::from_bytes::<T>(that) {
            let msg = Ok(HeaderMessage { hdr, t });
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

// impl SocketHdl

impl<'a, S, T, N> SocketHdl<'a, S, T, N>
where
    S: Storage<Response<T>>,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
    pub fn port(&self) -> u8 {
        self.port
    }

    pub fn stack(&self) -> N::Target {
        unsafe { (*addr_of!((*self.ptr.as_ptr().as_ptr()).net)).clone() }
    }

    pub fn try_recv(&mut self) -> Option<Response<T>> {
        let net: N::Target = self.stack();
        let f = || {
            let this_ref: &Socket<S, T, N> = unsafe { self.ptr.as_ptr().as_ref() };
            let box_ref: &mut StoreBox<S, Response<T>> = unsafe { &mut *this_ref.inner.get() };

            box_ref.sto.try_pop()
        };
        unsafe { net.with_lock(f) }
    }

    pub fn recv<'b>(&'b mut self) -> Recv<'b, 'a, S, T, N> {
        Recv { hdl: self }
    }
}

impl<S, T, N> Drop for SocketHdl<'_, S, T, N>
where
    S: Storage<Response<T>>,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
    fn drop(&mut self) {
        // SAFETY: the drop of the SocketHdl signifies the end of the pinned relationship
        // of the socket. At the beginning of drop, we are pinned, and then remove ourselves
        // from the socket list.
        //
        // `detach_socket`is performed with the mutex locked.
        //
        // Once we return from this function, the contained `SocketPtr` is dropped, and its
        // `on_drop` function is called. If this handle was created with a `Pin<&mut Socket>`,
        // the socket itself will not yet be dropped. If this SocketHdl was created with a
        // Pin<Box<Socket>>, then that socket will be dropped by the on_drop handler.
        unsafe {
            let net = self.stack();
            let ptr: *mut SocketHeader = self.ptr.as_ptr().as_ref().hdr.get();
            let this: NonNull<SocketHeader> = NonNull::new_unchecked(ptr);
            net.detach_socket(this);
        }
    }
}

unsafe impl<S, T, N> Send for SocketHdl<'_, S, T, N>
where
    S: Storage<Response<T>>,
    T: Send,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
}

unsafe impl<S, T, N> Sync for SocketHdl<'_, S, T, N>
where
    S: Storage<Response<T>>,
    T: Send,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
}

// impl Recv

impl<S, T, N> Future for Recv<'_, '_, S, T, N>
where
    S: Storage<Response<T>>,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
    type Output = Response<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let net: N::Target = self.hdl.stack();
        let f = || {
            let this_ref: &Socket<S, T, N> = unsafe { self.hdl.ptr.as_ptr().as_ref() };
            let box_ref: &mut StoreBox<S, Response<T>> = unsafe { &mut *this_ref.inner.get() };

            if let Some(resp) = box_ref.sto.try_pop() {
                return Some(resp);
            }

            let new_wake = cx.waker();
            if let Some(w) = box_ref.wait.take()
                && !w.will_wake(new_wake)
            {
                w.wake();
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

unsafe impl<S, T, N> Sync for Recv<'_, '_, S, T, N>
where
    S: Storage<Response<T>>,
    T: Send,
    T: Clone + DeserializeOwned + 'static,
    N: NetStackHandle,
{
}

// impl StoreBox

impl<S: Storage<T>, T: 'static> StoreBox<S, T> {
    const fn new(sto: S) -> Self {
        Self {
            wait: None,
            sto,
            _pd: PhantomData,
        }
    }
}
