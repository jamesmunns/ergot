//! Embassy USB Bulk packet interface
//!
//! This implementation uses USB Bulk endpoints to send data in a framed manner. This uses
//! the convention of non-max sized frames signalling the end of a frame, and using zero length
//! packets (ZLP) to signal the end of a frame when a message happens to be the max size of of the interface.
//!
//! For example on USB FS, where the max packet size is 64, sending a 32 byte message would take up
//! one frame, and we would know it is the "end" of the frame because 32 != 64. Similarly, we could
//! send a 96 byte message as two packets, 64 + 32 bytes, and we would know the second packet ends the
//! frame. Finally, we could send a 128 byte frame as three packets: 64 + 64 + 0, using a ZLP to mark
//! the end of the frame.

use core::{marker::PhantomData, sync::atomic::AtomicU8};

use bbq2::{
    queue::BBQueue,
    traits::{bbqhdl::BbqHandle, notifier::maitake::MaiNotSpsc, storage::Inline},
};
use static_cell::ConstStaticCell;

use crate::interface_manager::{Interface, utils::framed_stream};

/// An Embassy-USB interface implementation
pub struct EmbassyInterface<Q: BbqHandle + 'static> {
    _pd: PhantomData<Q>,
}

/// A small type for handling USB device name enumeration
struct ErgotHandler {}

/// A type alias for the outgoing packet queue typically used by the [`EmbassyInterface`]
pub type Queue<const N: usize, C> = BBQueue<Inline<N>, C, MaiNotSpsc>;
/// A type alias for the InterfaceSink typically used by the [`EmbassyInterface`]
pub type EmbassySink<Q> = framed_stream::Sink<Q>;

/// Interface Implementation
impl<Q: BbqHandle + 'static> Interface for EmbassyInterface<Q> {
    type Sink = EmbassySink<Q>;
}

// Helper bits

/// Errors observable by the sender
enum TransmitError {
    ConnectionClosed,
    Timeout,
}

// ---- Constants ----

/// Random device GUIDs generated for demos
pub const DEVICE_INTERFACE_GUIDS: &[&str] = &["{AFB9A6FB-30BA-44BC-9232-806CFC875321}"];
/// Default time in milliseconds to wait for the completion of sending
pub const DEFAULT_TIMEOUT_MS_PER_FRAME: usize = 2;
/// Default max packet size for USB Full Speed
pub const USB_FS_MAX_PACKET_SIZE: usize = 64;

// ---- Statics ----

static STINDX: AtomicU8 = AtomicU8::new(0xFF);
static HDLR: ConstStaticCell<ErgotHandler> = ConstStaticCell::new(ErgotHandler {});

// ---- Types ----

pub struct UsbDeviceBuffers<
    const CONFIG: usize = 256,
    const BOS: usize = 256,
    const CONTROL: usize = 64,
    const MSOS: usize = 256,
> {
    /// Config descriptor storage
    pub config_descriptor: [u8; CONFIG],
    /// BOS descriptor storage
    pub bos_descriptor: [u8; BOS],
    /// CONTROL endpoint buffer storage
    pub control_buf: [u8; CONTROL],
    /// MSOS descriptor buffer storage
    pub msos_descriptor: [u8; MSOS],
}

// ---- impls ----

// impl UsbDeviceBuffers

impl<const CONFIG: usize, const BOS: usize, const CONTROL: usize, const MSOS: usize>
    UsbDeviceBuffers<CONFIG, BOS, CONTROL, MSOS>
{
    /// Create a new, empty set of buffers
    pub const fn new() -> Self {
        Self {
            config_descriptor: [0u8; CONFIG],
            bos_descriptor: [0u8; BOS],
            msos_descriptor: [0u8; MSOS],
            control_buf: [0u8; CONTROL],
        }
    }
}

impl<const CONFIG: usize, const BOS: usize, const CONTROL: usize, const MSOS: usize> Default
    for UsbDeviceBuffers<CONFIG, BOS, CONTROL, MSOS>
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "embassy-usb-v0_5")]
pub mod eusb_0_5 {
    use core::sync::atomic::Ordering;

    use crate::logging::{debug, info, warn};
    use bbq2::{
        prod_cons::framed::FramedConsumer,
        queue::BBQueue,
        traits::{coordination::Coord, notifier::maitake::MaiNotSpsc, storage::Inline},
    };
    use embassy_futures::select::{Either, select};
    use embassy_time::Timer;
    use embassy_usb_0_5::{
        Builder, UsbDevice,
        driver::{Driver, Endpoint, EndpointIn},
        msos::{self, windows_version},
    };
    use static_cell::ConstStaticCell;

    use crate::interface_manager::interface_impls::embassy_usb::TransmitError;

    use super::{DEVICE_INTERFACE_GUIDS, ErgotHandler, HDLR, STINDX, UsbDeviceBuffers};

