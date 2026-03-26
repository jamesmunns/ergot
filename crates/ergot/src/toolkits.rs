//! Toolkits
//!
//! Toolkits are a collection of types and methods useful when using a specific
//! profile of netstack. Ideally: end users should need relatively few `use` statements
//! outside of the given toolkit they plan to use.

pub mod null {
    #[cfg(feature = "std")]
    use mutex::raw_impls::cs::CriticalSectionRawMutex;

    use crate::interface_manager::profiles::null::Null;

    #[cfg(feature = "std")]
    use crate::interface_manager::ConstInit;

    pub type NullStack<R> = crate::NetStack<R, Null>;

    #[cfg(feature = "std")]
    pub type ArcNullStack = crate::net_stack::arc::ArcNetStack<CriticalSectionRawMutex, Null>;

    #[cfg(feature = "std")]
    pub fn new_arc_null_stack() -> ArcNullStack {
        crate::net_stack::arc::ArcNetStack::new_with_profile(Null::INIT)
    }
}

#[cfg(feature = "embedded-io-async-v0_6")]
pub mod embedded_io_async_v0_6 {
    use crate::{
        exports::bbqueue::{
            BBQueue,
            prod_cons::stream::StreamProducer,
            traits::{bbqhdl::BbqHandle, notifier::maitake::MaiNotSpsc, storage::Inline},
        },
        interface_manager::{
            profiles::direct_edge::{DirectEdge, EmbeddedIoManager},
            utils::cobs_stream::Sink,
        },
    };
    use mutex::{ConstInit, ScopedRawMutex};

    use crate::NetStack;
    pub use crate::interface_manager::interface_impls::embedded_io::tx_worker;

    pub type Queue<const N: usize, C> = BBQueue<Inline<N>, C, MaiNotSpsc>;
    pub type Stack<Q, R> = NetStack<R, EmbeddedIoManager<Q>>;
    pub type BaseStack<Q, R> = crate::NetStack<R, EmbeddedIoManager<Q>>;

    pub const fn new_target_stack<Q, R>(producer: StreamProducer<Q>, mtu: u16) -> Stack<Q, R>
    where
        Q: BbqHandle + 'static,
        R: ScopedRawMutex + ConstInit + 'static,
    {
        NetStack::new_with_profile(DirectEdge::new_target(Sink::new(producer, mtu)))
    }
}

#[cfg(feature = "embedded-io-async-v0_7")]
pub mod embedded_io_async_v0_7 {
    use crate::{
        exports::bbqueue::{
            BBQueue,
            prod_cons::stream::StreamProducer,
            traits::{bbqhdl::BbqHandle, notifier::maitake::MaiNotSpsc, storage::Inline},
        },
        interface_manager::{
            profiles::direct_edge::{DirectEdge, EmbeddedIoManager},
            utils::cobs_stream::Sink,
        },
    };
    use mutex::{ConstInit, ScopedRawMutex};

    use crate::NetStack;
    pub use crate::interface_manager::interface_impls::embedded_io::tx_worker;

    pub type Queue<const N: usize, C> = BBQueue<Inline<N>, C, MaiNotSpsc>;
    pub type Stack<Q, R> = NetStack<R, EmbeddedIoManager<Q>>;
    pub type BaseStack<Q, R> = crate::NetStack<R, EmbeddedIoManager<Q>>;

    pub const fn new_target_stack<Q, R>(producer: StreamProducer<Q>, mtu: u16) -> Stack<Q, R>
    where
        Q: BbqHandle + 'static,
        R: ScopedRawMutex + ConstInit + 'static,
    {
        NetStack::new_with_profile(DirectEdge::new_target(Sink::new(producer, mtu)))
    }
}

#[cfg(feature = "embassy-usb-v0_5")]
pub mod embassy_usb_v0_5 {
    use crate::{
        exports::bbqueue::{
            BBQueue,
            prod_cons::framed::FramedProducer,
            traits::{bbqhdl::BbqHandle, notifier::maitake::MaiNotSpsc, storage::Inline},
        },
        interface_manager::{
            profiles::direct_edge::{DirectEdge, EmbassyUsbManager},
            utils::framed_stream::Sink,
        },
    };
    use mutex::{ConstInit, ScopedRawMutex};

