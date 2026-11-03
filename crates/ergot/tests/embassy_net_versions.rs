//! Constructors must keep accepting the matching socket when Cargo unifies
//! both embassy-net features. Checking the library alone misses this: its
//! constructor can compile while rejecting the caller's socket type.

#![cfg(all(
    feature = "std",
    any(feature = "embassy-net-v0_7", feature = "embassy-net-v0_9")
))]

use ergot::exports::bbqueue::traits::coordination::cas::AtomicCoord;
use ergot::interface_manager::profiles::direct_edge::EdgeFrameProcessor;
use ergot::toolkits::null::new_arc_null_stack;

macro_rules! constructor_test {
    ($name:ident, $kit:ident, $net:ident) => {
        #[test]
        fn $name() {
            fn accepts_socket(
                socket: $net::udp::UdpSocket<'static>,
                remote: $net::udp::UdpMetadata,
            ) {
                use ergot::toolkits::$kit as kit;
                static QUEUE: kit::Queue<1024, AtomicCoord> = kit::Queue::new();
                let _worker = kit::RxTxWorker::new(
                    new_arc_null_stack(),
                    socket,
                    EdgeFrameProcessor::new(),
                    (),
                    QUEUE.framed_consumer(),
                    remote,
                );
            }
            // Type-check the constructor without requiring a running network
            // stack or fabricating an invalid socket.
            let _: fn($net::udp::UdpSocket<'static>, $net::udp::UdpMetadata) = accepts_socket;
        }
    };
}

#[cfg(feature = "embassy-net-v0_7")]
constructor_test!(
    worker_07_accepts_07_socket,
    embassy_net_v0_7,
    embassy_net_0_7
);
#[cfg(feature = "embassy-net-v0_9")]
constructor_test!(
    worker_09_accepts_09_socket,
    embassy_net_v0_9,
    embassy_net_0_9
);

#[cfg(feature = "embassy-net-v0_7")]
#[test]
fn legacy_worker_keeps_07_socket_when_both_features_are_enabled() {
    fn accepts_socket(
        socket: embassy_net_0_7::udp::UdpSocket<'static>,
        remote: embassy_net_0_7::udp::UdpMetadata,
    ) {
        use ergot::interface_manager::transports::embassy_net_udp::RxTxWorker;
        use ergot::toolkits::embassy_net_v0_7 as kit;
        static QUEUE: kit::Queue<1024, AtomicCoord> = kit::Queue::new();
        let _worker = RxTxWorker::new(
            new_arc_null_stack(),
            socket,
            EdgeFrameProcessor::new(),
            (),
            QUEUE.framed_consumer(),
            remote,
        );
    }
    let _: fn(embassy_net_0_7::udp::UdpSocket<'static>, embassy_net_0_7::udp::UdpMetadata) =
        accepts_socket;
}

#[cfg(all(feature = "embassy-net-v0_9", not(feature = "embassy-net-v0_7")))]
#[test]
fn legacy_worker_accepts_09_socket_when_only_09_is_enabled() {
    fn accepts_socket(
        socket: embassy_net_0_9::udp::UdpSocket<'static>,
        remote: embassy_net_0_9::udp::UdpMetadata,
    ) {
        use ergot::interface_manager::transports::embassy_net_udp::RxTxWorker;
        use ergot::toolkits::embassy_net_v0_9 as kit;
        static QUEUE: kit::Queue<1024, AtomicCoord> = kit::Queue::new();
        let _worker = RxTxWorker::new(
            new_arc_null_stack(),
            socket,
            EdgeFrameProcessor::new(),
            (),
            QUEUE.framed_consumer(),
            remote,
        );
    }
    let _: fn(embassy_net_0_9::udp::UdpSocket<'static>, embassy_net_0_9::udp::UdpMetadata) =
        accepts_socket;
}