    /// A helper type for `static` storage of buffers and driver components
    pub struct WireStorage<
        const CONFIG: usize = 256,
        const BOS: usize = 256,
        const CONTROL: usize = 64,
        const MSOS: usize = 256,
    > {
        /// Usb buffer storage
        pub bufs_usb: ConstStaticCell<UsbDeviceBuffers<CONFIG, BOS, CONTROL, MSOS>>,
    }

    /// Transmitter worker task
    ///
    /// Takes a bbqueue from the NetStack of packets to send. While sending,
    /// we will timeout if
    pub async fn tx_worker<'a, D: Driver<'a>, const N: usize, C: Coord>(
        ep_in: &mut D::EndpointIn,
        rx: FramedConsumer<&'static BBQueue<Inline<N>, C, MaiNotSpsc>>,
        timeout_ms_per_frame: usize,
        max_usb_frame_size: usize,
    ) {
        assert!(max_usb_frame_size.is_power_of_two());
        info!("Started tx_worker");
        let mut pending = false;
        loop {
            // Wait for the endpoint to be connected...
            ep_in.wait_enabled().await;

            info!("Endpoint marked connected");

            'connection: loop {
                // Wait for an outgoing frame
                let frame = rx.wait_read().await;

                debug!("Got frame to send len {}", frame.len());

                // Attempt to send it
                let res = send_all::<D>(
                    ep_in,
                    &frame,
                    &mut pending,
                    timeout_ms_per_frame,
                    max_usb_frame_size,
                )
                .await;

                // Done with this frame, free the buffer space
                frame.release();

                match res {
                    Ok(()) => {}
                    Err(TransmitError::Timeout) => {
                        // TODO: treat enough timeouts like a connection closed?
                        warn!("Transmit timeout!");
                    }
                    Err(TransmitError::ConnectionClosed) => break 'connection,
                }
            }

            // Drain any pending frames - the connection was lost.
            while let Ok(frame) = rx.read() {
                frame.release();
            }
        }
    }

    #[inline]
    async fn send_all<'a, D>(
        ep_in: &mut D::EndpointIn,
        out: &[u8],
        pending_frame: &mut bool,
        timeout_ms_per_frame: usize,
        max_usb_frame_size: usize,
    ) -> Result<(), TransmitError>
    where
        D: Driver<'a>,
    {
        if out.is_empty() {
            return Ok(());
        }

        // Calculate an estimated timeout based on the number of frames we need to send
        // For now, we use 2ms/frame by default, rounded UP
        let frames = out.len().div_ceil(max_usb_frame_size);
        let timeout_ms = frames * timeout_ms_per_frame;

        let send_fut = async {
            // If we left off a pending frame, send one now so we don't leave an unterminated
            // message
            if *pending_frame && ep_in.write(&[]).await.is_err() {
                return Err(TransmitError::ConnectionClosed);
            }
            *pending_frame = true;

            // write in segments of max_usb_frame_size. The last chunk may
            // be 0 < len <= max_usb_frame_size.
            for ch in out.chunks(max_usb_frame_size) {
                if ep_in.write(ch).await.is_err() {
                    return Err(TransmitError::ConnectionClosed);
                }
            }
            // If the total we sent was a multiple of max_usb_frame_size, send an
            // empty message to "flush" the transaction. We already checked
            // above that the len != 0.
            if (out.len() & (max_usb_frame_size - 1)) == 0 && ep_in.write(&[]).await.is_err() {
                return Err(TransmitError::ConnectionClosed);
            }

            *pending_frame = false;
            Ok(())
        };

        match select(send_fut, Timer::after_millis(timeout_ms as u64)).await {
            Either::First(res) => res,
            Either::Second(()) => Err(TransmitError::Timeout),
        }
    }

    // impl WireStorage

    impl<const CONFIG: usize, const BOS: usize, const CONTROL: usize, const MSOS: usize>
        WireStorage<CONFIG, BOS, CONTROL, MSOS>
    {
        /// Create a new, uninitialized static set of buffers
        pub const fn new() -> Self {
            Self {
                bufs_usb: ConstStaticCell::new(UsbDeviceBuffers::new()),
            }
        }

        /// Initialize the static storage, reporting as ergot compatible
        ///
        /// This must only be called once.
        pub fn init_ergot<D: Driver<'static> + 'static>(
            &'static self,
            driver: D,
            config: embassy_usb_0_5::Config<'static>,
        ) -> (UsbDevice<'static, D>, D::EndpointIn, D::EndpointOut) {
            let bufs = self.bufs_usb.take();

            let mut builder = Builder::new(
                driver,
                config,
                &mut bufs.config_descriptor,
                &mut bufs.bos_descriptor,
                &mut bufs.msos_descriptor,
                &mut bufs.control_buf,
            );

            // Register a ergot-compatible string handler
            let hdlr = HDLR.take();
            builder.handler(hdlr);

            // Add the Microsoft OS Descriptor (MSOS/MOD) descriptor.
            // We tell Windows that this entire device is compatible with the "WINUSB" feature,
            // which causes it to use the built-in WinUSB driver automatically, which in turn
            // can be used by libusb/rusb software without needing a custom driver or INF file.
            // In principle you might want to call msos_feature() just on a specific function,
            // if your device also has other functions that still use standard class drivers.
            builder.msos_descriptor(windows_version::WIN8_1, 0);
            builder.msos_feature(msos::CompatibleIdFeatureDescriptor::new("WINUSB", ""));
            builder.msos_feature(msos::RegistryPropertyFeatureDescriptor::new(
                "DeviceInterfaceGUIDs",
                msos::PropertyData::RegMultiSz(DEVICE_INTERFACE_GUIDS),
            ));

            // Add a vendor-specific function (class 0xFF), and corresponding interface,
            // that uses our custom handler.
            let mut function = builder.function(0xFF, 0, 0);
            let mut interface = function.interface();
            let stindx = interface.string();
            STINDX.store(stindx.0, Ordering::Relaxed);
            let mut alt = interface.alt_setting(0xFF, 0xCA, 0x7D, Some(stindx));
            let ep_out = alt.endpoint_bulk_out(None, 64);
            let ep_in = alt.endpoint_bulk_in(None, 64);
            drop(function);

            // Build the builder.
            let usb = builder.build();

            (usb, ep_in, ep_out)
        }

        /// Initialize the static storage.
        ///
        /// This must only be called once.
        pub fn init<D: Driver<'static> + 'static>(
            &'static self,
            driver: D,
            config: embassy_usb_0_5::Config<'static>,
        ) -> (UsbDevice<'static, D>, D::EndpointIn, D::EndpointOut) {
            let (builder, wtx, wrx) = self.init_without_build(driver, config);
            let usb = builder.build();
            (usb, wtx, wrx)
        }
        /// Initialize the static storage, without building `Builder`
        ///
        /// This must only be called once.
        pub fn init_without_build<D: Driver<'static> + 'static>(
            &'static self,
            driver: D,
            config: embassy_usb_0_5::Config<'static>,
        ) -> (Builder<'static, D>, D::EndpointIn, D::EndpointOut) {
            let bufs = self.bufs_usb.take();

            let mut builder = Builder::new(
                driver,
                config,
                &mut bufs.config_descriptor,
                &mut bufs.bos_descriptor,
                &mut bufs.msos_descriptor,
                &mut bufs.control_buf,
            );

            // Register a ergot-compatible string handler
            let hdlr = HDLR.take();
            builder.handler(hdlr);

            // Add the Microsoft OS Descriptor (MSOS/MOD) descriptor.
            // We tell Windows that this entire device is compatible with the "WINUSB" feature,
            // which causes it to use the built-in WinUSB driver automatically, which in turn
            // can be used by libusb/rusb software without needing a custom driver or INF file.
            // In principle you might want to call msos_feature() just on a specific function,
            // if your device also has other functions that still use standard class drivers.
            builder.msos_descriptor(windows_version::WIN8_1, 0);
            builder.msos_feature(msos::CompatibleIdFeatureDescriptor::new("WINUSB", ""));
            builder.msos_feature(msos::RegistryPropertyFeatureDescriptor::new(
                "DeviceInterfaceGUIDs",
                msos::PropertyData::RegMultiSz(DEVICE_INTERFACE_GUIDS),
            ));

            // Add a vendor-specific function (class 0xFF), and corresponding interface,
            // that uses our custom handler.
            let mut function = builder.function(0xFF, 0, 0);
            let mut interface = function.interface();
            let stindx = interface.string();
            STINDX.store(stindx.0, Ordering::Relaxed);
            let mut alt = interface.alt_setting(0xFF, 0xCA, 0x7D, Some(stindx));
            let ep_out = alt.endpoint_bulk_out(None, 64);
            let ep_in = alt.endpoint_bulk_in(None, 64);
            drop(function);

            (builder, ep_in, ep_out)
        }
    }

    impl<const CONFIG: usize, const BOS: usize, const CONTROL: usize, const MSOS: usize> Default
        for WireStorage<CONFIG, BOS, CONTROL, MSOS>
    {
        fn default() -> Self {
            Self::new()
        }
    }

    // impl ErgotHandler

    impl embassy_usb_0_5::Handler for ErgotHandler {
        fn get_string(
            &mut self,
            index: embassy_usb_0_5::types::StringIndex,
            lang_id: u16,
        ) -> Option<&str> {
            use embassy_usb_0_5::descriptor::lang_id;

            let stindx = STINDX.load(Ordering::Relaxed);
            if stindx == 0xFF {
                return None;
            }
            if lang_id == lang_id::ENGLISH_US && index.0 == stindx {
                Some("ergot")
            } else {
                None
            }
        }
    }
}
