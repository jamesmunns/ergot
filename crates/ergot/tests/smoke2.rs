// These tests are from when ergot and ergot base were separate

use std::{pin::pin, time::Duration};

use ergot::{
    Address, AnyAllAppendix, DEFAULT_TTL, FrameKind, Header, HeaderSeq, Key, NetStack, endpoint,
    interface_manager::profiles::null::Null, net_stack::ReqRespError, traits::Endpoint,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};
use tokio::{
    spawn,
    time::{sleep, timeout},
};

#[derive(Serialize, Deserialize, Debug, PartialEq, Schema, Clone)]
pub struct Example {
    a: u8,
    b: u32,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Schema, Clone)]
pub struct Other {
    a: u64,
    b: i32,
}

#[derive(Serialize, Debug, PartialEq, Schema, Clone)]
pub struct SerializeOnly {
    value: u32,
}

endpoint!(ExampleEndpoint, Example, u32, "example");
endpoint!(OtherEndpoint, Other, u32, "other");
endpoint!(SerializeOnlyEndpoint, SerializeOnly, u32, "serialize-only");

type TestNetStack = NetStack<CriticalSectionRawMutex, Null>;

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
        let socket = STACK.endpoints().bounded_server::<ExampleEndpoint, 2>(None);
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
                        any_all: Some(AnyAllAppendix {
                            key: Key(OtherEndpoint::REQ_KEY.to_bytes()),
                            nash: None,
                        }),
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
                        any_all: Some(AnyAllAppendix {
                            key: Key(ExampleEndpoint::REQ_KEY.to_bytes()),
                            nash: None,
                        }),
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
            let body = postcard::to_vec::<_, 128>(&Example { a: 56, b: 1234 }).unwrap();
            STACK
                .send_raw(
                    &HeaderSeq {
                        src,
                        dst,
                        any_all: Some(AnyAllAppendix {
                            key: Key(ExampleEndpoint::REQ_KEY.to_bytes()),
                            nash: None,
                        }),
                        seq_no: 123,
                        kind: FrameKind::ENDPOINT_REQ,
                        ttl: DEFAULT_TTL,
                    },
                    &body,
                    (),
                )
                .unwrap();
        });

        let msg = hdl.recv_manual().await.unwrap();
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

        let msg = hdl.recv_manual().await.unwrap();

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
                any_all: Some(AnyAllAppendix {
                    key: Key(OtherEndpoint::REQ_KEY.to_bytes()),
                    nash: None,
                }),
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
                any_all: Some(AnyAllAppendix {
                    key: Key(ExampleEndpoint::REQ_KEY.to_bytes()),
                    nash: None,
                }),
                seq_no: None,
                kind: FrameKind::ENDPOINT_REQ,
                ttl: DEFAULT_TTL,
            },
            &Example { a: 42, b: 789 },
        )
        .unwrap_err();
}

#[tokio::test]
async fn req_resp() {
    static STACK: TestNetStack = NetStack::new();

    // Start the server...
    let server = STACK.endpoints().bounded_server::<ExampleEndpoint, 2>(None);
    let server = pin!(server);
    let mut server_hdl = server.attach();

    let reqqr = tokio::task::spawn(async {
        for i in 0..3 {
            sleep(Duration::from_millis(100)).await;

            // Make the request, look ma only the stack handle
            let resp = STACK
                .endpoints()
                .request::<ExampleEndpoint>(
                    Address {
                        network_id: 0,
                        node_id: 0,
                        port_id: 0,
                    },
                    &Example {
                        a: i as u8,
                        b: i * 10,
                    },
                    None,
                )
                .await
                .unwrap();

            println!("RESP: {:?}", resp);
        }
    });

    // normally you'd do this in a loop...
    for _i in 0..3 {
        let srv = timeout(
            Duration::from_secs(1),
            server_hdl.serve(async |req| {
                // fn(Example) -> u32
                req.b + 5
            }),
        )
        .await;
        println!("SERV: {:?}", srv);
    }

    reqqr.await.unwrap();
}

