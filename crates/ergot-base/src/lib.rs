#![doc = include_str!("../README.md")]
//!
//! # Technical Overview
//!
//! Ergot is centered around the [`NetStack`]. The `NetStack` is designed to be
//! a `static`, living for the duration of the firmware/program.
//!
//! ## The `NetStack`
//!
//! The `NetStack` is the primary interface for *sending* messages, as well as
//! adding new sockets and interfaces.
//!
//! The `NetStack` contains two main items:
//!
//! * A list of local sockets
//! * An Interface Manager, responsible for holding any interfaces the
//!   `NetStack` may use.
//!
//! ### One Main "Trick"
//!
//! In general, whenever *multiple* items need to be stored in the `NetStack`,
//! they should be stored *intrusively*, or as elements in an intrusively linked
//! list. This allows devices without a heap allocator to effectively handle
//! a variable number of items.
//!
//! Ergot heavily leverages a trick to allow ephemeral items (that may reside
//! on the stack) to be safely added to a static linked list: It requires that
//! items added to intrusive lists are [pinned], and that when the pinned items
//! are dropped, they MUST be removed from the list prior to dropping. This
//! guarantee is backed by a [`BlockingMutex`], which MUST be held whenever
//! interacting with the items of a linked list, including in the local context
//! where the items are defined, and especially including the `Drop` impl of
//! those items.
//!
//! For single core microcontrollers, this has little impact: the mutex is held
//! whenever access to the stack occurs, and the mutex may not be held across
//! an await point. For larger system, this may lead to some non-ideal
//! contention across parallel threads, however it is intended that this mutex
//! is for as short of a time as possible.
//!
//! [`BlockingMutex`]: mutex::BlockingMutex
//! [pinned]: https://doc.rust-lang.org/std/pin/
//!
//! ## The "Sockets"
//!
//! Ergot is oriented around type-safe sockets. Rather than TCP/IP sockets,
//! which provide users with either streams or frames of bytes (e.g. `[u8]`),
//! Ergot sockets are always of a certain Rust data type, such as structs or
//! enums. They provide an API very similar to "channels", a common way of
//! passing data around within Rust programs.
//!
//! Ergot leverages the techniques of `postcard`, `postcard-rpc`, and
//! `postcard-schema` to define types in terms of their serialization schema,
//! and an 8-byte hash of a type's schema is used as a key to identify that
//! socket's type.
//!
//! When messages are received from outside of the current application/firmware,
//! messages are deserialized using the `postcard` serialization format, a
//! compact, non-self-describing, binary format.
//!
//! When messages are sent locally within a device, no serialization or
//! deserialization occurs, meaning that fundamentally sending data to an
//! Ergot socket locally has no cost over using a normal channel.
//!
//! In general: Sockets **receive**, and the NetStack **sends**.
//!
//! ### Non-stateful sockets
//!
//! Currently, sockets in Ergot are not stateful, meaning that they only serve
//! to receive messages. Replies may be made by sending a response to the
//! source address of the received message, using the [`NetStack`] to send
//! the response.
//!
//! Conceptually, this makes Ergot sockets similar to UDP sockets: delivery
//! is not guaranteed.
//!
//! ### A variety of sockets
//!
//! Ergot allows for different implementations of what a "socket" is, with
//! a common subset of functionality. Normally, this might sound like just the
//! problem to solve with a Rust `trait`, however as we would like to store
//! all of these items in a single intrusive linked list, this becomes
//! problematic.
//!
//! Instead, the pinned sockets all feature a common socket header, which
//! includes a hand-crafted vtable used to interact with the socket. This allows
//! us to have the moral equivalent to `List<dyn Socket>`, but in a way that
//! is easier to support on embedded devices without an allocator.
//!
//! This indirection allows us to be flexible both in intent of a socket, for
//! example a socket that expects a single one-shot response, or a socket that
//! expects a stream of requests; as well as flexible in the means of storage
//! of a socket, for example using stackful bounded queues of message on
//! embedded systems, or heapful unbounded queues of messages on systems with
//! an allocator.
//!
//! This approach of using a linked list, common header, and vtable, is NEARLY
//! IDENTICAL to how most async executors operate in Rust, particularly how
//! Tasks containing differently-typed Futures are handled by the executor
//! itself when it comes to polling or dropping a Task.
//!
//! ### Socket Flavors
//!
//! Currently, Ergot aims to support three main kinds of sockets, derived from
//! `postcard-rpc`'s messaging model:
//!
//! * Endpoint Request sockets
//!   * Used by a "service" to accept incoming requests
//!   * The "service" will respond with a specific Response type
//!   * Typically a continuous stream of Requests are expected
//! * Endpoint Response sockets
//!   * Used by a "client" to accept a response to an outgoing request
//!   * Typically receives a single one-shot Response
//! * Topic-in sockets
//!   * Still in flux, but aims to receive a stream of incoming Topic messages
//!   * This is the "sub" part of "pub-sub"
//!
//! ## Addressing
//!
//! Addresses in Ergot have three main components:
//!
//! * A 16-bit **Network ID**
//! * An 8-bit **Node ID**
//! * An 8-bit **Socket ID**
//!
//! This addressing is similar in form to AppleTalk's addressing. This
//! addressing is quite different to how TCP/IP IPv4 addressing works.
//!
//! ### Network IDs
//!
//! Network IDs represent a single "network segment", where all nodes of a
//! network segment can hear all messages sent on that segment.
//!
//! For example, in a point-to-point link (e.g. UART, TCP, USB), that link will
//! be a single Network ID, containing two nodes. In a bus-style link
//! (e.g. RS-485, I2C), the bus will be a single Network ID, with one or more
//! nodes residing on that link.
//!
//! The Network ID of "0" is reserved, and is generally used when sending
//! messages within the local device, or used to mean "the current network
//! segment" before an interface has discovered the Network ID of the Network
//! Segment it resides on.
//!
//! The Network ID of "65535" is reserved.
//!
//! Network IDs are intended to be discovered/negotiated at runtime, and are
//! not typically hardcoded. The general process of negotiating Network IDs,
//! particularly across multiple network segment hops, is not yet defined.
//!
//! Networks that require more than 65534 network segments are not supported
//! by Ergot. At that point, you should probably just use IPv4/v6.
//!
//! ### Node IDs
//!
//! Node IDs represent a single entity on a network segment.
//!
//! The Node ID of "0" is reserved, and is generally used when sending messages
//! within the local device.
//!
//! The Node ID of "255" is reserved.
//!
//! Network segments that require more than 254 nodes are not supported by
//! Ergot.
//!
//! Network IDs are intended to be discovered/negotiated at runtime, and are
//! not typically hardcoded. One exception to this is for known point-to-point
//! network segments that have a defined "controller" and "target" role, such
//! as USB (where the "host" is the "controller", and the "device" is the
//! "target"). In these cases, the "controller" typically hardcodes the Node ID
//! of "1", and the "target" hardcodes the Node ID of "2". This is done to
//! reduce complexity on these interface implementations.
//!
//! ### Socket IDs
//!
//! Socket IDs represent a single receiving socket within a [`NetStack`].
//!
//! The Socket ID of "0" is reserved, and is generally used as a "wildcard"
//! when sending messages to a device.
//!
//! The Socket ID of "255" is reserved.
//!
//! Systems that require more than 254 active sockets are not supported by
//! Ergot.
//!
//! Socket IDs are assigned dynamically by the [`NetStack`], and are never
//! intended to be hardcoded. Socket IDs may be recycled over time.
//!
//! If a device has multiple interfaces, and therefore has multiple (Network ID,
//! Node ID) tuples that refer to it, the same Socket ID is used on all
//! interfaces.
//!
//! ### Form on the wire
//!
//! When serialized into a packet, addresses are encoded with Network ID as the
//! most significant bytes, and the socket ID as the least significant bytes,
//! and then varint encoded. This means that in many cases, where "0" is used
//! for the Network or Node ID, or low numbers are used, Addresses can be
//! encoded in fewer than 4 bytes on the wire.
//!
//! For this reason, when negotiating any ID, lower numbers should be preferred
//! when possible. Addresses are only encoded as larger than 4 bytes when
//! addressing a network ID >= 4096.
//!
//! ## The Interface Manager
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

