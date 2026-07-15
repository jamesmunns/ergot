//! # Net Stack Conformance
//!
//! ## Non-Error Sends
//!
//! ```text
//!    ┌────────────────┐
//! ┌ ─│ Non-Error Send ├ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─
//!    └────────────────┘               ┌────────────┐                     │
//! │                                   │    dest    │
//!                                     │ broadcast? │                     │
//! │                                   │ (*:*.255)  │
//!                            unicast  ├────────────┤  broadcast          │
//! │                          ┌────────┘            └────────┐
//!                            │                              │            │
//! │                    ┌─────▼─────┐                  ┌─────▼─────┐
//!                      │ dest addr │ is 0.0:*         │ Offer to  │      │
//! │                    │  local?   ├────────┐         │  Sockets  │
//!                      │  (0:0.*)  │        │         │(find all) │      │
//! │┌─────────┐ Profile └─────┬─────┘        │         └─────┬─────┘
//!  │ Success │ Accepts       │ not 0.0:*    │               │            │
//! ││ (Done)  ◀─────────┬─────▼─────┐        │         ┌─────▼─────┐
//!  └─────────┘         │ Offer to  │        │         │ Offer to  │      │
//! │┌─────────┐ Profile │  Profile  │        │         │  Profile  │
//!  │  Error  │ Rejects │(find one) │        │         │(find all) │      │
//! ││ (Done)  ◀─────────┴─────┬─────┘        │         ├───────────┤
//!  └─────────┘ dest is local │              │         │           │      │
//! │                    ┌─────▼─────┐        │         │           │
//!                      │ Offer to  │        │    sockets OR   sockets AND│
//! │                    │  Sockets  │◀───────┘     profile       profile
//!                      │(find one) │              Accepted     Rejected  │
//! │                    ├───────────┤                  │           │
//!               Socket │           │  Socket          │           │      │
//! │            Accepts │           │ Rejects          │           │
//!                 ┌────▼────┐ ┌────▼────┐        ┌────▼────┐ ┌────▼────┐ │
//! │               │ Success │ │  Error  │        │ Success │ │  Error  │
//!                 │ (Done)  │ │ (Done)  │        │ (Done)  │ │ (Done)  │ │
//! │               └─────────┘ └─────────┘        └─────────┘ └─────────┘
//!  ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ┘
//! ```
//!
//! The Net Stack SHALL behave as follows for sending non-protocol-error messages:
//!
//! ### Evaluate destination port
//!
//! The header's destination port field SHALL be inspected to see whether it is
//! a broadcast message (the port is `255`) or a unicast message (the port is NOT
//! `255`).
//!
//! #### Unicast Messages
//!
//! The header's destination `net_id` and `node_id` fields SHALL be inspected to see
//! whether the destination is `0:0.*`.
//!
//! If the destination is NOT `0:0.*`, then the message SHALL be offered to the Profile
//! for sending.
//!
//! If the destination IS `0:0.*`, then the message SHALL NOT be offered to the Profile
//! for sending, and the message SHALL be offered to the Sockets for sending.
//!
//! ##### Profile Sending
//!
//! If the unicast message is offered to the Profile, the result of the Profile's send
//! SHALL be used as follows:
//!
//! * If the Profile reports a successful send, then the net stack SHALL NOT offer the
//!   message to the local sockets, and SHALL return a success.
//! * If the Profile reports a "Destination Local" error, e.g. the address is NOT `0:0.*`,
//!   but DOES match an address assigned to one of the interfaces of the Profile, then the
//!   message SHALL be offered to the Sockets for sending.
//! * If the Profile reports any other error, then the net stack SHALL NOT offer the
//!   message to the local sockets, and SHALL return the error.
//!
//! ##### Socket Sending
//!
//! If the unicast message is offered to the Sockets, the header's destination port field
//! SHALL be used as follows:
//!
//! * If the destination port is NOT `0`, the Sockets will be searched for a socket with
//!   exactly the given port.
//!     * If a socket with the matching port is found, the Net Stack SHALL offer the message
//!       to the socket.
//!     * If no socket is found, the Net Stack SHALL return a "No Route to Destination" error.
//! * If the destination port IS `0`, the Sockets SHALL be searched for a socket with matching
//!   metadata.
//!    * If the message DOES NOT include the Any/All appendix, the Net Stack SHALL return
//!      an "Any Port Missing Key" error.
//!    * If no socket is found, the Net Stack SHALL return a "No Route to Destination" error.
//!    * If a matching socket is found, the Net Stack SHALL off the message to the socket.
//!
//! If a matching Socket is found, the Net Stack SHALL return the result of the socket send.
//!
//! #### Broadcast Messages
//!
//! A broadcast (destination port `255`) is addressed to *everyone*, not to a
//! single destination, and is delivered best-effort: it is offered to all
//! matching local sockets (find all) AND offered to the Profile to be flooded
//! outward on all interfaces. See the book's
//! [Delivery and Reliability](crate::book::_04_delivery_and_reliability) chapter
//! for the model.
//!
//! * If the message DOES NOT include the Any/All appendix, the Net Stack SHALL
//!   return an "All Port Missing Key" error.
//! * If at least one local socket matched the broadcast OR the Profile accepts
//!   the message, the Net Stack SHALL return success. A matched local socket
//!   whose bounded queue is full still counts as a recipient — the audience
//!   exists and the message is best-effort dropped for it (at-most-once), which
//!   is NOT a "no route" condition.
//! * A Profile result of "No Route to Destination" or "Routing Loop" SHALL be
//!   treated as *no external recipient* — a successful best-effort no-op, NOT a
//!   delivery error. Because a broadcast has no single destination, "nobody is
//!   listening" is an expected outcome under at-most-once delivery, not a
//!   failure. (This is why the flowchart's "profile Accepted" branch covers the
//!   no-route case for broadcasts.)
//! * If there is no local recipient AND the Profile reports a genuine delivery
//!   failure to an interface that exists (e.g. its outgoing queue is full), the
//!   Net Stack SHALL return a "No Route" error.
#![cfg_attr(not(test), allow(dead_code, unused_imports, unused_macros))]

