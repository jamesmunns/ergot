//! A message a device sends itself is held to the MTU of the interface it
//! leaves by, as a forwarded frame is: a frame of exactly the MTU is sent, a
//! longer one is refused as `PacketTooBig`, and a frame that would fit is
//! refused as `InterfaceFull` when the queue has no room for it.

#![cfg(feature = "std")]
// The std bbqueue behind the sinks trips Miri's leak checker, as in the other
// std-queue tests.
#![cfg(not(miri))]

use ergot::{
    Address, FrameKind, Header, TrafficClass,
    interface_manager::{
        Interface, InterfaceSendError, InterfaceState, Profile,
        profiles::direct_edge::DirectEdge,
        utils::{
            can, cobs_stream, framed_stream,
            std::{StdQueue, new_std_queue},
        },
    },
    wire_frames,
};

/// The MTU of every interface here, unless a test names another.
const MTU: u16 = 512;
/// Room for several frames of the MTU.
const QUEUE: usize = 4096;
/// Bodies of zeros and bodies without one: COBS spends the fewest bytes on
/// the first, and the most on a long run of the second.
const FILLS: [u8; 3] = [0x00, 0x01, 0xFF];

struct Cobs;
impl Interface for Cobs {
    type Sink = cobs_stream::Sink<StdQueue>;
}

struct Framed;
impl Interface for Framed {
    type Sink = framed_stream::Sink<StdQueue>;
}

struct Can;
impl Interface for Can {
    type Sink = can::Sink<StdQueue, { MTU as usize }>;
}

fn cobs() -> cobs_stream::Sink<StdQueue> {
    cobs_stream::Sink::new_from_handle(new_std_queue(QUEUE), MTU)
}

fn framed() -> framed_stream::Sink<StdQueue> {
    framed_stream::Sink::new_from_handle(new_std_queue(QUEUE), MTU)
}

/// A CAN FD sink, which assembles frames of up to its `MTU` parameter.
fn can() -> can::Sink<StdQueue, { MTU as usize }> {
    can::Sink::new(new_std_queue(QUEUE), new_std_queue(QUEUE), 64)
}

/// An edge at node 2 of net 1, with its interface up.
fn edge<I: Interface>(sink: I::Sink) -> DirectEdge<I> {
    let mut edge = DirectEdge::new_target(sink);
    edge.set_interface_state(
        (),
        InterfaceState::Active {
            net_id: 1,
            node_id: 2,
        },
    )
    .unwrap();
    edge
}

/// A request from the edge to a port of its router. Its addresses are the
/// ones it leaves with, so its length on the wire is its length here.
fn header() -> Header {
    Header {
        src: Address {
            network_id: 1,
            node_id: 2,
            port_id: 10,
        },
        dst: Address {
            network_id: 1,
            node_id: 1,
            port_id: 20,
        },
        any_all: None,
        kind: FrameKind::ENDPOINT_REQ,
        class: TrafficClass::Normal,
        ttl: 15,
    }
}

/// The frame of [`header`] and `body`, as every interface is given it.
fn frame(body: &[u8]) -> Vec<u8> {
    wire_frames::encode_frame_ty(postcard::ser_flavors::StdVec::new(), &header(), &body).unwrap()
}

/// A body of `fill` bytes whose frame is `frame_len` bytes long.
fn body(frame_len: usize, fill: u8) -> Vec<u8> {
    // The body is a few bytes shorter than its frame: look from the top.
    (0..=frame_len)
        .rev()
        .map(|len| vec![fill; len])
        .find(|body| frame(body).len() == frame_len)
        .expect("a body makes a frame of this length")
}

/// Sends `body` as the application on the edge would.
fn send<I: Interface>(edge: &mut DirectEdge<I>, body: &[u8]) -> Result<(), InterfaceSendError> {
    edge.send(&header(), &body)
}

/// Sends 100-byte frames until the interface refuses one, as full.
fn fill_queue<I: Interface>(edge: &mut DirectEdge<I>) {
    let small = body(100, 0x5A);
    let taken = (0..QUEUE)
        .take_while(|_| send(edge, &small).is_ok())
        .count();
    assert!(taken > 0 && taken < QUEUE, "took {taken} frames");
    assert_eq!(send(edge, &small), Err(InterfaceSendError::InterfaceFull));
}

