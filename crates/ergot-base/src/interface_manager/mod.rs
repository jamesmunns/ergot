//! The Interface Manager
//!
//! The [`NetStack`] is generic over an "Interface Manager", which is
//! responsible for handling any external interfaces of the current program
//! or device.
//!
//! Different interface managers may support a various number of external
//! interfaces. The simplest interface manager is a "Null Interface Manager",
//! Which supports no external interfaces, meaning that messages may only be
//! routed locally.
//!
//! The next simplest interface manager is one that only supports zero or one
//! active interfaces, for example if a device is directly connected to a PC
//! using USB. In this case, routing is again simple: if messages are not
//! intended for the local device, they should be routed out of the one external
//! interface. Similarly, if we support an interface, but it is not connected
//! (e.g. the USB cable is unplugged), all packets with external destinations
//! will fail to send.
//!
//! For more complex devices, an interface manager with multiple (bounded or
//! unbounded) interfaces, and more complex routing capabilities, may be
//! selected.
//!
//! Unlike Sockets, which might be various and diverse on all systems, a system
//! is expected to have one statically-known interface manager, which may
//! manage various and diverse interfaces. Therefore, the interface manager is
//! a generic type (unlike sockets), while the interfaces owned by an interface
//! manager use similar "trick"s like the socket list to handle different
//! kinds of interfaces (for example, USB on one interface, and RS-485 on
//! another).
//!
//! In general when sending a message, the [`NetStack`] will check if the
//! message is definitively for the local device (e.g. Net ID = 0, Node ID = 0),
//! and if not the NetStack will pass the message to the Interface Manager. If
//! the interface manager can route this packet, it informs the NetStack it has
//! done so. If the Interface Manager realizes that the packet is still for us
//! (e.g. matching a Net ID and Node ID of the local device), it may bounce the
//! message back to the NetStack to locally route.
//!
//! [`NetStack`]: crate::NetStack

use core::any::Any;

use crate::{
    AnyAllAppendix, Header, ProtocolError,
    net_stack::{StackRegisterSinkError, StackSetActiveError},
    wire_frames::CommonHeader,
};
use serde::Serialize;

pub mod impls;
pub mod utils;

#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum InterfaceSendError {
    /// Refusing to send local destination remotely
    DestinationLocal,
    /// Interface Manager does not know how to route to requested destination
    NoRouteToDest,
    /// Interface Manager found a destination interface, but that interface
    /// was full in space/slots
    InterfaceFull,
    /// TODO: Remove
    PlaceholderOhNo,
    /// Destination was an "any" port, but a key was not provided
    AnyPortMissingKey,
    /// TTL has reached the terminal value
    TtlExpired,
}

pub trait ConstInit {
    const INIT: Self;
}

// An interface send is very similar to a socket send, with the exception
// that interface sends are ALWAYS a serializing operation (or required
// serialization has already been done), which means we don't need to
// differentiate between "send owned" and "send borrowed". The exception
// to this is "send raw", where serialization has already been done, e.g.
// if we are routing a packet.
pub trait InterfaceManager {
    type InterfaceIdent;

    fn get_interface<T: Interface + Any>(&mut self, ident: Self::InterfaceIdent) -> Option<&mut T>;
    fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError>;
    fn send_err(&mut self, hdr: &Header, err: ProtocolError) -> Result<(), InterfaceSendError>;
    fn send_raw(
        &mut self,
        hdr: &Header,
        hdr_raw: &[u8],
        data: &[u8],
    ) -> Result<(), InterfaceSendError>;
    fn interface_register<I: Interface>(
        &mut self,
        ident: Self::InterfaceIdent,
        sink: I::Sink,
    ) -> Result<(), StackRegisterSinkError>;
    fn interface_deregister<I: Interface>(
        &mut self,
        ident: Self::InterfaceIdent,
    ) -> Option<I::Sink>;
    fn interface_state(&mut self, ident: Self::InterfaceIdent) -> Option<InterfaceState>;
    fn interface_set_active(
        &mut self,
        ident: Self::InterfaceIdent,
        net_id: u16,
    ) -> Result<(), StackSetActiveError>;
}

