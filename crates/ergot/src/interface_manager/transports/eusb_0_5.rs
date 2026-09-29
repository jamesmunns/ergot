//! USB bulk endpoint RxWorker (embassy-usb v0.5).
//!
//! Generic over any [`FrameProcessor`], so it works with [`DirectEdge`],
//! [`Router`], or any future profile.
//!
//! [`FrameProcessor`]: crate::interface_manager::FrameProcessor
//! [`DirectEdge`]: crate::interface_manager::profiles::direct_edge::DirectEdge
//! [`Router`]: crate::interface_manager::profiles::router::Router

use crate::logging::info;
use crate::{
    interface_manager::{
        FrameProcessor, InterfaceState, LivenessConfig, Profile,
        interface_impls::embassy_usb::USB_SUSPEND, transports::link::Link,
    },
    net_stack::NetStackHandle,
    time::{Duration, Instant, sleep_until},
};
use embassy_futures::select::{Either3, select3};
use embassy_usb_0_5::driver::{Driver, Endpoint, EndpointError, EndpointOut};
use maitake_sync::WaitQueue;

/// A generic USB bulk endpoint RxWorker.
///
/// Reads USB bulk frames, detects USB suspend/resume via
/// `USB_SUSPEND` watch, and feeds decoded frames to a
/// [`FrameProcessor`].
pub struct RxWorker<N, D, P>
where
    N: NetStackHandle,
    D: Driver<'static>,
    P: FrameProcessor<N>,
{
    link: Link<N>,
    rx: D::EndpointOut,
    processor: P,
    liveness: Option<LivenessConfig>,
}

/// Errors observable by the receiver
enum ReceiverError {
    ReceivedMessageTooLarge,
    ConnectionClosed,
}

