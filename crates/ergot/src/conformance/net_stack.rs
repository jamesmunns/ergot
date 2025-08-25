use mocks::{ExpectedSend, TestNetStack, test_stack};

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

macro_rules! send_testa {
    (   | Header     | Val           | ProRet      | StackRet    |
        | $(-)+      | $(-)+         | $(-)+       | $(-)+       |
      $(| $hdr:ident | $val:literal  | $pret:ident | $sret:ident |)+
    ) => {
        let stack = test_stack();
        let cases: &[(Header, Vec<u8>, Result<(), InterfaceSendError>)] = &[$(
            (
                $hdr(),
                postcard::to_stdvec(&$val).unwrap(),
                $pret(),
            ),
        )+];

        stack.manage_profile(|p| {
            for (hdr, data, retval) in cases.iter() {
                p.add_exp_send(ExpectedSend {
                    hdr: hdr.clone(),
                    data: data.clone(),
                    retval: *retval,
                });
            }
        });

        $({
            let actval = stack.send_ty(&$hdr(), &$val);
            assert_eq!(actval, $sret());
        })+
    };
}

fn default_hdr() -> Header {
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

fn ok<E>() -> Result<(), E> {
    Ok(())
}

fn inoroute() -> Result<(), InterfaceSendError> {
    Err(InterfaceSendError::NoRouteToDest)
}

fn sinoroute() -> Result<(), NetStackSendError> {
    Err(NetStackSendError::InterfaceSend(
        InterfaceSendError::NoRouteToDest,
    ))
}

#[test]
pub fn send_tests_no_sockets() {
    send_testa! {
        | Header        | Val     | ProRet   | StackRet  |
        | -------       | ------- | ------   | --------- |
        // normal send, interface takes
        | default_hdr   | 1234u64 | ok       | ok        |
        // normal send, interface doesn't take
        | default_hdr   | 1234u64 | inoroute | sinoroute |
    };
}
