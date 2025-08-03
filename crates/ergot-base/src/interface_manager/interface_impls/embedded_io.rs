use bbq2::{
    prod_cons::stream::StreamConsumer,
    queue::BBQueue,
    traits::{
        bbqhdl::BbqHandle, coordination::Coord, notifier::maitake::MaiNotSpsc, storage::Inline,
    },
};
use core::marker::PhantomData;
use defmt::info;
use embedded_io_async_0_6::Write;

use crate::interface_manager::{Interface, utils::cobs_stream};

pub struct IoInterface<Q: BbqHandle> {
    _pd: PhantomData<Q>,
}

/// A type alias for the outgoing packet queue typically used by the [`EmbassyInterface`]
pub type Queue<const N: usize, C> = BBQueue<Inline<N>, C, MaiNotSpsc>;
/// A type alias for the InterfaceSink typically used by the [`EmbassyInterface`]
pub type SerialSink<Q> = cobs_stream::Sink<Q>;

/// Interface Implementation
impl<Q: BbqHandle + 'static> Interface for IoInterface<Q> {
    type Sink = SerialSink<Q>;
}

/// Transmitter worker task
///
/// Takes a bbqueue from the NetStack of packets to send. While sending,
/// we will timeout if
pub async fn tx_worker<'a, O: Write, const N: usize, C: Coord>(
    tx: &mut O,
    rx: StreamConsumer<&'static BBQueue<Inline<N>, C, MaiNotSpsc>>,
) -> Result<(), O::Error> {
    info!("Started tx_worker");
    loop {
        let data = rx.wait_read().await;
        let used = tx.write(&data).await?;
        data.release(used);
        if used == 0 {
            return Ok(());
        }
    }
}
