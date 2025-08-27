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
#![cfg_attr(not(test), allow(dead_code, unused_imports, unused_macros))]

use mocks::{ExpectedSend, test_stack};

use crate::{
    Address, DEFAULT_TTL, FrameKind, Header, NetStackSendError,
    interface_manager::InterfaceSendError,
};

pub mod mocks {
    use std::collections::VecDeque;

    use mutex::raw_impls::cs::CriticalSectionRawMutex;

    use crate::{
        Header, ProtocolError,
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
        pub hdr: Header,
        pub hdr_raw: Vec<u8>,
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
            log::trace!("Sending hdr:{hdr:?}, data:{data:02X?}");
            let now = self.expected_sends.pop_front().expect("Unexpected send");
            assert_eq!(&now.hdr, hdr, "Send header mismatch");
            assert_eq!(&now.data, &data, "Send data mismatch");
            now.retval
        }

        fn send_err(
            &mut self,
            _hdr: &Header,
            _err: ProtocolError,
        ) -> Result<(), InterfaceSendError> {
            todo!()
        }

        fn send_raw(
            &mut self,
            _hdr: &Header,
            _hdr_raw: &[u8],
            _data: &[u8],
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

send_testa! {
    | Case                          | Header                | Val     | ProfileReturns  | StackReturns  |
    | ----                          | ------                | ---     | --------------  | ------------  |
    | no_sockets_interface_takes    | unicast_specific_port | 1234u64 | ok              | ok            |
    | no_sockets_no_iroute          | unicast_specific_port | 1234u64 | inoroute        | sinoroute     |
    | no_sockets_interface_full     | unicast_specific_port | 1234u64 | ifull           | sifull        |
    | no_sockets_interface_local    | unicast_specific_port | 1234u64 | ilocal          | snoroute      |
}
