use crate::{
    interface_manager::Profile,
    net_stack::{NetStack, NetStackHandle, Services},
};
use core::ops::Deref;
use mutex::{ConstInit, ScopedRawMutex};

use super::{discovery::Discovery, endpoints::Endpoints, topics::Topics};

pub struct ArcNetStack<R, P>
where
    R: ScopedRawMutex,
    P: Profile,
{
    inner: std::sync::Arc<NetStack<R, P>>,
}

impl<R, P> Deref for ArcNetStack<R, P>
where
    R: ScopedRawMutex,
    P: Profile,
{
    type Target = NetStack<R, P>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<R, P> Clone for ArcNetStack<R, P>
where
    R: ScopedRawMutex,
    P: Profile,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<R, P> NetStackHandle for ArcNetStack<R, P>
where
    R: ScopedRawMutex,
    P: Profile,
{
    type Mutex = R;
    type Profile = P;
    type Target = Self;

    fn stack(&self) -> Self::Target {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<R, M> NetStackHandle for &'_ ArcNetStack<R, M>
where
    R: ScopedRawMutex,
    M: Profile,
{
    type Mutex = R;
    type Profile = M;
    type Target = ArcNetStack<R, M>;

    fn stack(&self) -> Self::Target {
        ArcNetStack {
            inner: self.inner.clone(),
        }
    }
}

impl<R: ScopedRawMutex + ConstInit, M: Profile> ArcNetStack<R, M> {
    pub fn new_with_profile(p: M) -> Self {
        Self {
            inner: NetStack::new_arc(p),
        }
    }
}

impl<R: ScopedRawMutex + ConstInit, M: Profile + Default> ArcNetStack<R, M> {
    pub fn new() -> Self {
        Self {
            inner: NetStack::new_arc(Default::default()),
        }
    }
}

impl<R: ScopedRawMutex + ConstInit, M: Profile + Default> Default for ArcNetStack<R, M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: ScopedRawMutex, M: Profile> ArcNetStack<R, M> {
    pub fn services(&self) -> Services<Self> {
        Services {
            inner: self.clone(),
        }
    }

    pub fn endpoints(&self) -> Endpoints<Self> {
        Endpoints {
            inner: self.clone(),
        }
    }

    pub fn topics(&self) -> Topics<Self> {
        Topics {
            inner: self.clone(),
        }
    }

    pub fn discovery(&self) -> Discovery<Self> {
        Discovery {
            inner: self.clone(),
        }
    }
}
