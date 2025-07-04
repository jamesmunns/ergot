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

use crate::{Header, HeaderSeq, ProtocolError};
use serde::Serialize;

pub mod cobs_stream;
pub mod framed_stream;
pub mod null;

#[cfg(feature = "embassy-usb-v0_4")]
pub mod eusb_0_4_client;

#[cfg(feature = "nusb-v0_1")]
pub mod nusb_0_1_router;

#[cfg(feature = "std")]
pub mod std_tcp_client;
#[cfg(feature = "std")]
pub mod std_tcp_router;
#[cfg(feature = "std")]
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
    /// TTL has reached the terminal value
    TtlExpired,
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
    fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError>;
    fn send_err(&mut self, hdr: &Header, err: ProtocolError) -> Result<(), InterfaceSendError>;
    fn send_raw(&mut self, hdr: &Header, data: &[u8]) -> Result<(), InterfaceSendError>;
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

pub mod wire_frames {
    use log::warn;
    use postcard::{Serializer, ser_flavors};
    use serde::{Deserialize, Serialize};

    use crate::{Address, FrameKind, HeaderSeq, Key, ProtocolError};

    use super::BorrowedFrame;

    #[derive(Serialize, Deserialize, Debug)]
    pub struct CommonHeader {
        pub src: u32,
        pub dst: u32,
        pub seq_no: u16,
        pub kind: u8,
        pub ttl: u8,
    }

    pub enum PartialDecodeTail<'a> {
        Specific(&'a [u8]),
        AnyAll { key: Key, body: &'a [u8] },
        Err(ProtocolError),
    }

    pub struct PartialDecode<'a> {
        pub hdr: CommonHeader,
        pub tail: PartialDecodeTail<'a>,
    }

    pub(crate) fn decode_frame_partial(data: &[u8]) -> Option<PartialDecode<'_>> {
        let (common, remain) = postcard::take_from_bytes::<CommonHeader>(data).ok()?;
        let is_err = common.kind == FrameKind::PROTOCOL_ERROR.0;
        let any_all = [0, 255].contains(&Address::from_word(common.dst).port_id);

        match (is_err, any_all) {
            // Not allowed: any/all AND is err
            (true, true) => {
                warn!("Rejecting any/all protocol error message");
                None
            }
            (true, false) => {
                // err
                let (err, remain) = postcard::take_from_bytes::<ProtocolError>(remain).ok()?;
                if !remain.is_empty() {
                    warn!("Excess data, rejecting");
                    return None;
                }
                Some(PartialDecode {
                    hdr: common,
                    tail: PartialDecodeTail::Err(err),
                })
            }
            (false, true) => {
                let (key, remain) = postcard::take_from_bytes::<Key>(remain).ok()?;
                Some(PartialDecode {
                    hdr: common,
                    tail: PartialDecodeTail::AnyAll { key, body: remain },
                })
            }
            (false, false) => Some(PartialDecode {
                hdr: common,
                tail: PartialDecodeTail::Specific(remain),
            }),
        }
    }

    // must not be error
    // doesn't check if dest is actually any/all
    pub(crate) fn encode_frame_ty<F, T>(
        flav: F,
        hdr: &CommonHeader,
        key: Option<&Key>,
        body: &T,
    ) -> Result<F::Output, ()>
    where
        F: ser_flavors::Flavor,
        T: Serialize,
    {
        let mut serializer = Serializer { output: flav };
        hdr.serialize(&mut serializer).map_err(drop)?;

        if let Some(key) = key {
            serializer.output.try_extend(&key.0).map_err(drop)?;
        }

        body.serialize(&mut serializer).map_err(drop)?;
        serializer.output.finalize().map_err(drop)
    }

    // must not be error
    // doesn't check if dest is actually any/all
    pub(crate) fn encode_frame_raw<F>(
        flav: F,
        hdr: &CommonHeader,
        key: Option<&Key>,
        body: &[u8],
    ) -> Result<F::Output, ()>
    where
        F: ser_flavors::Flavor,
    {
        let mut serializer = Serializer { output: flav };
        hdr.serialize(&mut serializer).map_err(drop)?;

        if let Some(key) = key {
            serializer.output.try_extend(&key.0).map_err(drop)?;
        }

        serializer.output.try_extend(body).map_err(drop)?;
        serializer.output.finalize().map_err(drop)
    }

    pub(crate) fn encode_frame_err<F>(
        flav: F,
        hdr: &CommonHeader,
        err: ProtocolError,
    ) -> Result<F::Output, ()>
    where
        F: ser_flavors::Flavor,
    {
        let mut serializer = Serializer { output: flav };
        hdr.serialize(&mut serializer).map_err(drop)?;
        err.serialize(&mut serializer).map_err(drop)?;
        serializer.output.finalize().map_err(drop)
    }

    #[allow(dead_code)]
    pub(crate) fn de_frame(remain: &[u8]) -> Option<BorrowedFrame<'_>> {
        let res = decode_frame_partial(remain)?;

        let key;
        let body = match res.tail {
            PartialDecodeTail::Specific(body) => {
                key = None;
                Ok(body)
            }
            PartialDecodeTail::AnyAll { key: skey, body } => {
                key = Some(skey);
                Ok(body)
            }
            PartialDecodeTail::Err(protocol_error) => {
                key = None;
                Err(protocol_error)
            }
        };

        let CommonHeader {
            src,
            dst,
            seq_no,
            kind,
            ttl,
        } = res.hdr;

        Some(BorrowedFrame {
            hdr: HeaderSeq {
                src: Address::from_word(src),
                dst: Address::from_word(dst),
                seq_no,
                key,
                kind: FrameKind(kind),
                ttl,
            },
            body,
        })
    }
}

#[allow(dead_code)]
pub(crate) struct BorrowedFrame<'a> {
    pub(crate) hdr: HeaderSeq,
    pub(crate) body: Result<&'a [u8], ProtocolError>,
}
