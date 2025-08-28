//! A point to point "Edge" profile using USB bulk packets
//!
//! This is useful for devices that are directly connected to a PC via USB with
//! no additional interfaces.

use crate::{
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::embassy_usb::EmbassyInterface,
        profiles::direct_edge::{DirectEdge, process_frame},
    },
    net_stack::NetStackHandle,
};
use bbq2::traits::bbqhdl::BbqHandle;
use defmt::info;
use embassy_usb_0_4::driver::{Driver, Endpoint, EndpointError, EndpointOut};

pub type EmbassyUsbManager<Q> = DirectEdge<EmbassyInterface<Q>>;

/// The Receive Worker
///
/// This manages the receiver operations, as well as manages the connection state.
///
/// The `N` const generic buffer is the size of the outgoing buffer.
pub struct RxWorker<Q, N, D>
where
    N: NetStackHandle<Profile = EmbassyUsbManager<Q>>,
    Q: BbqHandle + 'static,
    D: Driver<'static>,
{
    nsh: N,
    rx: D::EndpointOut,
    net_id: Option<u16>,
}

/// Errors observable by the receiver
enum ReceiverError {
    ReceivedMessageTooLarge,
    ConnectionClosed,
}

// ---- impls ----

impl<Q, N, D> RxWorker<Q, N, D>
where
    N: NetStackHandle<Profile = EmbassyUsbManager<Q>>,
    Q: BbqHandle + 'static,
    D: Driver<'static>,
{
    /// Create a new receiver object
    pub fn new(stack: N, rx: D::EndpointOut) -> Self {
        Self {
            nsh: stack,
            rx,
            net_id: None,
        }
    }

    /// Runs forever, processing incoming frames.
    ///
    /// The provided slice is used for receiving a frame via USB. It is used as the MTU
    /// for the entire connections.
    ///
    /// `max_usb_frame_size` is the largest size of USB frame we can receive. For example,
    /// it would be 64. This is NOT the largest message we can receive. It MUST be a power
    /// of two.
    pub async fn run(mut self, frame: &mut [u8], max_usb_frame_size: usize) -> ! {
        assert!(max_usb_frame_size.is_power_of_two());
        loop {
            self.rx.wait_enabled().await;
            info!("Connection established");

            // Mark the interface as established
            _ = self
                .nsh
                .stack()
                .manage_profile(|im| im.set_interface_state((), InterfaceState::Inactive));

            // Handle all frames for the connection
            self.one_conn(frame, max_usb_frame_size).await;

            // Mark the connection as lost
            info!("Connection lost");
            self.nsh.stack().manage_profile(|im| {
                _ = im.set_interface_state((), InterfaceState::Down);
            });
        }
    }

    /// Handle all frames, returning when a connection error occurs
    async fn one_conn(&mut self, frame: &mut [u8], max_usb_frame_size: usize) {
        loop {
            match self.one_frame(frame, max_usb_frame_size).await {
                Ok(f) => {
                    // NOTE: this is BLOCKING, but does NOT wait for the request to
                    // be processed, we just copy the frame into its destination
                    // buffers.
                    //
                    // We COULD potentially gain some throughput by having another
                    // buffer here, so we can immediately begin receiving the next
                    // frame, at the cost of extra buffer space and copies.
                    process_frame(&mut self.net_id, f, &self.nsh, ());
                }
                Err(ReceiverError::ConnectionClosed) => break,
                Err(_e) => {
                    continue;
                }
            }
        }
    }

    /// Receive a single ergot frame, which might be across multiple reads of the endpoint
    ///
    /// No checking of the frame is done, only that the bulk endpoint gave us a frame.
    async fn one_frame<'a>(
        &mut self,
        frame: &'a mut [u8],
        max_frame_len: usize,
    ) -> Result<&'a mut [u8], ReceiverError> {
        let buflen = frame.len();
        let mut window = &mut frame[..];

        while !window.is_empty() {
            let n = match self.rx.read(window).await {
                Ok(n) => n,
                Err(EndpointError::BufferOverflow) => {
                    return Err(ReceiverError::ReceivedMessageTooLarge);
                }
                Err(EndpointError::Disabled) => return Err(ReceiverError::ConnectionClosed),
            };

            let (_now, later) = window.split_at_mut(n);
            window = later;
            if n != max_frame_len {
                // We now have a full frame! Great!
                let wlen = window.len();
                let len = buflen - wlen;
                let frame = &mut frame[..len];

                return Ok(frame);
            }
        }

        // If we got here, we've run out of space. That's disappointing. Accumulate to the
        // end of this packet
        loop {
            match self.rx.read(frame).await {
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

impl<Q, N, D> Drop for RxWorker<Q, N, D>
where
    N: NetStackHandle<Profile = EmbassyUsbManager<Q>>,
    Q: BbqHandle + 'static,
    D: Driver<'static>,
{
    fn drop(&mut self) {
        // No receiver? Drop the interface.
        self.nsh.stack().manage_profile(|im| {
            _ = im.set_interface_state((), InterfaceState::Down);
        })
    }
}