    use crate::NetStack;

    pub use crate::interface_manager::interface_impls::embassy_usb::{
        DEFAULT_TIMEOUT_MS_PER_FRAME, USB_FS_MAX_PACKET_SIZE,
        eusb_0_5::{WireStorage, tx_worker},
    };

    pub type Queue<const N: usize, C> = BBQueue<Inline<N>, C, MaiNotSpsc>;
    pub type Stack<Q, R> = NetStack<R, EmbassyUsbManager<Q>>;
    pub type BaseStack<Q, R> = crate::NetStack<R, EmbassyUsbManager<Q>>;

    pub const fn new_target_stack<Q, R>(producer: FramedProducer<Q, u16>, mtu: u16) -> Stack<Q, R>
    where
        Q: BbqHandle,
        R: ScopedRawMutex + ConstInit + 'static,
    {
        NetStack::new_with_profile(DirectEdge::new_target(Sink::new(producer, mtu)))
    }
}

#[cfg(feature = "embassy-usb-v0_6")]
pub mod embassy_usb_v0_6 {
    use crate::{
        exports::bbqueue::{
            BBQueue,
            prod_cons::framed::FramedProducer,
            traits::{bbqhdl::BbqHandle, notifier::maitake::MaiNotSpsc, storage::Inline},
        },
        interface_manager::{
            profiles::direct_edge::{DirectEdge, EmbassyUsbManager},
            utils::framed_stream::Sink,
        },
    };
    use mutex::{ConstInit, ScopedRawMutex};

    use crate::NetStack;

    pub use crate::interface_manager::interface_impls::embassy_usb::{
        DEFAULT_TIMEOUT_MS_PER_FRAME, USB_FS_MAX_PACKET_SIZE,
        eusb_0_6::{WireStorage, tx_worker},
    };

    pub type Queue<const N: usize, C> = BBQueue<Inline<N>, C, MaiNotSpsc>;
    pub type Stack<Q, R> = NetStack<R, EmbassyUsbManager<Q>>;
    pub type BaseStack<Q, R> = crate::NetStack<R, EmbassyUsbManager<Q>>;

    pub const fn new_target_stack<Q, R>(producer: FramedProducer<Q, u16>, mtu: u16) -> Stack<Q, R>
    where
        Q: BbqHandle,
        R: ScopedRawMutex + ConstInit + 'static,
    {
        NetStack::new_with_profile(DirectEdge::new_target(Sink::new(producer, mtu)))
    }
}

#[cfg(feature = "embassy-net-v0_7")]
pub mod embassy_net_v0_7 {
    use crate::NetStack;
    use crate::interface_manager::InterfaceState;
    use crate::interface_manager::interface_impls::embassy_net_udp::EmbassyNetInterface;
    use crate::interface_manager::profiles::direct_edge::DirectEdge;
    use crate::interface_manager::utils::framed_stream::Sink;
    use bbqueue::BBQueue;
    use bbqueue::prod_cons::framed::FramedProducer;
    use bbqueue::traits::bbqhdl::BbqHandle;
    use bbqueue::traits::notifier::maitake::MaiNotSpsc;
    use bbqueue::traits::storage::Inline;
    use maitake_sync::blocking::{ConstInit, ScopedRawMutex};

    pub type Queue<const N: usize, C> = BBQueue<Inline<N>, C, MaiNotSpsc>;

    pub type EdgeStack<Q, R> = NetStack<R, DirectEdge<EmbassyNetInterface<Q>>>;

    pub const fn new_target_stack<Q, R>(producer: FramedProducer<Q>, mtu: u16) -> EdgeStack<Q, R>
    where
        Q: BbqHandle,
        R: ScopedRawMutex + ConstInit + 'static,
    {
        NetStack::new_with_profile(DirectEdge::new_target(Sink::new(producer, mtu)))
    }

