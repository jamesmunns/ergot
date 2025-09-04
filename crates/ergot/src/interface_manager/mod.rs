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

/// A successful Net ID assignment or refresh from a Seed Router
pub struct SeedNetAssignment {
    /// The newly assigned net id
    pub net_id: u16,
    /// How many seconds from NOW does the assignment expire?
    pub expires_seconds: u16,
    /// What is the LONGEST time that this seed router will grant Net IDs for?
    pub max_refresh_seconds: u16,
    /// Don't ask to refresh this token until we are < this many seconds from the expiration time
    pub min_refresh_seconds: u16,
    /// The unique token to be used for later refresh requests.
    pub refresh_token: u64,
}

/// An error occurred when assigning a net ID
pub enum SeedAssignmentError {
    /// The current Profile is not a seed router
    ProfileCantSeed,
    /// The Profile is out of Net IDs
    NetIdsExhausted,
    /// The source ID requesting the Net ID is unknown to this seed router
    UnknownSource,
}

/// An error occurred when refreshing a net ID
pub enum SeedRefreshError {
    /// The current Profile is not a seed router
    ProfileCantSeed,
    /// The requested Net ID to be refreshed is unknown by the Seed Router
    UnknownNetId,
    /// The requested Net ID to be refreshed was not assigned as a Seed Router Net ID
    /// (e.g. it is a Direct Connection and does not require refreshing)
    NotAssigned,
    /// The requested Net ID to refresh has already expired
    AlreadyExpired,
    /// The given data did not match the Seed Router table
    BadRequest,
    /// The request to refresh violated the min_refresh_seconds time
    TooSoon,
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
    type InterfaceIdent: Clone + core::fmt::Debug;

    /// Send a serializable message to the Profile.
    ///
    /// This method should only be used for messages that originate locally
    fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError>;

    /// Send a protocol error to the Profile
    ///
    /// Errors may originate locally or remotely
    fn send_err(
        &mut self,
        hdr: &Header,
        err: ProtocolError,
        source: Option<Self::InterfaceIdent>,
    ) -> Result<(), InterfaceSendError>;

    /// Send a pre-serialized message to the Profile.
    ///
    /// This method should only be used for messages that do NOT originate locally
    fn send_raw(
        &mut self,
        hdr: &Header,
        hdr_raw: &[u8],
        data: &[u8],
        source: Self::InterfaceIdent,
    ) -> Result<(), InterfaceSendError>;

    /// Obtain the interface state of the given interface ident
    ///
    /// Returns None if the given ident is unknown by the Profile
    fn interface_state(&mut self, ident: Self::InterfaceIdent) -> Option<InterfaceState>;

    /// Set the state of the given interface ident
    fn set_interface_state(
        &mut self,
        ident: Self::InterfaceIdent,
        state: InterfaceState,
    ) -> Result<(), SetStateError>;

    /// Request a Net ID assignment from this profile
    ///
    /// For Profiles that are not (currently acting as) a Seed Router, this method will always return
    /// an error.
    fn request_seed_net_assign(
        &mut self,
        source_net: u16,
    ) -> Result<SeedNetAssignment, SeedAssignmentError> {
        _ = source_net;
        Err(SeedAssignmentError::ProfileCantSeed)
    }

    /// Request the refresh of a Net ID assignment from this profile
    ///
    /// For Profiles that are not (currently acting as) a Seed Router, this method will always return
    /// an error.
    fn refresh_seed_net_assignment(
        &mut self,
        source_net: u16,
        refresh_net: u16,
        refresh_token: u64,
    ) -> Result<SeedNetAssignment, SeedRefreshError> {
        _ = source_net;
        _ = refresh_net;
        _ = refresh_token;
        Err(SeedRefreshError::ProfileCantSeed)
    }
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
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
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
    /// Interface detected that a packet should be routed back to its source
    RoutingLoop,
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
            InterfaceSendError::RoutingLoop => ProtocolError::ISE_ROUTING_LOOP,
        }
    }
}