use mocks::{ExpectedSend, test_stack};

use crate::{
    Address, AnyAllAppendix, DEFAULT_TTL, FrameKind, Header, Key, NetStackSendError, ProtocolError,
    interface_manager::InterfaceSendError,
};

pub mod mocks {
    use std::collections::VecDeque;

    use mutex::raw_impls::cs::CriticalSectionRawMutex;

    use crate::{
        Header, HeaderSeq, ProtocolError,
        interface_manager::{InterfaceSendError, InterfaceState, Profile, SetStateError},
        net_stack::ArcNetStack,
    };

    pub type TestNetStack = ArcNetStack<CriticalSectionRawMutex, MockProfile>;
    pub fn test_stack() -> TestNetStack {
        ArcNetStack::new_with_profile(MockProfile::default())
    }

    pub struct ExpectedSend {
        pub hdr: Header,
        pub data: Vec<u8>,
        pub retval: Result<(), InterfaceSendError>,
    }

    pub struct ExpectedSendErr {
        pub hdr: Header,
        pub err: ProtocolError,
        pub retval: Result<(), InterfaceSendError>,
    }

    pub struct ExpectedSendRaw {
        pub hdr: HeaderSeq,
        pub body: Vec<u8>,
        pub retval: Result<(), InterfaceSendError>,
    }

    #[derive(Default)]
    pub struct MockProfile {
        pub expected_sends: VecDeque<ExpectedSend>,
        pub expected_send_errs: VecDeque<ExpectedSendErr>,
        pub expected_send_raws: VecDeque<ExpectedSendRaw>,
    }

    impl MockProfile {
        pub fn add_exp_send(&mut self, exp: ExpectedSend) {
            self.expected_sends.push_back(exp);
        }

        pub fn add_exp_send_err(&mut self, exp: ExpectedSendErr) {
            self.expected_send_errs.push_back(exp);
        }

        pub fn add_exp_send_raw(&mut self, exp: ExpectedSendRaw) {
            self.expected_send_raws.push_back(exp);
        }

        pub fn assert_all_empty(&self) {
            assert!(self.expected_sends.is_empty());
            assert!(self.expected_send_errs.is_empty());
            assert!(self.expected_send_raws.is_empty());
        }
    }