    pub const fn new_controller_stack<Q, R>(
        producer: FramedProducer<Q>,
        mtu: u16,
    ) -> EdgeStack<Q, R>
    where
        Q: BbqHandle,
        R: ScopedRawMutex + ConstInit + 'static,
    {
        NetStack::new_with_profile(DirectEdge::new_controller(
            Sink::new(producer, mtu),
            InterfaceState::Down,
        ))
    }
}

#[cfg(feature = "tokio-std")]
pub mod tokio_tcp {
    use crate::interface_manager::{
        InterfaceState,
        interface_impls::tokio_tcp::TokioTcpInterface,
        profiles::direct_edge::{DirectEdge, EdgeFrameProcessor},
        profiles::direct_router::DirectRouter,
        transports::tokio_cobs_stream,
        utils::{cobs_stream, std::StdQueue},
    };
    use mutex::raw_impls::cs::CriticalSectionRawMutex;
    use tokio::net::TcpStream;

    pub use crate::interface_manager::utils::std::new_std_queue;

    use crate::net_stack::ArcNetStack;

    pub type RouterStack = ArcNetStack<CriticalSectionRawMutex, DirectRouter<TokioTcpInterface>>;
    pub type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<TokioTcpInterface>>;

    pub async fn register_router_interface(
        stack: &RouterStack,
        socket: TcpStream,
        max_ergot_packet_size: u16,
        outgoing_buffer_size: usize,
    ) -> Result<u8, tokio_cobs_stream::RouterRegistrationError> {
        let (rx, tx) = socket.into_split();
        tokio_cobs_stream::register_router(
            stack.clone(),
            rx,
            tx,
            max_ergot_packet_size,
            outgoing_buffer_size,
            None,
            None,
        )
        .await
    }

    pub async fn register_edge_interface(
        stack: &EdgeStack,
        socket: TcpStream,
        queue: &StdQueue,
    ) -> Result<(), tokio_cobs_stream::EdgeRegistrationError> {
        let (rx, tx) = socket.into_split();
        tokio_cobs_stream::register_edge::<_, TokioTcpInterface, _, _>(
            stack.clone(),
            rx,
            tx,
            queue.clone(),
            EdgeFrameProcessor::new(),
            InterfaceState::Inactive,
            None,
            None,
        )
        .await
    }

    pub fn new_target_stack(queue: &StdQueue, mtu: u16) -> EdgeStack {
        EdgeStack::new_with_profile(DirectEdge::new_target(cobs_stream::Sink::new_from_handle(
            queue.clone(),
            mtu,
        )))
    }
}

#[cfg(feature = "tokio-std")]
pub mod tokio_udp {
    use crate::interface_manager::{
        InterfaceState,
        interface_impls::tokio_udp::TokioUdpInterface,
        profiles::direct_edge::{CENTRAL_NODE_ID, DirectEdge, EdgeFrameProcessor},
        profiles::direct_router::DirectRouter,
        transports::tokio_udp as udp_transport,
        utils::{framed_stream, std::StdQueue},
    };
    use mutex::raw_impls::cs::CriticalSectionRawMutex;
    use tokio::net::UdpSocket;

    pub use crate::interface_manager::utils::std::new_std_queue;

    use crate::net_stack::ArcNetStack;

    pub type RouterStack = ArcNetStack<CriticalSectionRawMutex, DirectRouter<TokioUdpInterface>>;
    pub type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<TokioUdpInterface>>;

    pub async fn register_router_interface(
        stack: &RouterStack,
        socket: UdpSocket,
        max_ergot_packet_size: u16,
        outgoing_buffer_size: usize,
    ) -> Result<u8, udp_transport::RouterRegistrationError> {
        udp_transport::register_router(
            stack.clone(),
            socket,
            max_ergot_packet_size,
            outgoing_buffer_size,
            None,
            None,
        )
        .await
    }

