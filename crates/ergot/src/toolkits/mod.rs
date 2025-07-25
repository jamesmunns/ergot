//! Toolkits
//!
//! Toolkits are a collection of types and methods useful when using a specific
//! profile of netstack. Ideally: end users should need relatively few `use` statements
//! outside of the given toolkit they plan to use.

#[cfg(feature = "embassy-usb-v0_5")]
pub mod embassy_usb_v0_5 {
    use ergot_base::{
        exports::bbq2::{
            prod_cons::framed::FramedProducer,
            queue::BBQueue,
            traits::{bbqhdl::BbqHandle, notifier::maitake::MaiNotSpsc, storage::Inline},
        },
        interface_manager::{
            profiles::direct_edge::{
                DirectEdge,
                eusb_0_5::{self, EmbassyUsbManager},
            },
            utils::framed_stream::Sink,
        },
    };
    use mutex::{ConstInit, ScopedRawMutex};

    use crate::NetStack;

    pub use ergot_base::interface_manager::interface_impls::embassy_usb::{
        DEFAULT_TIMEOUT_MS_PER_FRAME, USB_FS_MAX_PACKET_SIZE,
        eusb_0_5::{WireStorage, tx_worker},
    };

    pub type Queue<const N: usize, C> = BBQueue<Inline<N>, C, MaiNotSpsc>;
    pub type Stack<Q, R> = NetStack<R, EmbassyUsbManager<Q>>;
    pub type BaseStack<Q, R> = ergot_base::NetStack<R, EmbassyUsbManager<Q>>;
    pub type RxWorker<Q, R, D> = eusb_0_5::RxWorker<Q, &'static BaseStack<Q, R>, D>;

    pub const fn new_target_stack<Q, R>(producer: FramedProducer<Q, u16>, mtu: u16) -> Stack<Q, R>
    where
        Q: BbqHandle,
        R: ScopedRawMutex + ConstInit + 'static,
    {
        NetStack::new_with_profile(DirectEdge::new_target(Sink::new(producer, mtu)))
    }
}

#[cfg(feature = "std")]
pub mod std_tcp {
    use ergot_base::interface_manager::{
        interface_impls::std_tcp::StdTcpInterface,
        profiles::{
            direct_edge::{self, DirectEdge, std_tcp::SocketAlreadyActive},
            direct_router::{self, DirectRouter, std_tcp::Error},
        },
        utils::{cobs_stream, std::StdQueue},
    };
    use mutex::raw_impls::cs::CriticalSectionRawMutex;
    use tokio::net::TcpStream;

    pub use ergot_base::interface_manager::utils::std::new_std_queue;

    use crate::net_stack::ArcNetStack;

    pub type RouterStack = ArcNetStack<CriticalSectionRawMutex, DirectRouter<StdTcpInterface>>;
    pub type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<StdTcpInterface>>;

    pub async fn register_router_interface(
        stack: &RouterStack,
        socket: TcpStream,
        max_ergot_packet_size: u16,
        outgoing_buffer_size: usize,
    ) -> Result<u64, Error> {
        direct_router::std_tcp::register_interface(
            stack.clone(),
            socket,
            max_ergot_packet_size,
            outgoing_buffer_size,
        )
        .await
    }

    pub async fn register_edge_interface(
        stack: &EdgeStack,
        socket: TcpStream,
        queue: &StdQueue,
    ) -> Result<(), SocketAlreadyActive> {
        direct_edge::std_tcp::register_target_interface(stack.clone(), socket, queue.clone()).await
    }

    pub fn new_target_stack(queue: &StdQueue, mtu: u16) -> EdgeStack {
        EdgeStack::new_with_profile(DirectEdge::new_target(cobs_stream::Sink::new_from_handle(
            queue.clone(),
            mtu,
        )))
    }
}

#[cfg(feature = "nusb-v0_1")]
pub mod nusb_v0_1 {
    use ergot_base::interface_manager::{
        interface_impls::nusb_bulk::NusbBulk,
        profiles::direct_router::{self, DirectRouter, nusb_0_1::Error},
    };
    use mutex::raw_impls::cs::CriticalSectionRawMutex;

    use crate::net_stack::ArcNetStack;

    pub use ergot_base::interface_manager::interface_impls::nusb_bulk::{
        NewDevice, find_new_devices,
    };

    pub type RouterStack = ArcNetStack<CriticalSectionRawMutex, DirectRouter<NusbBulk>>;
    pub async fn register_router_interface(
        stack: &RouterStack,
        device: NewDevice,
        max_ergot_packet_size: u16,
        outgoing_buffer_size: usize,
    ) -> Result<u64, Error> {
        direct_router::nusb_0_1::register_interface(
            stack.clone(),
            device,
            max_ergot_packet_size,
            outgoing_buffer_size,
        )
        .await
    }
}