fn too_big(mtu: u16) -> Result<(), InterfaceSendError> {
    Err(InterfaceSendError::PacketTooBig { mtu })
}

/// A frame too long for the interface never fits, and the sender is told so
/// with the interface's MTU, as a router tells the source of a frame it
/// cannot forward.
fn longer_than_the_mtu_is_too_big<I: Interface>(sink: fn() -> I::Sink) {
    let mut edge = edge::<I>(sink());
    let mtu = usize::from(MTU);
    for len in [mtu + 1, mtu + 2, mtu + 10, 2 * mtu, 4 * mtu, 3 * QUEUE] {
        for fill in FILLS {
            assert_eq!(
                send(&mut edge, &body(len, fill)),
                too_big(MTU),
                "a frame of {len} bytes of {fill:#04x}"
            );
        }
    }
}

/// The same answer when the queue is full as well: waiting for room would not
/// help the sender.
fn longer_than_the_mtu_is_too_big_for_a_full_queue<I: Interface>(sink: fn() -> I::Sink) {
    let mut edge = edge::<I>(sink());
    fill_queue(&mut edge);
    for fill in FILLS {
        assert_eq!(
            send(&mut edge, &body(usize::from(MTU) + 100, fill)),
            too_big(MTU),
            "a frame of {fill:#04x}"
        );
    }
}

/// A frame that would fit and finds no room is refused as full.
fn within_the_mtu_is_full_when_the_queue_is<I: Interface>(sink: fn() -> I::Sink) {
    let mut edge = edge::<I>(sink());
    fill_queue(&mut edge);
    for fill in FILLS {
        assert_eq!(
            send(&mut edge, &body(usize::from(MTU), fill)),
            Err(InterfaceSendError::InterfaceFull),
            "a frame of the MTU, of {fill:#04x}"
        );
    }
}

/// The MTU is the longest frame an interface takes, not one byte short of it.
fn up_to_the_mtu_is_sent<I: Interface>(sink: fn() -> I::Sink) {
    for len in [MTU - 2, MTU - 1, MTU].map(usize::from) {
        for fill in FILLS {
            let mut edge = edge::<I>(sink());
            assert_eq!(
                send(&mut edge, &body(len, fill)),
                Ok(()),
                "a frame of {len} bytes of {fill:#04x}"
            );
        }
    }
}

/// The tests above, for one kind of interface.
macro_rules! held_to_its_mtu {
    ($interface:ident, $kind:ty, $sink:expr) => {
        mod $interface {
            use super::*;

            #[test]
            fn a_frame_longer_than_the_mtu_is_refused_as_too_big() {
                longer_than_the_mtu_is_too_big::<$kind>($sink);
            }

            #[test]
            fn a_frame_longer_than_the_mtu_is_too_big_for_a_full_queue_too() {
                longer_than_the_mtu_is_too_big_for_a_full_queue::<$kind>($sink);
            }

            #[test]
            fn a_frame_within_the_mtu_is_refused_as_full_when_the_queue_is() {
                within_the_mtu_is_full_when_the_queue_is::<$kind>($sink);
            }

            #[test]
            fn a_frame_of_up_to_the_mtu_is_sent() {
                up_to_the_mtu_is_sent::<$kind>($sink);
            }
        }
    };
}

held_to_its_mtu!(cobs_interface, Cobs, cobs);
held_to_its_mtu!(framed_interface, Framed, framed);
held_to_its_mtu!(can_interface, Can, can);

/// The MTUs of the two tests below: the usual one, and those where the
/// blocks of COBS, 254 bytes each, begin and end.
const COBS_MTUS: [u16; 9] = [512, 100, 254, 255, 300, 508, 1016, 1024, 2048];

/// An edge over a COBS interface of `mtu`, and the queue it fills.
fn cobs_edge(mtu: u16) -> (DirectEdge<Cobs>, StdQueue) {
    let queue = new_std_queue(2 * QUEUE);
    let sink = cobs_stream::Sink::new_from_handle(queue.clone(), mtu);
    (edge(sink), queue)
}

