use core::sync::atomic::{AtomicU8, Ordering};

use embassy_nrf::{
    peripherals::USBD,
    usb::{self, vbus_detect::HardwareVbusDetect},
};
use embassy_sync::{
    blocking_mutex::raw::RawMutex, blocking_mutex::raw::ThreadModeRawMutex, mutex::Mutex,
};
use embassy_usb::{
    driver::Driver,
    msos::{self, windows_version},
    Builder, UsbDevice,
};
use static_cell::{ConstStaticCell, StaticCell};

pub type AppDriver = usb::Driver<'static, USBD, HardwareVbusDetect>;
pub type AppStorage = WireStorage<ThreadModeRawMutex, AppDriver, 256, 256, 64, 256>;
pub type BufStorage = PacketBuffers<1024, 1024>;

/// Static storage for generically sized input and output packet buffers
pub struct PacketBuffers<const TX: usize = 1024, const RX: usize = 1024> {
    /// the transmit buffer
    pub tx_buf: [u8; TX],
    /// thereceive buffer
    pub rx_buf: [u8; RX],
}

impl<const TX: usize, const RX: usize> PacketBuffers<TX, RX> {
    /// Create new empty buffers
    pub const fn new() -> Self {
        Self {
            tx_buf: [0u8; TX],
            rx_buf: [0u8; RX],
        }
    }
}

/// Statically store our packet buffers
pub static PBUFS: ConstStaticCell<BufStorage> = ConstStaticCell::new(BufStorage::new());

struct ErgotHandler {}

static STINDX: AtomicU8 = AtomicU8::new(0xFF);
static HDLR: ConstStaticCell<ErgotHandler> = ConstStaticCell::new(ErgotHandler {});
pub const DEVICE_INTERFACE_GUIDS: &[&str] = &["{AFB9A6FB-30BA-44BC-9232-806CFC875321}"];

/// Default time in milliseconds to wait for the completion of sending
pub const DEFAULT_TIMEOUT_MS_PER_FRAME: usize = 2;

impl embassy_usb::Handler for ErgotHandler {
    fn get_string(&mut self, index: embassy_usb::types::StringIndex, lang_id: u16) -> Option<&str> {
        use embassy_usb::descriptor::lang_id;

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

pub struct EUsbWireTxInner<D: Driver<'static>> {
    pub ep_in: D::EndpointIn,
    pub log_seq: u16,
    pub tx_buf: &'static mut [u8],
    pub pending_frame: bool,
    pub timeout_ms_per_frame: usize,
}

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

/// A helper type for `static` storage of buffers and driver components
pub struct WireStorage<
    M: RawMutex + 'static,
    D: Driver<'static> + 'static,
    const CONFIG: usize = 256,
    const BOS: usize = 256,
    const CONTROL: usize = 64,
    const MSOS: usize = 256,
> {
    /// Usb buffer storage
    pub bufs_usb: ConstStaticCell<UsbDeviceBuffers<CONFIG, BOS, CONTROL, MSOS>>,
    /// WireTx/Sender static storage
    pub cell: StaticCell<Mutex<M, EUsbWireTxInner<D>>>,
}

impl<
        M: RawMutex + 'static,
        D: Driver<'static> + 'static,
        const CONFIG: usize,
        const BOS: usize,
        const CONTROL: usize,
        const MSOS: usize,
    > WireStorage<M, D, CONFIG, BOS, CONTROL, MSOS>
{
    /// Create a new, uninitialized static set of buffers
    pub const fn new() -> Self {
        Self {
            bufs_usb: ConstStaticCell::new(UsbDeviceBuffers::new()),
            cell: StaticCell::new(),
        }
    }

    /// Initialize the static storage, reporting as ergot compatible
    ///
    /// This must only be called once.
    pub fn init_ergot(
        &'static self,
        driver: D,
        config: embassy_usb::Config<'static>,
        tx_buf: &'static mut [u8],
    ) -> (UsbDevice<'static, D>, EUsbWireTx<M, D>, EUsbWireRx<D>) {
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
        STINDX.store(stindx.0, core::sync::atomic::Ordering::Relaxed);
        let mut alt = interface.alt_setting(0xFF, 0xCA, 0x7D, Some(stindx));
        let ep_out = alt.endpoint_bulk_out(64);
        let ep_in = alt.endpoint_bulk_in(64);
        drop(function);

        let wtx = self.cell.init(Mutex::new(EUsbWireTxInner {
            ep_in,
            log_seq: 0,
            tx_buf,
            pending_frame: false,
            timeout_ms_per_frame: DEFAULT_TIMEOUT_MS_PER_FRAME,
        }));

        // Build the builder.
        let usb = builder.build();

        (usb, EUsbWireTx { inner: wtx }, EUsbWireRx { ep_out })
    }

    /// Initialize the static storage.
    ///
    /// This must only be called once.
    pub fn init(
        &'static self,
        driver: D,
        config: embassy_usb::Config<'static>,
        tx_buf: &'static mut [u8],
    ) -> (UsbDevice<'static, D>, EUsbWireTx<M, D>, EUsbWireRx<D>) {
        let (builder, wtx, wrx) = self.init_without_build(driver, config, tx_buf);
        let usb = builder.build();
        (usb, wtx, wrx)
    }
    /// Initialize the static storage, without building `Builder`
    ///
    /// This must only be called once.
    pub fn init_without_build(
        &'static self,
        driver: D,
        config: embassy_usb::Config<'static>,
        tx_buf: &'static mut [u8],
    ) -> (Builder<'static, D>, EUsbWireTx<M, D>, EUsbWireRx<D>) {
        let bufs = self.bufs_usb.take();

        let mut builder = Builder::new(
            driver,
            config,
            &mut bufs.config_descriptor,
            &mut bufs.bos_descriptor,
            &mut bufs.msos_descriptor,
            &mut bufs.control_buf,
        );

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
        let mut alt = interface.alt_setting(0xFF, 0, 0, None);
        let ep_out = alt.endpoint_bulk_out(64);
        let ep_in = alt.endpoint_bulk_in(64);
        drop(function);

        let wtx = self.cell.init(Mutex::new(EUsbWireTxInner {
            ep_in,
            log_seq: 0,
            tx_buf,
            pending_frame: false,
            timeout_ms_per_frame: DEFAULT_TIMEOUT_MS_PER_FRAME,
        }));

        (builder, EUsbWireTx { inner: wtx }, EUsbWireRx { ep_out })
    }
}

/// A [`WireTx`] implementation for embassy-usb 0.4.
#[derive(Copy)]
pub struct EUsbWireTx<M: RawMutex + 'static, D: Driver<'static> + 'static> {
    inner: &'static Mutex<M, EUsbWireTxInner<D>>,
}

impl<M: RawMutex + 'static, D: Driver<'static> + 'static> Clone for EUsbWireTx<M, D> {
    fn clone(&self) -> Self {
        EUsbWireTx { inner: self.inner }
    }
}

pub struct EUsbWireRx<D: Driver<'static>> {
    pub ep_out: D::EndpointOut,
}