    pub async fn register_edge_target_interface(
        stack: &EdgeStack,
        socket: UdpSocket,
        queue: &StdQueue,
        liveness: Option<crate::interface_manager::LivenessConfig>,
        state_notify: Option<std::sync::Arc<maitake_sync::WaitQueue>>,
    ) -> Result<(), udp_transport::EdgeRegistrationError> {
        udp_transport::register_edge::<_, TokioUdpInterface>(
            stack.clone(),
            socket,
            queue.clone(),
            EdgeFrameProcessor::new(),
            InterfaceState::Inactive,
            liveness,
            state_notify,
        )
        .await
    }

    pub async fn register_edge_controller_interface(
        stack: &EdgeStack,
        socket: UdpSocket,
        queue: &StdQueue,
        liveness: Option<crate::interface_manager::LivenessConfig>,
        state_notify: Option<std::sync::Arc<maitake_sync::WaitQueue>>,
    ) -> Result<(), udp_transport::EdgeRegistrationError> {
        udp_transport::register_edge::<_, TokioUdpInterface>(
            stack.clone(),
            socket,
            queue.clone(),
            EdgeFrameProcessor::new_controller(1),
            InterfaceState::Active {
                net_id: 1,
                node_id: CENTRAL_NODE_ID,
            },
            liveness,
            state_notify,
        )
        .await
    }

    pub fn new_target_stack(queue: &StdQueue, mtu: u16) -> EdgeStack {
        EdgeStack::new_with_profile(DirectEdge::new_target(
            framed_stream::Sink::new_from_handle(queue.clone(), mtu),
        ))
    }

    pub fn new_controller_stack(queue: &StdQueue, mtu: u16) -> EdgeStack {
        EdgeStack::new_with_profile(DirectEdge::new_controller(
            framed_stream::Sink::new_from_handle(queue.clone(), mtu),
            InterfaceState::Down,
        ))
    }
}

#[cfg(feature = "tokio-std")]
pub mod tokio_stream {
    use crate::interface_manager::{
        InterfaceState,
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::direct_edge::{CENTRAL_NODE_ID, DirectEdge, EdgeFrameProcessor},
        transports::tokio_cobs_stream,
        utils::{cobs_stream, std::StdQueue},
    };
    use mutex::raw_impls::cs::CriticalSectionRawMutex;
    use std::sync::Arc;
    use tokio::io::{AsyncRead, AsyncWrite};

    pub use crate::interface_manager::LivenessConfig;
    pub use crate::interface_manager::utils::std::new_std_queue;
    pub use maitake_sync::WaitQueue;

    use crate::net_stack::ArcNetStack;

    pub type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<TokioStreamInterface>>;

    pub async fn register_target_stream(
        stack: EdgeStack,
        reader: impl AsyncRead + Unpin + Send + 'static,
        writer: impl AsyncWrite + Unpin + Send + 'static,
        queue: StdQueue,
        liveness: Option<LivenessConfig>,
        state_notify: Option<Arc<WaitQueue>>,
    ) -> Result<(), tokio_cobs_stream::EdgeRegistrationError> {
        tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
            stack,
            reader,
            writer,
            queue,
            EdgeFrameProcessor::new(),
            InterfaceState::Inactive,
            liveness,
            state_notify,
        )
        .await
    }

    pub async fn register_controller_stream(
        stack: EdgeStack,
        reader: impl AsyncRead + Unpin + Send + 'static,
        writer: impl AsyncWrite + Unpin + Send + 'static,
        queue: StdQueue,
        liveness: Option<LivenessConfig>,
        state_notify: Option<Arc<WaitQueue>>,
    ) -> Result<(), tokio_cobs_stream::EdgeRegistrationError> {
        tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
            stack,
            reader,
            writer,
            queue,
            EdgeFrameProcessor::new_controller(1),
            InterfaceState::Active {
                net_id: 1,
                node_id: CENTRAL_NODE_ID,
            },
            liveness,
            state_notify,
        )
        .await
    }

    pub fn new_target_stack(queue: &StdQueue, mtu: u16) -> EdgeStack {
        EdgeStack::new_with_profile(DirectEdge::new_target(cobs_stream::Sink::new_from_handle(
            queue.clone(),
            mtu,
        )))
    }

    pub fn new_controller_stack(queue: &StdQueue, mtu: u16) -> EdgeStack {
        EdgeStack::new_with_profile(DirectEdge::new_controller(
            cobs_stream::Sink::new_from_handle(queue.clone(), mtu),
            InterfaceState::Down,
        ))
    }
}

