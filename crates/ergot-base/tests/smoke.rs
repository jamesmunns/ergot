use std::{pin::pin, time::Duration};

use ergot_base::{
    Address, DEFAULT_TTL, FrameKind, Header, Key, NetStack,
    interface_manager::null::NullInterfaceManager, socket::owned::OwnedSocket,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use serde::{Deserialize, Serialize};
use tokio::{spawn, time::sleep};

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Example {
    a: u8,
    b: u32,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Other {
    a: u64,
    b: i32,
}

type TestNetStack = NetStack<CriticalSectionRawMutex, NullInterfaceManager>;

#[tokio::test]
async fn hello() {
    static STACK: TestNetStack = NetStack::new();
    let src = Address {
        network_id: 0,
        node_id: 0,
        port_id: 123,
    };
    let dst = Address {
        network_id: 0,
        node_id: 0,
        port_id: 0,
    };

    {
        let socket =
            OwnedSocket::<Example, _, _>::new(&STACK, Key(*b"TEST1234"), FrameKind::ENDPOINT_REQ);
        let mut socket = pin!(socket);
        let mut hdl = socket.as_mut().attach();

        let tsk = spawn(async move {
            sleep(Duration::from_millis(100)).await;

            // try sending, should fail
            STACK
                .send_ty::<Other>(
                    &Header {
                        src,
                        dst,
                        key: Some(Key(*b"1234TEST")),
                        seq_no: None,
                        kind: FrameKind::ENDPOINT_REQ,
                        ttl: DEFAULT_TTL,
                    },
                    &Other { a: 345, b: -123 },
                )
                .unwrap_err();
            // typed sending works
            STACK
                .send_ty::<Example>(
                    &Header {
                        src,
                        dst,
                        key: Some(Key(*b"TEST1234")),
                        seq_no: None,
                        kind: FrameKind::ENDPOINT_REQ,
                        ttl: DEFAULT_TTL,
                    },
                    &Example { a: 42, b: 789 },
                )
                .unwrap();
            // raw sending works
            // (todo: wait a bit to free up space, we wont need this when we can
            // hold more than one message at a time)
            sleep(Duration::from_millis(100)).await;
            let body = postcard::to_stdvec(&Example { a: 56, b: 1234 }).unwrap();
            STACK
                .send_raw(
                    &Header {
                        src,
                        dst,
                        key: Some(Key(*b"TEST1234")),
                        seq_no: None,
                        kind: FrameKind::ENDPOINT_REQ,
                        ttl: DEFAULT_TTL,
                    },
                    &body,
                )
                .unwrap();
        });

        let msg = hdl.recv().await;
        assert_eq!(
            Address {
                network_id: 0,
                node_id: 0,
                port_id: 123
            },
            msg.hdr.src
        );
        assert_eq!(
            Address {
                network_id: 0,
                node_id: 0,
                port_id: 0
            },
            msg.hdr.dst
        );
        assert_eq!(Example { a: 42, b: 789 }, msg.t);

        let msg = hdl.recv().await;

        assert_eq!(
            Address {
                network_id: 0,
                node_id: 0,
                port_id: 123
            },
            msg.hdr.src
        );
        assert_eq!(
            Address {
                network_id: 0,
                node_id: 0,
                port_id: 0
            },
            msg.hdr.dst
        );
        assert_eq!(Example { a: 56, b: 1234 }, msg.t);
        tsk.await.unwrap();
    }
    // The socket has now been dropped, try sending again.
    //
    // Both sends should fail.
    STACK
        .send_ty::<Other>(
            &Header {
                src,
                dst,
                key: Some(Key(*b"1234TEST")),
                seq_no: None,
                kind: FrameKind::ENDPOINT_REQ,
                ttl: DEFAULT_TTL,
            },
            &Other { a: 345, b: -123 },
        )
        .unwrap_err();
    STACK
        .send_ty::<Example>(
            &Header {
                src,
                dst,
                key: Some(Key(*b"TEST1234")),
                seq_no: None,
                kind: FrameKind::ENDPOINT_REQ,
                ttl: DEFAULT_TTL,
            },
            &Example { a: 42, b: 789 },
        )
        .unwrap_err();
}
