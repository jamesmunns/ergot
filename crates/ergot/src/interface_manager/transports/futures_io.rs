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
//!   whenever this worker changes the interface state (frame processing,
//!   liveness, exit); see [`NetStack::wait_profile`](crate::NetStack::wait_profile)
//!   for changes made elsewhere.
//! - **Liveness timeout**: `RxWorker::run_with_liveness`, timed by the
//!   [time backend](crate::time) (`tokio-std`, `wasm`, ...).
//!
//! The caller is responsible for setting the initial interface state before
//! running the worker. On exit (or drop), the interface is set to
//! [`InterfaceState::Down`].
//!
//! [`FrameProcessor`]: crate::interface_manager::FrameProcessor
//! [`DirectEdge`]: crate::interface_manager::profiles::direct_edge::DirectEdge
//! [`Router`]: crate::interface_manager::profiles::router::Router

use core::{future::pending, pin::Pin};
use std::sync::Arc;

use cobs_acc::{CobsAccumulator, FeedResult};
use embassy_futures::select::{Either3, select3};
use maitake_sync::WaitQueue;

#[cfg(time_sleep)]
use crate::time::{Duration, sleep};
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
    link_local_on_timeout: bool,
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
            link_local_on_timeout: false,
        }
    }

    /// On a liveness timeout, revert the interface to link-local addressing
    /// ([`InterfaceState::link_local`], keeping its node_id) instead of
    /// [`InterfaceState::Inactive`]. On a point-to-point link that is the edge
    /// boot state ([`InterfaceState::edge_link_local`]); a bus device keeps
    /// the node_id it claimed.
    ///
    /// Use this for an edge or bridge upstream. `Inactive` gates transmit until
    /// frames resume, which is correct for a downstream peer but wrong for an
    /// upstream: a quiet upstream still needs to send (e.g. a link-local ping)
    /// to provoke the frame that re-discovers its net_id. Reverting to
    /// link-local keeps transmit ungated so recovery can proceed; the processor
    /// is reset either way, so the next inbound frame re-discovers the net_id.
    ///
    /// Trade-off: with this policy the interface state alone no longer
    /// distinguishes "link dead" from "alive but not yet (re)discovered" —
    /// both read as `Active { net_id: 0 }`. Liveness diagnostics move to logs
    /// or counters.
    pub fn revert_to_link_local_on_timeout(mut self) -> Self {
        self.link_local_on_timeout = true;
        self
    }

    /// End the run loop when `closer` is woken or closed.
    ///
    /// Allows graceful shutdown coordinated with a TX worker sharing
    /// the same closer.
    pub fn with_closer(mut self, closer: Arc<WaitQueue>) -> Self {
        self.closer = Some(closer);
        self
    }

    /// Wake `notify` whenever this worker changes the interface state (e.g.
    /// a frame activates the interface, or a liveness timeout deactivates
    /// it). Changes made elsewhere, such as the bus address claim, do not
    /// reach it; wait with
    /// [`NetStack::wait_profile`](crate::NetStack::wait_profile) to see
    /// every change.
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
        let res = self.run_inner(frame, scratch, None).await;
        self.set_down();
        res
    }

    /// Run the receive loop with a liveness timeout (needs a [time
    /// backend](crate::time)).
    ///
    /// Once at least one frame has been received, going `liveness.timeout_ms`
    /// milliseconds without a frame transitions the interface to
    /// [`InterfaceState::Inactive`] and resets the processor; the loop keeps
    /// running and recovers when frames resume.
    #[cfg(time_sleep)]
    pub async fn run_with_liveness(
        &mut self,
        frame: &mut [u8],
        scratch: &mut [u8],
        liveness: LivenessConfig,
    ) -> Result<RxEnd, std::io::Error> {
        let res = self.run_inner(frame, scratch, Some(liveness)).await;
        self.set_down();
        res
    }

    async fn run_inner(
        &mut self,
        frame: &mut [u8],
        scratch: &mut [u8],
        liveness: Option<LivenessConfig>,
    ) -> Result<RxEnd, std::io::Error> {
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
                    None => pending().await,
                }
            };
            let timeout_fut = async {
                #[cfg(time_sleep)]
                if let Some(cfg) = &liveness
                    && have_received
                {
                    return sleep(Duration::from_millis(cfg.timeout_ms)).await;
                }
                // No time backend: `run_with_liveness` does not exist.
                #[cfg(not(time_sleep))]
                let _ = (&liveness, have_received);
                pending().await
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

    /// Handle a liveness timeout: move the interface out of `Active` (if it is
    /// active) and reset the processor so the next frame triggers re-discovery.
    ///
    /// The target state is [`InterfaceState::Inactive`] by default, or
    /// link-local with the current node_id if [`revert_to_link_local_on_timeout`] was
    /// set (see that method for the rationale).
    ///
    /// [`revert_to_link_local_on_timeout`]: Self::revert_to_link_local_on_timeout
    fn liveness_timeout(&mut self) {
        let link_local = self.link_local_on_timeout;
        let changed = self.nsh.stack().manage_profile(|im| {
            let current = im.interface_state(self.ident.clone());
            let Some(InterfaceState::Active { node_id, .. }) = current else {
                return false;
            };
            // Link-local keeps the node_id: a bus device must not fall back
            // to the point-to-point EDGE_NODE_ID.
            let target = if link_local {
                InterfaceState::link_local(node_id)
            } else {
                InterfaceState::Inactive
            };
            if current != Some(target) {
                _ = im.set_interface_state(self.ident.clone(), target);
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
