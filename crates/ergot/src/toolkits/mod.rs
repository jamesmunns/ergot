pub mod embassy_usb_v0_5 {
    use ergot_base::{
        exports::bbq2::{
            queue::BBQueue,
            traits::{coordination::Coord, notifier::maitake::MaiNotSpsc, storage::Inline},
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
    pub type Stack<const N: usize, C, R> = NetStack<R, EmbassyUsbManager<&'static Queue<N, C>>>;
    pub type BaseStack<const N: usize, C, R> =
        ergot_base::NetStack<R, EmbassyUsbManager<&'static Queue<N, C>>>;
    pub type RxWorker<const N: usize, C, R, D> =
        eusb_0_5::RxWorker<&'static Queue<N, C>, &'static BaseStack<N, C, R>, D>;

    pub const fn new_target_stack<const N: usize, C, R>(
        queue: &'static Queue<N, C>,
        mtu: u16,
    ) -> Stack<N, C, R>
    where
        R: ScopedRawMutex + ConstInit + 'static,
        C: Coord + 'static,
    {
        NetStack::new_with_profile(DirectEdge::new_target(Sink::new(
            queue.framed_producer(),
            mtu,
        )))
    }
}
