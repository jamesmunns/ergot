use crate::Header;
use serde::Serialize;

pub mod null;
pub mod std_tcp_client;
pub mod std_tcp_router;
pub mod std_utils;

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
}

pub trait ConstInit {
    const INIT: Self;
}

// An interface send is very similar to a socket send, with the exception
// that interface sends are ALWAYS a serializing operation (or requires
// serialization has already been done), which means we don't need to
// differentiate between "send owned" and "send borrowed". The exception
// to this is "send raw", where serialization has already been done, e.g.
// if we are routing a packet.
pub trait InterfaceManager {
    fn send<T: Serialize>(&mut self, hdr: Header, data: &T) -> Result<(), InterfaceSendError>;

    fn send_raw(&mut self, hdr: Header, data: &[u8]) -> Result<(), InterfaceSendError>;
}