    impl Profile for MockProfile {
        type InterfaceIdent = u64;

        fn send<T: serde::Serialize>(
            &mut self,
            hdr: &Header,
            data: &T,
        ) -> Result<(), InterfaceSendError> {
            let data = postcard::to_stdvec(data).expect("Serializing send failed");
            log::trace!("{}: Sending data:{:02X?}", hdr, data);
            let now = self.expected_sends.pop_front().expect("Unexpected send");
            assert_eq!(&now.hdr, hdr, "Send header mismatch");
            assert_eq!(&now.data, &data, "Send data mismatch");
            now.retval
        }

        fn send_err(
            &mut self,
            _hdr: &Header,
            _err: ProtocolError,
            _source: Option<Self::InterfaceIdent>,
        ) -> Result<(), InterfaceSendError> {
            todo!()
        }

        fn send_raw(
            &mut self,
            _hdr: &HeaderSeq,
            _data: &[u8],
            _source: Self::InterfaceIdent,
        ) -> Result<(), InterfaceSendError> {
            todo!()
        }

        fn interface_state(&mut self, _ident: Self::InterfaceIdent) -> Option<InterfaceState> {
            todo!()
        }

        fn set_interface_state(
            &mut self,
            _ident: Self::InterfaceIdent,
            _state: InterfaceState,
        ) -> Result<(), SetStateError> {
            todo!()
        }
    }
}

/// Macro for generating test cases where:
///
/// * There are no routes
/// * A single `send_ty` is called
macro_rules! send_testa {
    (   | Case          | Header     | Val           | ProfileReturns   | StackReturns  |
        | $(-)+         | $(-)+      | $(-)+         | $(-)+            | $(-)+         |
      $(| $case:ident   | $hdr:ident | $val:literal  | $pret:ident      | $sret:ident   |)+
    ) => {
        $(
        #[test]
            fn $case() {
                let stack = test_stack();

                stack.manage_profile(|p| {
                    p.add_exp_send(ExpectedSend {
                        hdr: $hdr(),
                        data: postcard::to_stdvec(&$val).unwrap(),
                        retval: $pret(),
                    });
                });

                let actval = stack.send_ty(&$hdr(), &$val);
                assert_eq!(actval, $sret());

                stack.manage_profile(|p| {
                    p.assert_all_empty();
                });
            }
        )+
    };
}

fn unicast_specific_port() -> Header {
    Header {
        src: Address::unknown(),
        dst: Address {
            network_id: 10,
            node_id: 10,
            port_id: 10,
        },
        any_all: None,
        seq_no: None,
        kind: FrameKind::RESERVED,
        ttl: DEFAULT_TTL,
    }
}

/// A broadcast header (`*:*.255`) carrying the Any/All appendix that broadcast
/// delivery requires.
fn broadcast_hdr() -> Header {
    Header {
        src: Address::unknown(),
        dst: Address {
            network_id: 10,
            node_id: 10,
            port_id: 255,
        },
        any_all: Some(AnyAllAppendix {
            key: Key(*b"TESTTEST"),
            nash: None,
        }),
        seq_no: None,
        kind: FrameKind::TOPIC_MSG,
        ttl: DEFAULT_TTL,
    }
}

/// Returns an Ok(())
fn ok<E>() -> Result<(), E> {
    Ok(())
}

/// Interface returns this error
fn interface_err(err: InterfaceSendError) -> Result<(), InterfaceSendError> {
    Err(err)
}

/// Stack returns this interface error
fn stack_interface_err(err: InterfaceSendError) -> Result<(), NetStackSendError> {
    Err(NetStackSendError::InterfaceSend(err))
}

fn stack_err(err: NetStackSendError) -> Result<(), NetStackSendError> {
    Err(err)
}

/// Interface reports no route
fn inoroute() -> Result<(), InterfaceSendError> {
    interface_err(InterfaceSendError::NoRouteToDest)
}

/// Netstack reports the Interface reports no route
fn sinoroute() -> Result<(), NetStackSendError> {
    stack_interface_err(InterfaceSendError::NoRouteToDest)
}

/// Interface reports full
fn ifull() -> Result<(), InterfaceSendError> {
    interface_err(InterfaceSendError::InterfaceFull)
}

