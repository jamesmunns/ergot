use std::{pin::pin, time::Duration};

use ergot::{
    NetStack,
    ergot_base::{Address, FrameKind, Header}, interface_manager::null::NullInterfaceManager,
    socket::endpoint::OwnedEndpointSocket,
};
use ergot_base::Key;
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use postcard_rpc::{Endpoint, endpoint};
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};
use tokio::{spawn, time::{sleep, timeout}};

#[derive(Serialize, Deserialize, Debug, PartialEq, Schema)]
pub struct Example {
    a: u8,
    b: u32,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Schema)]
pub struct Other {
    a: u64,
    b: i32,
}

endpoint!(ExampleEndpoint, Example, u32, "example");
endpoint!(OtherEndpoint, Other, u32, "other");

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
        let socket = OwnedEndpointSocket::<ExampleEndpoint, _, _>::new(&STACK);
        let mut socket = pin!(socket);
        let mut hdl = socket.as_mut().attach();

        let tsk = spawn(async move {
            sleep(Duration::from_millis(100)).await;

            // try sending, should fail
            STACK
                .send_ty::<Other>(
                    Header {
                        src,
                        dst,
                        key: Some(Key(OtherEndpoint::REQ_KEY.to_bytes())),
                        seq_no: None,
                        kind: FrameKind::ENDPOINT_REQ,
                    },
                    Other { a: 345, b: -123 },
                )
                .unwrap_err();
            // typed sending works
            STACK
                .send_ty::<Example>(
                    Header {
                        src,
                        dst,
                        key: Some(Key(ExampleEndpoint::REQ_KEY.to_bytes())),
                        seq_no: None,
                        kind: FrameKind::ENDPOINT_REQ,
                    },
                    Example { a: 42, b: 789 },
                )
                .unwrap();
            // raw sending works
            // (todo: wait a bit to free up space, we wont need this when we can
            // hold more than one message at a time)
            sleep(Duration::from_millis(100)).await;
            let body = postcard::to_stdvec(&Example { a: 56, b: 1234 }).unwrap();
            STACK
                .send_raw(
                    Header {
                        src,
                        dst,
                        key: Some(Key(ExampleEndpoint::REQ_KEY.to_bytes())),
                        seq_no: None,
                        kind: FrameKind::ENDPOINT_REQ,
                    },
                    &body,
                )
                .unwrap();
        });

        let msg = hdl.recv_manual().await;
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

        let msg = hdl.recv_manual().await;

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
            Header {
                src,
                dst,
                key: Some(Key(OtherEndpoint::REQ_KEY.to_bytes())),
                seq_no: None,
                kind: FrameKind::ENDPOINT_REQ,
            },
            Other { a: 345, b: -123 },
        )
        .unwrap_err();
    STACK
        .send_ty::<Example>(
            Header {
                src,
                dst,
                key: Some(Key(ExampleEndpoint::REQ_KEY.to_bytes())),
                seq_no: None,
                kind: FrameKind::ENDPOINT_REQ,
            },
            Example { a: 42, b: 789 },
        )
        .unwrap_err();
}

#[tokio::test]
async fn req_resp() {
    static STACK: TestNetStack = NetStack::new();

    // Start the server...
    let server = OwnedEndpointSocket::<ExampleEndpoint, _, _>::new(&STACK);
    let server = pin!(server);
    let mut server_hdl = server.attach();

    let reqqr = tokio::task::spawn(async {
        for i in 0..3 {
            sleep(Duration::from_millis(100)).await;

            // Make the request, look ma only the stack handle
            let resp = STACK
                .req_resp::<ExampleEndpoint>(
                    Address {
                        network_id: 0,
                        node_id: 0,
                        port_id: 0,
                    },
                    Example {
                        a: i as u8,
                        b: i * 10,
                    },
                )
                .await
                .unwrap();

            println!("RESP: {resp:?}");
        }
    });

    // normally you'd do this in a loop...
    for _i in 0..3 {
        let srv = timeout(Duration::from_secs(1), server_hdl
            .serve(async |req| {
                // fn(Example) -> u32
                req.b + 5
            }))
            .await;
        println!("SERV: {srv:?}");
    }

    reqqr.await.unwrap();
}