#[cfg(feature = "nusb-v0_1")]
pub mod nusb_v0_1 {
    use crate::interface_manager::{
        interface_impls::nusb_bulk::NusbBulk, profiles::direct_edge::DirectEdge,
        profiles::direct_router::DirectRouter, transports::nusb as nusb_transport,
        utils::std::StdQueue,
    };
    use mutex::raw_impls::cs::CriticalSectionRawMutex;
    use std::sync::Arc;

    use crate::net_stack::ArcNetStack;

    pub use crate::interface_manager::interface_impls::nusb_bulk::{NewDevice, find_new_devices};

    pub type RouterStack = ArcNetStack<CriticalSectionRawMutex, DirectRouter<NusbBulk>>;
    pub type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<NusbBulk>>;

    pub async fn register_router_interface(
        stack: &RouterStack,
        device: NewDevice,
        max_ergot_packet_size: u16,
        outgoing_buffer_size: usize,
    ) -> Result<u8, nusb_transport::RouterRegistrationError> {
        nusb_transport::register_router(
            stack.clone(),
            device,
            max_ergot_packet_size,
            outgoing_buffer_size,
            None,
        )
        .await
    }

    pub async fn register_edge_interface(
        stack: &EdgeStack,
        device: NewDevice,
        queue: &StdQueue,
        max_ergot_packet_size: u16,
        state_notify: Option<Arc<maitake_sync::WaitQueue>>,
    ) -> Result<(), nusb_transport::EdgeRegistrationError> {
        nusb_transport::register_edge::<_, NusbBulk>(
            stack.clone(),
            device,
            queue.clone(),
            max_ergot_packet_size,
            state_notify,
        )
        .await
    }
}

#[cfg(feature = "tokio-serial-v5")]
pub mod tokio_serial_v5 {
    use crate::interface_manager::{
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::direct_edge::DirectEdge,
        profiles::direct_router::DirectRouter,
        transports::tokio_serial,
        utils::{cobs_stream, std::StdQueue},
    };
    use mutex::raw_impls::cs::CriticalSectionRawMutex;
    use std::sync::Arc;

    use crate::net_stack::ArcNetStack;

    pub type RouterStack = ArcNetStack<CriticalSectionRawMutex, DirectRouter<TokioStreamInterface>>;
    pub type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<TokioStreamInterface>>;

    pub async fn register_router_interface(
        stack: &RouterStack,
        port: &str,
        baud: u32,
        max_ergot_packet_size: u16,
        outgoing_buffer_size: usize,
    ) -> Result<u8, tokio_serial::RouterRegistrationError> {
        tokio_serial::register_router(
            stack.clone(),
            port,
            baud,
            max_ergot_packet_size,
            outgoing_buffer_size,
            None,
            None,
        )
        .await
    }

    pub async fn register_edge_interface(
        stack: &EdgeStack,
        port: &str,
        baud: u32,
        queue: &StdQueue,
        liveness: Option<crate::interface_manager::LivenessConfig>,
        state_notify: Option<Arc<maitake_sync::WaitQueue>>,
    ) -> Result<(), tokio_serial::EdgeRegistrationError> {
        tokio_serial::register_edge::<_, TokioStreamInterface>(
            stack.clone(),
            port,
            baud,
            queue.clone(),
            crate::interface_manager::profiles::direct_edge::EdgeFrameProcessor::new(),
            crate::interface_manager::InterfaceState::Inactive,
            liveness,
            state_notify,
        )
        .await
    }

    pub fn new_target_stack(queue: &StdQueue, mtu: u16) -> EdgeStack {
        EdgeStack::new_with_profile(DirectEdge::new_target(cobs_stream::Sink::new_from_handle(
            queue.clone(),
            mtu,
        )))
    }
}
