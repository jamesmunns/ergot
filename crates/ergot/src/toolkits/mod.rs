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