/// What an edge queued on a COBS interface: the frames, decoded.
fn queued_frames(queue: &StdQueue) -> Vec<Vec<u8>> {
    let consumer = queue.stream_consumer();
    let mut frames = Vec::new();
    while let Ok(grant) = consumer.read() {
        let len = grant.len();
        frames.extend(
            grant
                .split(|&byte| byte == 0)
                .filter(|encoded| !encoded.is_empty())
                .map(|encoded| cobs::decode_vec(encoded).expect("a COBS frame")),
        );
        grant.release(len);
    }
    frames
}

/// A COBS interface counts the frame, not what COBS makes of it. The room it
/// reserves is the most a frame of the MTU can need, and a frame COBS encodes
/// in less (one of zeros, in the fewest bytes) must not use the rest to be
/// longer than the MTU: the other end need not have room for it.
#[test]
fn a_cobs_interface_queues_no_frame_longer_than_its_mtu() {
    for mtu in COBS_MTUS {
        for over in 1..=12 {
            for fill in FILLS {
                let (mut edge, queue) = cobs_edge(mtu);
                let len = usize::from(mtu) + over;
                let sent = send(&mut edge, &body(len, fill));
                let queued: Vec<usize> = queued_frames(&queue).iter().map(Vec::len).collect();
                assert_eq!(
                    queued,
                    Vec::<usize>::new(),
                    "MTU {mtu}: queued a frame of {len} bytes of {fill:#04x}"
                );
                assert_eq!(
                    sent,
                    too_big(mtu),
                    "MTU {mtu}: a frame of {len} bytes of {fill:#04x}"
                );
            }
        }
    }
}

/// And it queues every frame up to its MTU, as the bytes it was given: also
/// one without a zero that ends where a block of COBS does, which takes a
/// byte more than `cobs::max_encoding_length` allows for.
#[test]
fn a_cobs_interface_queues_every_frame_up_to_its_mtu() {
    for mtu in COBS_MTUS {
        for under in 0..=12 {
            for fill in FILLS {
                let (mut edge, queue) = cobs_edge(mtu);
                let len = usize::from(mtu) - under;
                let body = body(len, fill);
                assert_eq!(
                    send(&mut edge, &body),
                    Ok(()),
                    "MTU {mtu}: a frame of {len} bytes of {fill:#04x}"
                );
                // The frame leaves with one hop of its TTL spent, which is in
                // a byte it had already: the same length.
                let queued = queued_frames(&queue);
                assert_eq!(queued.len(), 1);
                assert_eq!(queued[0].len(), len);
                assert!(queued[0].ends_with(&body), "MTU {mtu}, {fill:#04x}");
            }
        }
    }
}

/// Such a frame also has to arrive: the receiving end of a stream reserves
/// room for the longest encoded frame of its MTU.
#[cfg(feature = "tokio-std")]
mod over_a_stream {
    use std::{pin::pin, time::Duration};

    use ergot::{
        Address, FrameKind, Header, TrafficClass, endpoint,
        interface_manager::{
            InterfaceState, Profile,
            interface_impls::tokio_stream::TokioStreamInterface,
            profiles::{
                direct_edge::{DirectEdge, EdgeFrameProcessor},
                router::Router,
            },
            transports::tokio_cobs_stream,
            utils::{cobs_stream, std::new_std_queue},
        },
        net_stack::ArcNetStack,
        wire_frames,
    };
    use mutex::raw_impls::cs::CriticalSectionRawMutex;
    use tokio::{sync::oneshot, time::timeout};

    type Bytes = Vec<u8>;
    endpoint!(LengthEndpoint, Bytes, u32, "typed-send-mtu/length");

    type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<TokioStreamInterface>>;
    type RouterStack = ArcNetStack<
        CriticalSectionRawMutex,
        Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>,
    >;

