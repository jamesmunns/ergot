use std::{pin::pin, time::Duration};

use bbq2::{
    queue::BBQueue,
    traits::{coordination::cas::AtomicCoord, notifier::maitake::MaiNotSpsc, storage::Inline},
};
use ergot_base::{
    Address, AnyAllAppendix, DEFAULT_TTL, FrameKind, Header, Key, NetStack, ProtocolError,
    interface_manager::profiles::null::Null,
    socket::{Attributes, owned::single::Socket},
    wire_frames::{CommonHeader, encode_frame_ty},
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use postcard::ser_flavors;
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
        let socket = Socket::<Example, &_>::new(
            &STACK,
            Key(*b"TEST1234"),
            Attributes {
                kind: FrameKind::ENDPOINT_REQ,
                discoverable: true,
            },
            None,
        );
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
                            key: Key(*b"1234TEST"),
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
                            key: Key(*b"TEST1234"),
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
            let body = postcard::to_stdvec(&Example { a: 56, b: 1234 }).unwrap();
            let mut buf = [0u8; 128];
            let hdr = encode_frame_ty::<_, ()>(
                ser_flavors::Slice::new(&mut buf),
                &CommonHeader {
                    src,
                    dst,
                    seq_no: 123,
                    kind: FrameKind::ENDPOINT_REQ,
                    ttl: DEFAULT_TTL,
                },
                Some(&AnyAllAppendix {
                    key: Key(*b"TEST1234"),
                    nash: None,
                }),
                &(),
            )
            .unwrap();
            STACK
                .send_raw(
                    &Header {
                        src,
                        dst,
                        any_all: Some(AnyAllAppendix {
                            key: Key(*b"TEST1234"),
                            nash: None,
                        }),
                        seq_no: Some(123),
                        kind: FrameKind::ENDPOINT_REQ,
                        ttl: DEFAULT_TTL,
                    },
                    hdr,
                    &body,
                )
                .unwrap();
        });

        let msg = hdl.recv().await.unwrap();
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

        let msg = hdl.recv().await.unwrap();

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
                    key: Key(*b"1234TEST"),
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
                    key: Key(*b"TEST1234"),
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
async fn hello_err() {
    static STACK: TestNetStack = NetStack::new();
    let src = Address {
        network_id: 0,
        node_id: 0,
        port_id: 123,
    };

    let socket = Socket::<Example, &_>::new(
        &STACK,
        Key(*b"TEST1234"),
        Attributes {
            kind: FrameKind::ENDPOINT_REQ,
            discoverable: true,
        },
        None,
    );
    let mut socket = pin!(socket);
    let mut hdl = socket.as_mut().attach();
    let port = hdl.port();

    let tsk = spawn(async move {
        sleep(Duration::from_millis(100)).await;

        // Send an error
        STACK
            .send_err(
                &Header {
                    src,
                    dst: Address {
                        network_id: 0,
                        node_id: 0,
                        port_id: port,
                    },
                    any_all: None,
                    seq_no: None,
                    kind: FrameKind::PROTOCOL_ERROR,
                    ttl: 1,
                },
                ProtocolError::NSSE_NO_ROUTE,
            )
            .unwrap();
    });

    let msg = hdl.recv().await.unwrap_err();
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
            port_id: port,
        },
        msg.hdr.dst
    );
    assert_eq!(ProtocolError::NSSE_NO_ROUTE, msg.t);

    tsk.await.unwrap();
}

#[tokio::test]
async fn hello_borrowed() {
    static STACK: TestNetStack = NetStack::new();
    let src = Address {
        network_id: 0,
        node_id: 0,
        port_id: 123,
    };

    #[derive(Serialize, Deserialize, Clone)]
    struct Example<'a> {
        lol: &'a str,
    }

    use ergot_base::socket::borrow as brw;

    static QBUF: BBQueue<Inline<1024>, AtomicCoord, MaiNotSpsc> = BBQueue::new();

    let socket = brw::Socket::<&_, &str, &_>::new(
        &STACK,
        Key(*b"TEST1234"),
        Attributes {
            kind: FrameKind::ENDPOINT_REQ,
            discoverable: true,
        },
        &QBUF,
        256,
        None,
    );
    let mut socket = pin!(socket);
    let mut hdl = socket.as_mut().attach();
    let port = hdl.port();

    let tsk = spawn(async move {
        sleep(Duration::from_millis(100)).await;
        let s: &str = "hello, world!";

        // Send an error
        STACK
            .send_ty::<&str>(
                &Header {
                    src,
                    dst: Address {
                        network_id: 0,
                        node_id: 0,
                        port_id: port,
                    },
                    any_all: None,
                    seq_no: None,
                    kind: FrameKind::ENDPOINT_REQ,
                    ttl: 1,
                },
                &s,
            )
            .unwrap();
    });

    let msg = hdl.recv().await;
    let msgdeser = msg.try_access().unwrap();
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
            port_id: port,
        },
        msg.hdr.dst
    );
    assert_eq!("hello, world!", msgdeser.unwrap().t);

    tsk.await.unwrap();
}
