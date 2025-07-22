use core::marker::PhantomData;

use bbq2::{
    queue::BBQueue,
    traits::{coordination::Coord, notifier::maitake::MaiNotSpsc, storage::Inline},
};

use crate::interface_manager::{Interface, utils::framed_stream};

pub struct EmbassyInterface<const N: usize, C: Coord + 'static> {
    _pd: PhantomData<C>,
}
pub type Queue<const N: usize, C> = BBQueue<Inline<N>, C, MaiNotSpsc>;
pub type EmbassySink<const N: usize, C> = framed_stream::Sink<&'static Queue<N, C>>;

impl<const N: usize, C: Coord> Interface for EmbassyInterface<N, C> {
    type Sink = EmbassySink<N, C>;
}