/// Stack reports interface reports full
fn sifull() -> Result<(), NetStackSendError> {
    stack_interface_err(InterfaceSendError::InterfaceFull)
}

/// Interface reports address is local
fn ilocal() -> Result<(), InterfaceSendError> {
    interface_err(InterfaceSendError::DestinationLocal)
}

/// Stack reports no route (NOT from the interface)
fn snoroute() -> Result<(), NetStackSendError> {
    stack_err(NetStackSendError::NoRoute)
}

/// Interface reports a routing loop (a broadcast whose only route is back to its
/// source — i.e. no external recipient).
fn iroutingloop() -> Result<(), InterfaceSendError> {
    interface_err(InterfaceSendError::RoutingLoop)
}

send_testa! {
    | Case                          | Header                | Val     | ProfileReturns  | StackReturns  |
    | ----                          | ------                | ---     | --------------  | ------------  |
    | no_sockets_interface_takes    | unicast_specific_port | 1234u64 | ok              | ok            |
    | no_sockets_no_iroute          | unicast_specific_port | 1234u64 | inoroute        | sinoroute     |
    | no_sockets_interface_full     | unicast_specific_port | 1234u64 | ifull           | sifull        |
    | no_sockets_interface_local    | unicast_specific_port | 1234u64 | ilocal          | snoroute      |
}

// Broadcast (`*:*.255`) has no single destination: it is best-effort to all
// current recipients. "No route to dest" and "routing loop" both mean "no
// external recipient", which is a successful no-op — not a delivery error.
// A genuine failure to an *existing* interface (e.g. `InterfaceFull`) still
// surfaces as an error. (See the book's delivery-model chapter.)
send_testa! {
    | Case                           | Header        | Val     | ProfileReturns  | StackReturns  |
    | ----                           | ------        | ---     | --------------  | ------------  |
    | bcast_interface_takes          | broadcast_hdr | 1234u64 | ok              | ok            |
    | bcast_no_audience_no_iroute    | broadcast_hdr | 1234u64 | inoroute        | ok            |
    | bcast_no_audience_routing_loop | broadcast_hdr | 1234u64 | iroutingloop    | ok            |
    | bcast_genuine_failure_errors   | broadcast_hdr | 1234u64 | ifull           | snoroute      |
}

/// Regression: a protocol-error send addressed to a
/// broadcast/reserved destination port (`*:*.255`) MUST NOT panic.
///
/// This is remotely reachable via the router's `PacketTooBig` reply path
/// (`profiles::router::process_frame`): a received frame whose *source* port is a
/// reserved port (0 or 255) becomes the error reply's *destination* port, and the
/// reply is handed to `NetStack::send_err`. An error cannot be unicast to a
/// broadcast port, so this SHALL return a delivery error rather than crash.
#[test]
fn send_err_to_broadcast_port_does_not_panic() {
    let stack = test_stack();
    let hdr = Header {
        src: Address::unknown(),
        dst: Address {
            network_id: 10,
            node_id: 10,
            port_id: 255,
        },
        any_all: None,
        seq_no: None,
        kind: FrameKind::PROTOCOL_ERROR,
        ttl: DEFAULT_TTL,
    };
    let res = stack.send_err(&hdr, ProtocolError::IseNoRouteToDest, None);
    assert_eq!(res, Err(NetStackSendError::NoRoute));
    stack.manage_profile(|p| p.assert_all_empty());
}

/// A protocol-error frame handed to a normal (non-error) send path must be
/// rejected, not panic: error frames belong on the `send_err` path.
#[test]
fn send_ty_with_protocol_error_kind_does_not_panic() {
    let stack = test_stack();
    let hdr = Header {
        src: Address::unknown(),
        dst: Address {
            network_id: 10,
            node_id: 10,
            port_id: 10,
        },
        any_all: None,
        seq_no: None,
        kind: FrameKind::PROTOCOL_ERROR,
        ttl: DEFAULT_TTL,
    };
    let res = stack.send_ty::<u64>(&hdr, &1234);
    assert_eq!(res, Err(NetStackSendError::NoRoute));
    stack.manage_profile(|p| p.assert_all_empty());
}