#[tokio::test]
async fn request_can_be_sent_before_waiting_for_response() {
    static STACK: TestNetStack = NetStack::new();

    let server = STACK.endpoints().single_server::<ExampleEndpoint>(None);
    let server = pin!(server);
    let mut server = server.attach();

    let client = STACK.endpoints().single_client::<ExampleEndpoint>();
    let client = pin!(client);
    let mut client = client.attach();
    client
        .send_request(Address::unknown(), &Example { a: 7, b: 35 }, None)
        .unwrap();

    server
        .serve(async |req| req.b + u32::from(req.a))
        .await
        .unwrap();
    let response = client.recv().await.unwrap();
    assert_eq!(response.t, 42);
}

#[test]
fn split_client_accepts_serialize_only_requests() {
    static STACK: TestNetStack = NetStack::new();

    let client = STACK.endpoints().single_client::<SerializeOnlyEndpoint>();
    let client = pin!(client);
    let mut client = client.attach();

    // There is intentionally no server. This only needs to reach the send
    // path: clients serialize requests but never deserialize them.
    for value in [42, 43] {
        assert!(matches!(
            client.send_request(Address::unknown(), &SerializeOnly { value }, None),
            Err(ReqRespError::Local(_)),
        ));
    }
}

#[tokio::test]
async fn client_rejects_a_second_outstanding_request() {
    static STACK: TestNetStack = NetStack::new();

    let server = STACK.endpoints().bounded_server::<ExampleEndpoint, 2>(None);
    let server = pin!(server);
    let mut server = server.attach();

    let client = STACK.endpoints().single_client::<ExampleEndpoint>();
    let client = pin!(client);
    let mut client = client.attach();
    client
        .send_request(Address::unknown(), &Example { a: 1, b: 20 }, None)
        .unwrap();

    assert_eq!(
        client.send_request(Address::unknown(), &Example { a: 2, b: 40 }, None),
        Err(ReqRespError::RequestPending),
    );

    server
        .serve(async |req| req.b + u32::from(req.a))
        .await
        .unwrap();
    assert_eq!(client.recv().await.unwrap().t, 21);

    client
        .send_request(Address::unknown(), &Example { a: 2, b: 40 }, None)
        .unwrap();
    server
        .serve(async |req| req.b + u32::from(req.a))
        .await
        .unwrap();
    assert_eq!(client.recv().await.unwrap().t, 42);
}

#[cfg(feature = "std")]
#[tokio::test]
async fn boxed_client_handle_can_move_after_synchronous_send() {
    static STACK: TestNetStack = NetStack::new();

    let server = STACK.endpoints().single_server::<ExampleEndpoint>(None);
    let server = pin!(server);
    let mut server = server.attach();

    let client = STACK.endpoints().single_client::<ExampleEndpoint>();
    let mut client = Box::pin(client).attach_boxed();
    client
        .send_request(Address::unknown(), &Example { a: 2, b: 40 }, None)
        .unwrap();

    let response_task = tokio::spawn(async move { client.recv().await });
    server
        .serve(async |req| req.b + u32::from(req.a))
        .await
        .unwrap();
    assert_eq!(response_task.await.unwrap().unwrap().t, 42);
}

#[tokio::test]
async fn req_resp_stack_vec() {
    static STACK: TestNetStack = NetStack::new();

    // Start the server...
    let server = STACK
        .endpoints()
        .bounded_server::<ExampleEndpoint, 64>(None);
    let server = pin!(server);
    let mut server_hdl = server.attach();

    let reqqr = tokio::task::spawn(async {
        for i in 0..3 {
            sleep(Duration::from_millis(100)).await;

            // Make the request, look ma only the stack handle
            let resp = STACK
                .endpoints()
                .request::<ExampleEndpoint>(
                    Address {
                        network_id: 0,
                        node_id: 0,
                        port_id: 0,
                    },
                    &Example {
                        a: i as u8,
                        b: i * 10,
                    },
                    None,
                )
                .await
                .unwrap();

            println!("RESP: {:?}", resp);
        }
    });

    // normally you'd do this in a loop...
    for _i in 0..3 {
        let srv = timeout(
            Duration::from_secs(1),
            server_hdl.serve(async |req| {
                // fn(Example) -> u32
                req.b + 5
            }),
        )
        .await;
        println!("SERV: {:?}", srv);
    }

    reqqr.await.unwrap();
}