    /// The router at node 1 of net 1 and its edge at node 2, each with a COBS
    /// interface of `mtu`, and the port on the router that answers a request
    /// with the length of its body.
    async fn link(mtu: u16) -> (RouterStack, EdgeStack, u8) {
        let router: RouterStack = RouterStack::new();
        let queue = new_std_queue(8192);
        let edge = EdgeStack::new_with_profile(DirectEdge::new_target(
            cobs_stream::Sink::new_from_handle(queue.clone(), mtu),
        ));
        // Pipes shorter than the frames, so that each end collects a frame
        // from several reads, in the room it reserved for one.
        let (edge_read, router_write) = tokio::io::duplex(100);
        let (router_read, edge_write) = tokio::io::duplex(100);
        tokio_cobs_stream::register_router(
            router.clone(),
            router_read,
            router_write,
            mtu,
            8192,
            None,
            None,
        )
        .await
        .unwrap();
        tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
            edge.clone(),
            edge_read,
            edge_write,
            queue,
            EdgeFrameProcessor::new(),
            InterfaceState::edge_link_local(),
            None,
            None,
        )
        .await
        .unwrap();

        let (port, served_at) = oneshot::channel();
        tokio::spawn({
            let router = router.clone();
            async move {
                let server = router.endpoints().bounded_server::<LengthEndpoint, 4>(None);
                let server = pin!(server);
                let mut server = server.attach();
                port.send(server.port()).unwrap();
                loop {
                    let _ = server
                        .serve(|body: &Bytes| {
                            let len = body.len() as u32;
                            async move { len }
                        })
                        .await;
                }
            }
        });
        let port = served_at.await.unwrap();

        // The edge learns its net from the first answer. Until then its
        // addresses, and so its frames, are shorter.
        let link_local = Address {
            network_id: 0,
            node_id: 1,
            port_id: port,
        };
        assert_eq!(ask(&edge, link_local, 1).await, Ok(1));
        assert_eq!(
            edge.manage_profile(|profile| profile.interface_state(())),
            Some(InterfaceState::Active {
                net_id: 1,
                node_id: 2
            })
        );
        (router, edge, port)
    }

    /// Asks `at` for the length of a body of `len` bytes, none of them zero.
    async fn ask(edge: &EdgeStack, at: Address, len: usize) -> Result<u32, String> {
        let body = vec![0xA5; len];
        let request = edge.endpoints().request::<LengthEndpoint>(at, &body, None);
        match timeout(Duration::from_secs(2), request).await {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(refused)) => Err(format!("{refused:?}")),
            Err(_) => Err("no answer".to_owned()),
        }
    }

    /// The request frame of a body of `len` bytes, from any port of the edge
    /// to any port of the router: the ports do not change its length.
    fn request_frame(len: usize) -> Vec<u8> {
        let header = Header {
            src: Address {
                network_id: 1,
                node_id: 2,
                port_id: 1,
            },
            dst: Address {
                network_id: 1,
                node_id: 1,
                port_id: 1,
            },
            any_all: None,
            kind: FrameKind::ENDPOINT_REQ,
            class: TrafficClass::Normal,
            ttl: 15,
        };
        wire_frames::encode_frame_ty(
            postcard::ser_flavors::StdVec::new(),
            &header,
            &vec![0xA5u8; len],
        )
        .unwrap()
    }

    /// A frame of the MTU without a zero crosses a link whose MTU is where a
    /// block of COBS ends, as the shorter frames before it do.
    #[tokio::test]
    async fn a_frame_of_the_mtu_arrives_where_a_block_of_cobs_ends() {
        for mtu in [254u16, 508, 1016] {
            let (_router, edge, port) = link(mtu).await;
            let router = Address {
                network_id: 1,
                node_id: 1,
                port_id: port,
            };
            let longest = (0..usize::from(mtu))
                .rfind(|&len| request_frame(len).len() == usize::from(mtu))
                .expect("a body makes a frame of the MTU");
            assert!(!request_frame(longest).contains(&0));
            for len in longest - 3..=longest {
                assert_eq!(
                    ask(&edge, router, len).await,
                    Ok(len as u32),
                    "MTU {mtu}: a request frame of {} bytes",
                    request_frame(len).len()
                );
            }
        }
    }
}
