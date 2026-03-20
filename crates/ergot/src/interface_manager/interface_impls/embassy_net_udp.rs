use core::marker::PhantomData;

use bbqueue::BBQueue;
use bbqueue::traits::bbqhdl::BbqHandle;
use bbqueue::traits::notifier::maitake::MaiNotSpsc;
use bbqueue::traits::storage::Inline;

use crate::interface_manager::Interface;
use crate::interface_manager::utils::framed_stream;

/// A type alias for the outgoing packet queue typically used by the [`EmbassyNetInterface`]
pub type Queue<const N: usize, C> = BBQueue<Inline<N>, C, MaiNotSpsc>;
/// A type alias for the InterfaceSink typically used by the [`EmbassyNetInterface`]
pub type EmbassySink<Q> = framed_stream::Sink<Q>;

/// An Embassy-USB interface implementation
pub struct EmbassyNetInterface<Q: BbqHandle + 'static> {
    _pd: PhantomData<Q>,
}

/// Interface Implementation
impl<Q: BbqHandle + 'static> Interface for EmbassyNetInterface<Q> {
    type Sink = EmbassySink<Q>;
}

#[cfg(feature = "embassy-net-v0_7")]
pub mod enet_0_7 {}