impl<N, D, P> RxWorker<N, D, P>
where
    N: NetStackHandle,
    D: Driver<'static>,
    P: FrameProcessor<N>,
{
    /// Create a new receiver object.
    ///
    /// `processor` handles decoded frames (profile-specific logic).
    /// `ident` is the interface identifier used for state management.
    pub fn new(
        nsh: N,
        rx: D::EndpointOut,
        processor: P,
        ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
    ) -> Self {
        Self {
            link: Link::new(nsh, ident),
            rx,
            processor,
            liveness: None,
        }
    }

    /// Enable liveness tracking with the given timeout.
    ///
    /// When enabled, the RxWorker transitions the interface to
    /// [`InterfaceState::Inactive`] if no frames are received within
    /// `config.timeout_ms`. The timer only starts after the first frame
    /// is received. Recovery is automatic — when frames resume,
    /// the processor transitions back to [`InterfaceState::Active`].
    pub fn with_liveness(mut self, config: LivenessConfig) -> Self {
        self.liveness = Some(config);
        self
    }

    /// Set a [`WaitQueue`] to be notified on interface state transitions.
    pub fn with_state_notify(mut self, notify: &'static WaitQueue) -> Self {
        self.link.set_state_notify(notify);
        self
    }

    /// Runs forever, processing incoming frames.
    ///
    /// The provided slice is used for receiving a frame via USB. It is used as the MTU
    /// for the entire connection.
    ///
    /// `max_usb_frame_size` is the largest size of USB frame we can receive. For example,
    /// it would be 64. This is NOT the largest message we can receive. It MUST be a power
    /// of two.
    pub async fn run(mut self, frame: &mut [u8], max_usb_frame_size: usize) -> ! {
        assert!(max_usb_frame_size.is_power_of_two());
        let mut suspend_rx = USB_SUSPEND
            .receiver()
            .expect("USB_SUSPEND watch receiver slot exhausted");
        loop {
            self.rx.wait_enabled().await;

            // Clear any stale suspend state from a previous connection
            USB_SUSPEND.sender().send(false);

            info!("Connection established");

            // Mark the interface as established with link-local addressing
            // (net_id=0). The real net_id is discovered from the first incoming frame.
            // Skip if already Active (e.g. a Router downstream that was assigned a
            // net_id via seed routing before USB physically connected).
            _ = self.link.nsh.stack().manage_profile(|im| {
                if matches!(
                    im.interface_state(self.link.ident.clone()),
                    Some(InterfaceState::Active { .. })
                ) {
                    return Ok(());
                }
                im.set_interface_state(
                    self.link.ident.clone(),
                    InterfaceState::Active {
                        net_id: 0,
                        node_id: crate::interface_manager::edge_port::EDGE_NODE_ID,
                    },
                )
            });
            self.link.notify();

            // Handle all frames for the connection
            self.one_conn(frame, max_usb_frame_size, &mut suspend_rx)
                .await;

            // Mark the connection as lost
            info!("Connection lost");
            self.link.set_state(InterfaceState::Down);
        }
    }

    /// Handle all frames, returning when a connection error occurs.
    ///
    /// Uses [`USB_SUSPEND`] watch for instant USB suspend/resume detection
    /// and an optional liveness timer for no-data timeout.
    async fn one_conn(
        &mut self,
        frame: &mut [u8],
        max_usb_frame_size: usize,
        suspend_rx: &mut embassy_sync::watch::Receiver<
            'static,
            embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
            bool,
            2,
        >,
    ) {
        // When the last frame arrived, once one has: the liveness deadline
        // runs from there, across suspend/resume events in between.
        let mut last_data_at: Option<Instant> = None;

        loop {
            let deadline = match (&self.liveness, last_data_at) {
                (Some(config), Some(at)) => Some(at + Duration::from_millis(config.timeout_ms)),
                _ => None,
            };
            let timeout = async {
                match deadline {
                    Some(deadline) => sleep_until(deadline).await,
                    None => core::future::pending::<()>().await,
                }
            };

            match select3(
                Self::one_frame(&mut self.rx, frame, max_usb_frame_size),
                suspend_rx.changed(),
                timeout,
            )
            .await
            {
                // Frame received
                Either3::First(Ok(f)) => {
                    last_data_at = Some(Instant::now());
                    let changed =
                        self.processor
                            .process_frame(f, &self.link.nsh, self.link.ident.clone());
                    if changed {
                        self.link.notify();
                    }
                }
                Either3::First(Err(ReceiverError::ConnectionClosed)) => break,
                Either3::First(Err(_e)) => continue,

                // USB suspend/resume — instant wakeup from Handler
                Either3::Second(suspended) => {
                    if suspended {
                        info!("USB suspended, marking interface inactive");
                        self.link.deactivate(false);
                        self.processor.reset();
                        last_data_at = None;
                    }
                    // If not suspended (resume event), just continue —
                    // recovery happens automatically via process_frame
                }

                // Liveness timeout — no data for configured duration
                Either3::Third(()) => {
                    info!("USB liveness timeout, marking interface inactive");
                    self.link.deactivate(false);
                    self.processor.reset();
                    last_data_at = None;
                }
            }
        }
    }

    /// Receive a single ergot frame, which might be across multiple reads of the endpoint.
    ///
    /// No checking of the frame is done, only that the bulk endpoint gave us a frame.
    async fn one_frame<'a>(
        rx: &mut D::EndpointOut,
        frame: &'a mut [u8],
        max_frame_len: usize,
    ) -> Result<&'a mut [u8], ReceiverError> {
        let buflen = frame.len();
        let mut window = &mut frame[..];

        while !window.is_empty() {
            let n = match rx.read(window).await {
                Ok(n) => n,
                Err(EndpointError::BufferOverflow) => {
                    return Err(ReceiverError::ReceivedMessageTooLarge);
                }
                Err(EndpointError::Disabled) => return Err(ReceiverError::ConnectionClosed),
            };

            let (_now, later) = window.split_at_mut(n);
            window = later;
            if n != max_frame_len {
                // We now have a full frame
                let wlen = window.len();
                let len = buflen - wlen;
                return Ok(&mut frame[..len]);
            }
        }

        // `frame` is completely full of max-size packets. Peek the next packet to
        // tell a complete frame from an over-length one:
        //   - a zero-length packet terminates a frame whose length is an exact
        //     multiple of the max packet size — since the buffer is full, that
        //     frame is exactly `buflen` bytes and complete (a 0-byte read writes
        //     nothing, so `frame` still holds it);
        //   - any non-empty packet is data beyond the buffer, so the frame is
        //     genuinely too large.
        match rx.read(frame).await {
            Ok(0) => return Ok(&mut frame[..buflen]),
            Ok(n) if n == max_frame_len => {}
            Ok(_) => return Err(ReceiverError::ReceivedMessageTooLarge),
            Err(EndpointError::BufferOverflow) => {
                return Err(ReceiverError::ReceivedMessageTooLarge);
            }
            Err(EndpointError::Disabled) => return Err(ReceiverError::ConnectionClosed),
        };

        // The frame is too large. Drain the rest of it to its terminator so the
        // endpoint is left clean for the next frame, then report the error.
        loop {
            match rx.read(frame).await {
                Ok(n) if n == max_frame_len => {}
                Ok(_) => return Err(ReceiverError::ReceivedMessageTooLarge),
                Err(EndpointError::BufferOverflow) => {
                    return Err(ReceiverError::ReceivedMessageTooLarge);
                }
                Err(EndpointError::Disabled) => return Err(ReceiverError::ConnectionClosed),
            };
        }
    }
}
