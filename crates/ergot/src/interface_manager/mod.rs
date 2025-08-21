//! The Interface Manager
//!
//! The [`NetStack`] is generic over a "Profile", which is how it handles
//! any external interfaces of the current program or device.
//!
//! Different profiles may support a various number of external
//! interfaces. The simplest profile is the "Null Profile",
//! Which supports no external interfaces, meaning that messages may only be
//! routed locally.
//!
//! The next simplest profile is one that only supports zero or one
//! active interfaces, for example if a device is directly connected to a PC
//! using USB. In this case, routing is again simple: if messages are not
//! intended for the local device, they should be routed out of the one external
//! interface. Similarly, if we support an interface, but it is not connected
//! (e.g. the USB cable is unplugged), all packets with external destinations
//! will fail to send.
//!
//! For more complex devices, a profile with multiple (bounded or
//! unbounded) interfaces, and more complex routing capabilities, may be
//! selected.
//!
//! Unlike Sockets, which might be various and diverse on all systems, a system
//! is expected to have one statically-known profile, which may
//! manage various and diverse interfaces.
//!
//! In general when sending a message, the [`NetStack`] will check if the
//! message is definitively for the local device (e.g. Net ID = 0, Node ID = 0),
//! and if not the NetStack will pass the message to the Interface Manager. If
//! the profile can route this packet, it informs the NetStack it has
//! done so. If the Interface Manager realizes that the packet is still for us
//! (e.g. matching a Net ID and Node ID of the local device), it may bounce the
//! message back to the NetStack to locally route.
//!
//! [`NetStack`]: crate::NetStack

use crate::{AnyAllAppendix, Header, ProtocolError, wire_frames::CommonHeader};
use serde::Serialize;

pub mod interface_impls;
pub mod profiles;
pub mod utils;

pub trait ConstInit {
    const INIT: Self;
}

// An interface send is very similar to a socket send, with the exception
// that interface sends are ALWAYS a serializing operation (or required
// serialization has already been done), which means we don't need to
// differentiate between "send owned" and "send borrowed". The exception
// to this is "send raw", where serialization has already been done, e.g.
// if we are routing a packet.
pub trait Profile {
    /// The kind of type that is used to identify a single interface.
    /// If a Profile only supports a single interface, this is often the `()` type.
    /// If a Profile supports many interfaces, this could be an enum or integer type.
    type InterfaceIdent;

    fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError>;
    fn send_err(&mut self, hdr: &Header, err: ProtocolError) -> Result<(), InterfaceSendError>;
    fn send_raw(
        &mut self,
        hdr: &Header,
        hdr_raw: &[u8],
        data: &[u8],
    ) -> Result<(), InterfaceSendError>;

    fn interface_state(&mut self, ident: Self::InterfaceIdent) -> Option<InterfaceState>;
    fn set_interface_state(
        &mut self,
        ident: Self::InterfaceIdent,
        state: InterfaceState,
    ) -> Result<(), SetStateError>;
}

/// Interfaces define how messages are transported over the wire
pub trait Interface {
    /// The Sink is the type used to send messages out of the Profile
    type Sink: InterfaceSink;
}

/// The "Sink" side of the interface.
///
/// This is typically held by a profile, and feeds data to the interface's
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

#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum InterfaceSendError {
    /// Refusing to send local destination remotely
    DestinationLocal,
    /// Profile does not know how to route to requested destination
    NoRouteToDest,
    /// Profile found a destination interface, but that interface
    /// was full in space/slots
    InterfaceFull,
    /// TODO: Remove
    PlaceholderOhNo,
    /// Destination was an "any" port, but a key was not provided
    AnyPortMissingKey,
    /// TTL has reached the terminal value
    TtlExpired,
}

/// An error when deregistering an interface
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeregisterError {
    NoSuchInterface,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InterfaceState {
    // Missing sink, no net id
    Down,
    // Has sink, no net id
    Inactive,
    // Has sink, has node_id but not net_id
    ActiveLocal { node_id: u8 },
    // Has sink, has net id
    Active { net_id: u16, node_id: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum RegisterSinkError {
    AlreadyActive,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum SetStateError {
    InterfaceNotFound,
    InvalidNodeId,
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
