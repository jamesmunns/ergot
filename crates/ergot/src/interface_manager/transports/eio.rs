//! Embedded-IO COBS stream RxWorker.
//!
//! Generic over any [`FrameProcessor`], so it works with [`DirectEdge`],
//! [`Router`], or any future profile.
//!
//! [`FrameProcessor`]: crate::interface_manager::FrameProcessor
//! [`DirectEdge`]: crate::interface_manager::profiles::direct_edge::DirectEdge
//! [`Router`]: crate::interface_manager::profiles::router::Router

use cobs_acc::{CobsAccumulator, FeedResult};

use crate::{
    eio::Read,
    interface_manager::{FrameProcessor, InterfaceState, Profile},
    net_stack::NetStackHandle,
};

#[cfg(feature = "embassy-time")]
use crate::interface_manager::LivenessConfig;
#[cfg(feature = "embassy-time")]
use maitake_sync::WaitQueue;

/// A generic embedded-io COBS stream RxWorker.
///
/// Reads bytes from an `embedded_io_async::Read` source, decodes COBS
/// frames, and feeds them to a [`FrameProcessor`].
///
/// Supports optional liveness timeout and state change notifications
/// (requires `embassy-time` feature).
pub struct RxWorker<N, R, P>
where
    N: NetStackHandle,
    R: Read,
    P: FrameProcessor<N>,
{
    nsh: N,
    rx: R,
    processor: P,
    ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
    #[cfg(feature = "embassy-time")]
    liveness: Option<LivenessConfig>,
    #[cfg(feature = "embassy-time")]
    state_notify: Option<&'static WaitQueue>,
    #[cfg(feature = "embassy-time")]
    have_received: bool,
    #[cfg(feature = "embassy-time")]
    needs_cobs_reset: bool,
}

impl<N, R, P> RxWorker<N, R, P>
where
    N: NetStackHandle,
    R: Read,
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
            #[cfg(feature = "embassy-time")]
            liveness: None,
            #[cfg(feature = "embassy-time")]
            state_notify: None,
            #[cfg(feature = "embassy-time")]
            have_received: false,
            #[cfg(feature = "embassy-time")]
            needs_cobs_reset: false,
        }
    }

    /// Set liveness tracking configuration.
    ///
    /// When enabled, the RxWorker transitions the interface to `Inactive`
    /// if no frames are received within `config.timeout_ms`. The timer
    /// only starts after the first frame is received. Recovery is
    /// automatic — when frames resume, the processor transitions back
    /// to `Active`.
    #[cfg(feature = "embassy-time")]
    pub fn with_liveness(mut self, config: LivenessConfig) -> Self {
        self.liveness = Some(config);
        self
    }

    /// Set a [`WaitQueue`] to be notified on interface state transitions.
    #[cfg(feature = "embassy-time")]
    pub fn with_state_notify(mut self, notify: &'static WaitQueue) -> Self {
        self.state_notify = Some(notify);
        self
    }

    /// Notify the state observer, if configured.
    #[cfg(feature = "embassy-time")]
    fn notify(&self) {
        if let Some(notify) = self.state_notify {
            notify.wake_all();
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
    ) -> Result<(), R::Error> {
        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state(self.ident.clone(), initial_state));
        #[cfg(feature = "embassy-time")]
        self.notify();

        let res = self.run_inner(frame, scratch).await;

        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state(self.ident.clone(), InterfaceState::Down));
        #[cfg(feature = "embassy-time")]
        self.notify();

        res
    }

    async fn run_inner(&mut self, frame: &mut [u8], scratch: &mut [u8]) -> Result<(), R::Error> {
        let mut acc = CobsAccumulator::new(frame);

        'outer: loop {
            let used = self.read_or_timeout(scratch).await?;

            // After liveness timeout, flush stale COBS state
            #[cfg(feature = "embassy-time")]
            if self.needs_cobs_reset {
                acc.reset();
                self.needs_cobs_reset = false;
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
                        #[allow(unused_variables)]
                        let changed =
                            self.processor
                                .process_frame(data, &self.nsh, self.ident.clone());
                        #[cfg(feature = "embassy-time")]
                        {
                            self.have_received = true;
                        }
                        #[cfg(feature = "embassy-time")]
                        if changed {
                            self.notify();
                        }
                        remain = remaining;
                    }
                }
            }
        }
    }

    /// Read from the transport with optional liveness timeout.
    ///
    /// Without `embassy-time` feature, this is a plain read.
    /// With `embassy-time` and liveness configured, transitions to `Inactive`
    /// on timeout (only after first frame received). Loops back waiting for
    /// frames or another timeout — the transport read error is the only exit.
    async fn read_or_timeout(&mut self, scratch: &mut [u8]) -> Result<usize, R::Error> {
        #[cfg(feature = "embassy-time")]
        {
            loop {
                let liveness_active = self.liveness.is_some() && self.have_received;
                if liveness_active {
                    let timeout_ms = self.liveness.as_ref().unwrap().timeout_ms;
                    let duration = embassy_time::Duration::from_millis(timeout_ms);
                    match embassy_time::with_timeout(duration, self.rx.read(scratch)).await {
                        Ok(result) => return result,
                        Err(_timeout) => {
                            let changed = self.nsh.stack().manage_profile(|im| {
                                if matches!(
                                    im.interface_state(self.ident.clone()),
                                    Some(InterfaceState::Active { .. })
                                ) {
                                    _ = im.set_interface_state(
                                        self.ident.clone(),
                                        InterfaceState::Inactive,
                                    );
                                    true
                                } else {
                                    false
                                }
                            });
                            if changed {
                                self.notify();
                            }
                            self.processor.reset();
                            self.have_received = false;
                            self.needs_cobs_reset = true;
                            continue;
                        }
                    }
                } else {
                    return self.rx.read(scratch).await;
                }
            }
        }
        #[cfg(not(feature = "embassy-time"))]
        {
            self.rx.read(scratch).await
        }
    }
}

impl<N, R, P> Drop for RxWorker<N, R, P>
where
    N: NetStackHandle,
    R: Read,
    P: FrameProcessor<N>,
{
    fn drop(&mut self) {
        self.nsh.stack().manage_profile(|im| {
            _ = im.set_interface_state(self.ident.clone(), InterfaceState::Down);
        });
        #[cfg(feature = "embassy-time")]
        self.notify();
    }
}
