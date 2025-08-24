use mocks::{test_stack, ExpectedSend, TestNetStack};

use crate::{interface_manager::InterfaceSendError, Address, FrameKind, Header, NetStackSendError, DEFAULT_TTL};

pub mod mocks {
    use std::collections::VecDeque;

    use mutex::raw_impls::cs::CriticalSectionRawMutex;

    use crate::{
        interface_manager::{InterfaceSendError, InterfaceState, Profile, SetStateError}, net_stack::ArcNetStack, Header, ProtocolError
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

type Steppa<'a> = &'a [(Result<(), NetStackSendError>, Box<dyn Fn(TestNetStack) -> Result<(), NetStackSendError> + 'a>)];

#[test]
pub fn first() {
    let stack = test_stack();

    let cases: &[(Header, Vec<u8>, Result<(), InterfaceSendError>)] = &[
        (
            Header {
                src: Address::unknown(),
                dst: Address { network_id: 10, node_id: 10, port_id: 10 },
                any_all: None,
                seq_no: None,
                kind: FrameKind::ENDPOINT_REQ,
                ttl: DEFAULT_TTL,
            },
            postcard::to_stdvec(&1234u64).unwrap(),
            Ok(()),
        ),
    ];
    let steps: Steppa<'_> = &[
        (Ok(()), Box::new(|stack| {
            stack.send_ty(&cases[0].0, &1234u64)
        })),
    ];



    stack.manage_profile(|p| {
        for (hdr, data, retval) in cases.iter() {
            p.add_exp_send(ExpectedSend {
                hdr: hdr.clone(),
                data: data.clone(),
                retval: *retval,
            });
        }
    });

    for (res, func) in steps.iter() {
        let actval = func(stack.clone());
        assert_eq!(*res, actval);
    }
}