pub mod address;
pub mod interface_manager;
pub mod net_stack;
pub mod socket;
pub mod well_known;

pub use address::Address;
pub use net_stack::{NetStack, NetStackSendError};
use postcard_rpc::Key;
use socket::SocketTy;

#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum FrameKind {
    EndpointRequest,
    EndpointResponse,
    Topic,
}

impl FrameKind {
    pub fn from_wire(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::EndpointRequest),
            1 => Some(Self::EndpointResponse),
            2 => Some(Self::Topic),
            _ => None,
        }
    }

    pub fn to_wire(self) -> u8 {
        match self {
            FrameKind::EndpointRequest => 0,
            FrameKind::EndpointResponse => 1,
            FrameKind::Topic => 2,
        }
    }

    pub fn matches(&self, other: &SocketTy) -> bool {
        #[allow(clippy::match_like_matches_macro)]
        match (self, other) {
            (FrameKind::EndpointRequest, SocketTy::EndpointReq(_)) => true,
            (FrameKind::EndpointResponse, SocketTy::EndpointResp(_)) => true,
            (FrameKind::Topic, SocketTy::TopicIn(_)) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Header {
    pub src: Address,
    pub dst: Address,
    pub key: Option<Key>,
    pub seq_no: Option<u16>,
    pub kind: FrameKind,
}

#[derive(Debug, Clone)]
pub struct HeaderSeq {
    pub src: Address,
    pub dst: Address,
    pub key: Option<Key>,
    pub seq_no: u16,
    pub kind: FrameKind,
}

impl Header {
    #[inline]
    pub fn to_headerseq_or_with_seq<F: FnOnce() -> u16>(&self, f: F) -> HeaderSeq {
        HeaderSeq {
            src: self.src,
            dst: self.dst,
            key: self.key,
            seq_no: self.seq_no.unwrap_or_else(f),
            kind: self.kind,
        }
    }
}

impl From<HeaderSeq> for Header {
    fn from(val: HeaderSeq) -> Self {
        Self {
            src: val.src,
            dst: val.dst,
            key: val.key,
            seq_no: Some(val.seq_no),
            kind: val.kind,
        }
    }
}
