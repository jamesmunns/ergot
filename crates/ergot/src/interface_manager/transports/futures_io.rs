//! Futures-IO COBS stream RxWorker.
//!
//! Runtime-agnostic transport using `futures_io::AsyncRead`. Works with any
//! async executor (tokio, wasm, embassy via adapters, etc.).
//!
//! Generic over any [`FrameProcessor`], so it works with [`DirectEdge`],
//! [`Router`], or any future profile.
//!
//! [`FrameProcessor`]: crate::interface_manager::FrameProcessor
//! [`DirectEdge`]: crate::interface_manager::profiles::direct_edge::DirectEdge
//! [`Router`]: crate::interface_manager::profiles::router::Router

use core::pin::Pin;

use cobs_acc::{CobsAccumulator, FeedResult};

use crate::{
    interface_manager::{FrameProcessor, InterfaceState, Profile},
    net_stack::NetStackHandle,
};

/// Async read helper: wraps `futures_io::AsyncRead::poll_read` into a future.
async fn async_read<R: futures_io::AsyncRead + Unpin>(
    reader: &mut R,
    buf: &mut [u8],
) -> Result<usize, std::io::Error> {
    core::future::poll_fn(|cx| Pin::new(&mut *reader).poll_read(cx, buf)).await
}

/// A generic futures-io COBS stream RxWorker.
///
/// Reads bytes from a `futures_io::AsyncRead` source, decodes COBS
/// frames, and feeds them to a [`FrameProcessor`].
pub struct RxWorker<N, R, P>
where
    N: NetStackHandle,
    R: futures_io::AsyncRead + Unpin,
    P: FrameProcessor<N>,
{
    nsh: N,
    rx: R,
    processor: P,
    ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
}

impl<N, R, P> RxWorker<N, R, P>
where
    N: NetStackHandle,
    R: futures_io::AsyncRead + Unpin,
    P: FrameProcessor<N>,
{
    /// Create a new RX worker.
    ///
    /// `processor` handles decoded frames (profile-specific logic).
    /// `ident` is the interface identifier used for state management.
    pub fn new(
        nsh: N,
        rx: R,
        processor: P,
        ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
    ) -> Self {
        Self {
            nsh,
            rx,
            processor,
            ident,
        }
    }

    /// Run the receive loop with an initial state.
    ///
    /// Sets `initial_state` on the interface before entering the frame
    /// loop. On exit (transport error or drop), the interface is set to
    /// [`InterfaceState::Down`].
    pub async fn run(
        &mut self,
        initial_state: InterfaceState,
        frame: &mut [u8],
        scratch: &mut [u8],
    ) -> Result<(), std::io::Error> {
        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state(self.ident.clone(), initial_state));

        let res = self.run_inner(frame, scratch).await;

        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state(self.ident.clone(), InterfaceState::Down));

        res
    }

    async fn run_inner(
        &mut self,
        frame: &mut [u8],
        scratch: &mut [u8],
    ) -> Result<(), std::io::Error> {
        let mut acc = CobsAccumulator::new(frame);

        'outer: loop {
            let used = async_read(&mut self.rx, scratch).await?;
            if used == 0 {
                // EOF — peer closed the connection
                return Ok(());
            }

            let mut remain = &mut scratch[..used];

            loop {
                match acc.feed_raw(remain) {
                    FeedResult::Consumed => continue 'outer,
                    FeedResult::OverFull(items) => {
                        remain = items;
                    }
                    FeedResult::DecodeError(items) => {
                        remain = items;
                    }
                    FeedResult::Success { data, remaining }
                    | FeedResult::SuccessInput { data, remaining } => {
                        self.processor
                            .process_frame(data, &self.nsh, self.ident.clone());
                        remain = remaining;
                    }
                }
            }
        }
    }
}

impl<N, R, P> Drop for RxWorker<N, R, P>
where
    N: NetStackHandle,
    R: futures_io::AsyncRead + Unpin,
    P: FrameProcessor<N>,
{
    fn drop(&mut self) {
        self.nsh.stack().manage_profile(|im| {
            _ = im.set_interface_state(self.ident.clone(), InterfaceState::Down);
        });
    }
}

/// Transmitter worker task.
///
/// Reads COBS-encoded frames from a bbqueue consumer and writes them
/// to a `futures_io::AsyncWrite` sink.
pub async fn tx_worker<W, Q>(
    tx: &mut W,
    rx: bbqueue::prod_cons::stream::StreamConsumer<Q>,
) -> Result<(), std::io::Error>
where
    W: futures_io::AsyncWrite + Unpin,
    Q: bbqueue::traits::bbqhdl::BbqHandle,
    Q::Notifier: bbqueue::traits::notifier::AsyncNotifier,
{
    loop {
        let data = rx.wait_read().await;
        let len = data.len();
        if len == 0 {
            return Ok(());
        }
        async_write_all(tx, &data).await?;
        data.release(len);
    }
}

/// Async write helper: writes all bytes, handling partial writes.
async fn async_write_all<W: futures_io::AsyncWrite + Unpin>(
    writer: &mut W,
    mut buf: &[u8],
) -> Result<(), std::io::Error> {
    while !buf.is_empty() {
        let n = core::future::poll_fn(|cx| Pin::new(&mut *writer).poll_write(cx, buf)).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "write zero",
            ));
        }
        buf = &buf[n..];
    }
    // Flush after each frame batch
    core::future::poll_fn(|cx| Pin::new(&mut *writer).poll_flush(cx)).await?;
    Ok(())
}
