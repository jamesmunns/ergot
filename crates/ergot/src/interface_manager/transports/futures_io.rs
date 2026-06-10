//! Futures-IO COBS stream transport.
//!
//! Runtime-agnostic transport using `futures_io::AsyncRead`/`AsyncWrite`.
//! Works with any async executor (tokio via compat, wasm-bindgen-futures,
//! smol, etc.).
//!
//! Generic over any [`FrameProcessor`], so it works with [`DirectEdge`],
//! [`Router`], or any future profile.
//!
//! Optional features are injected rather than tied to a runtime:
//! - **Graceful shutdown**: [`RxWorker::with_closer`] — a
//!   [`maitake_sync::WaitQueue`] that ends the loop when woken or closed.
//! - **State change notifications**: [`RxWorker::with_state_notify`] — woken
//!   whenever the interface state changes (frame processing or liveness).
//! - **Liveness timeout**: [`RxWorker::run_with_liveness`] — takes a
//!   `sleeper` closure so any runtime's timer can drive it (e.g.
//!   `tokio::time::sleep`, `gloo_timers::future::sleep`).
//!
//! The caller is responsible for setting the initial interface state before
//! running the worker. On exit (or drop), the interface is set to
//! [`InterfaceState::Down`].
//!
//! [`FrameProcessor`]: crate::interface_manager::FrameProcessor
//! [`DirectEdge`]: crate::interface_manager::profiles::direct_edge::DirectEdge
//! [`Router`]: crate::interface_manager::profiles::router::Router

use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

use cobs_acc::{CobsAccumulator, FeedResult};
use embassy_futures::select::{Either3, select3};
use maitake_sync::WaitQueue;

use crate::{
    interface_manager::{FrameProcessor, InterfaceState, LivenessConfig, Profile},
    net_stack::NetStackHandle,
};

/// Async read helper: wraps `futures_io::AsyncRead::poll_read` into a future.
async fn async_read<R: futures_io::AsyncRead + Unpin>(
    reader: &mut R,
    buf: &mut [u8],
) -> Result<usize, std::io::Error> {
    core::future::poll_fn(|cx| Pin::new(&mut *reader).poll_read(cx, buf)).await
}

/// Why an [`RxWorker`] run loop ended (without a transport error).
#[derive(Debug, PartialEq)]
pub enum RxEnd {
    /// The peer closed the connection (read returned 0 bytes).
    Eof,
    /// The closer was woken or closed.
    Closed,
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
    closer: Option<Arc<WaitQueue>>,
    state_notify: Option<Arc<WaitQueue>>,
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
            closer: None,
            state_notify: None,
        }
    }

    /// End the run loop when `closer` is woken or closed.
    ///
    /// Allows graceful shutdown coordinated with a TX worker sharing
    /// the same closer.
    pub fn with_closer(mut self, closer: Arc<WaitQueue>) -> Self {
        self.closer = Some(closer);
        self
    }

    /// Wake `notify` whenever the interface state changes (e.g. a frame
    /// activates the interface, or a liveness timeout deactivates it).
    pub fn with_state_notify(mut self, notify: Arc<WaitQueue>) -> Self {
        self.state_notify = Some(notify);
        self
    }

    fn notify(&self) {
        if let Some(notify) = &self.state_notify {
            notify.wake_all();
        }
    }

    /// Run the receive loop.
    ///
    /// The caller must set the interface state before calling. On exit
    /// (transport error, EOF, closer, or drop), the interface is set to
    /// [`InterfaceState::Down`].
    pub async fn run(
        &mut self,
        frame: &mut [u8],
        scratch: &mut [u8],
    ) -> Result<RxEnd, std::io::Error> {
        let res = self
            .run_inner(frame, scratch, None::<(_, fn(u64) -> core::future::Pending<()>)>)
            .await;
        self.set_down();
        res
    }

    /// Run the receive loop with a liveness timeout.
    ///
    /// Once at least one frame has been received, going `liveness.timeout_ms`
    /// milliseconds without a frame transitions the interface to
    /// [`InterfaceState::Inactive`] and resets the processor; the loop keeps
    /// running and recovers when frames resume.
    ///
    /// `sleeper` provides the timer: a closure from milliseconds to a future
    /// that resolves after that long (e.g.
    /// `|ms| tokio::time::sleep(Duration::from_millis(ms))`).
    pub async fn run_with_liveness<S, F>(
        &mut self,
        frame: &mut [u8],
        scratch: &mut [u8],
        liveness: LivenessConfig,
        sleeper: S,
    ) -> Result<RxEnd, std::io::Error>
    where
        S: Fn(u64) -> F,
        F: Future<Output = ()>,
    {
        let res = self
            .run_inner(frame, scratch, Some((liveness, sleeper)))
            .await;
        self.set_down();
        res
    }

    async fn run_inner<S, F>(
        &mut self,
        frame: &mut [u8],
        scratch: &mut [u8],
        liveness: Option<(LivenessConfig, S)>,
    ) -> Result<RxEnd, std::io::Error>
    where
        S: Fn(u64) -> F,
        F: Future<Output = ()>,
    {
        let mut acc = CobsAccumulator::new(frame);
        let closer = self.closer.clone();
        let mut have_received = false;

        loop {
            let close_fut = async {
                match &closer {
                    Some(c) => {
                        // Both a wake and a close mean "shut down".
                        let _ = c.wait().await;
                    }
                    None => core::future::pending().await,
                }
            };
            let timeout_fut = async {
                match &liveness {
                    Some((cfg, sleeper)) if have_received => sleeper(cfg.timeout_ms).await,
                    _ => core::future::pending().await,
                }
            };

            let used =
                match select3(async_read(&mut self.rx, scratch), close_fut, timeout_fut).await {
                    Either3::First(res) => res?,
                    Either3::Second(()) => return Ok(RxEnd::Closed),
                    Either3::Third(()) => {
                        self.liveness_timeout();
                        have_received = false;
                        acc.reset();
                        continue;
                    }
                };
            if used == 0 {
                // EOF — peer closed the connection
                return Ok(RxEnd::Eof);
            }

            let mut remain = &mut scratch[..used];

            while !remain.is_empty() {
                remain = match acc.feed_raw(remain) {
                    FeedResult::Consumed => break,
                    FeedResult::OverFull(items) => items,
                    FeedResult::DecodeError(items) => items,
                    FeedResult::Success { data, remaining }
                    | FeedResult::SuccessInput { data, remaining } => {
                        let changed =
                            self.processor
                                .process_frame(data, &self.nsh, self.ident.clone());
                        have_received = true;
                        if changed {
                            self.notify();
                        }
                        remaining
                    }
                };
            }
        }
    }

    /// Handle a liveness timeout: deactivate the interface (if active)
    /// and reset the processor so the next frame triggers re-discovery.
    fn liveness_timeout(&mut self) {
        let changed = self.nsh.stack().manage_profile(|im| {
            if matches!(
                im.interface_state(self.ident.clone()),
                Some(InterfaceState::Active { .. })
            ) {
                _ = im.set_interface_state(self.ident.clone(), InterfaceState::Inactive);
                true
            } else {
                false
            }
        });
        if changed {
            self.notify();
        }
        self.processor.reset();
    }

    fn set_down(&self) {
        let changed = self.nsh.stack().manage_profile(|im| {
            let was_down = matches!(
                im.interface_state(self.ident.clone()),
                Some(InterfaceState::Down) | None
            );
            _ = im.set_interface_state(self.ident.clone(), InterfaceState::Down);
            !was_down
        });
        if changed {
            self.notify();
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
        self.set_down();
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