impl InterfaceSendError {
    pub fn to_error(&self) -> ProtocolError {
        match self {
            InterfaceSendError::DestinationLocal => ProtocolError::ISE_DESTINATION_LOCAL,
            InterfaceSendError::NoRouteToDest => ProtocolError::ISE_NO_ROUTE_TO_DEST,
            InterfaceSendError::InterfaceFull => ProtocolError::ISE_INTERFACE_FULL,
            InterfaceSendError::PlaceholderOhNo => ProtocolError::ISE_PLACEHOLDER_OH_NO,
            InterfaceSendError::AnyPortMissingKey => ProtocolError::ISE_ANY_PORT_MISSING_KEY,
            InterfaceSendError::TtlExpired => ProtocolError::ISE_TTL_EXPIRED,
        }
    }
}

pub enum InterfaceState {
    // Missing sink, no net id
    Down,
    // Has sink, no net id
    Inactive,
    // Has sink, has net id
    Active { net_id: u16 },
}

pub enum RegisterSinkError {
    AlreadyActive,
}

pub enum SetActiveError {
    CantSetZero,
    NoActiveSink,
}

#[allow(clippy::result_unit_err)]
pub trait Interface: Any {
    type Sink: InterfaceSink;

    fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError>;
    fn send_err(&mut self, hdr: &Header, err: ProtocolError) -> Result<(), InterfaceSendError>;
    fn send_raw(
        &mut self,
        hdr: &Header,
        hdr_raw: &[u8],
        data: &[u8],
    ) -> Result<(), InterfaceSendError>;
    fn register(&mut self, sink: Self::Sink) -> Result<(), RegisterSinkError>;
    fn deregister(&mut self) -> Option<Self::Sink>;
    fn state(&self) -> InterfaceState;
    fn set_active(&mut self, net_id: u16) -> Result<(), SetActiveError>;
}

/// The "Sink" side of the interface.
///
/// This is typically held by an InterfaceManager, and feeds data to the interface's
/// TX worker.
#[allow(clippy::result_unit_err)]
pub trait InterfaceSink {
    fn send_ty<T: Serialize>(
        &mut self,
        hdr: &CommonHeader,
        apdx: Option<&AnyAllAppendix>,
        body: &T,
    ) -> Result<(), ()>;
    fn send_raw(&mut self, hdr: &CommonHeader, hdr_raw: &[u8], body: &[u8]) -> Result<(), ()>;
    fn send_err(&mut self, hdr: &CommonHeader, err: ProtocolError) -> Result<(), ()>;
}

/// A wrapper that turns a single Interface into an Interface Manager
pub struct SoloInterface<I: Interface> {
    inner: I,
}

impl<I: Interface + ConstInit> ConstInit for SoloInterface<I> {
    const INIT: Self = Self { inner: I::INIT };
}

impl<I: Interface> InterfaceManager for SoloInterface<I> {
    type InterfaceIdent = ();

    fn get_interface<T: Interface + 'static>(&mut self, _ident: ()) -> Option<&mut T> {
        let i: &mut dyn Any = &mut self.inner;
        i.downcast_mut()
    }

    #[inline]
    fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError> {
        self.inner.send(hdr, data)
    }

    #[inline]
    fn send_err(&mut self, hdr: &Header, err: ProtocolError) -> Result<(), InterfaceSendError> {
        self.inner.send_err(hdr, err)
    }

    #[inline]
    fn send_raw(
        &mut self,
        hdr: &Header,
        hdr_raw: &[u8],
        data: &[u8],
    ) -> Result<(), InterfaceSendError> {
        self.inner.send_raw(hdr, hdr_raw, data)
    }

    fn interface_register<T: Interface>(
        &mut self,
        _ident: (),
        sink: T::Sink,
    ) -> Result<(), StackRegisterSinkError> {
        let i: &mut dyn Any = &mut self.inner;
        let Some(i): Option<&mut T> = i.downcast_mut() else {
            return Err(StackRegisterSinkError::NoSuchInterface);
        };
        i.register(sink)?;
        Ok(())
    }

    fn interface_deregister<T: Interface>(&mut self, _ident: ()) -> Option<T::Sink> {
        let i: &mut dyn Any = &mut self.inner;
        let i: &mut T = i.downcast_mut()?;
        i.deregister()
    }

    fn interface_state(&mut self, _ident: ()) -> Option<InterfaceState> {
        Some(self.inner.state())
    }

    fn interface_set_active(&mut self, _ident: (), net_id: u16) -> Result<(), StackSetActiveError> {
        self.inner.set_active(net_id)?;
        Ok(())
    }
}
