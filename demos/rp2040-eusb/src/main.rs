//! ergot null-interface demo

#![no_std]
#![no_main]

use core::pin::pin;

use bbq2::{
    prod_cons::framed::FramedConsumer,
    queue::BBQueue,
    traits::{coordination::cs::CsCoord, notifier::maitake::MaiNotSpsc, storage::Inline},
};
use defmt::{info, warn};
use embassy_executor::{task, Spawner};
use embassy_rp::usb;
use embassy_rp::{
    bind_interrupts,
    gpio::{Level, Output},
    peripherals::USB,
};
use embassy_time::{Duration, Ticker, Timer};
use embassy_usb::{driver::Driver, Config, UsbDevice};
use ergot::{
    endpoint,
    interface_manager::eusb_0_4_client::{
        self, EmbassyUsbManager, WireStorage, DEFAULT_TIMEOUT_MS_PER_FRAME, USB_FS_MAX_PACKET_SIZE,
    },
    topic,
    well_known::ErgotPingEndpoint,
    Address, NetStack,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use static_cell::{ConstStaticCell, StaticCell};

use {defmt_rtt as _, panic_probe as _};

const OUT_QUEUE_SIZE: usize = 4096;
const MAX_PACKET_SIZE: usize = 1024;

// Our nrf52840-specific USB driver
pub type AppDriver = usb::Driver<'static, USB>;
// Our USB buffer sizes that we can store as a static
pub type AppStorage = WireStorage<256, 256, 64, 256>;
// The type of our output message queue, sent from the net stack to the tx worker
pub type OutQueue = BBQueue<Inline<OUT_QUEUE_SIZE>, CsCoord, MaiNotSpsc>;
// The type of our Interface Manager using Embassy USB
pub type EUsbInterfaceManager = EmbassyUsbManager<OUT_QUEUE_SIZE, CsCoord>;
// The type of our receiver that processes incoming data and feeds it to the net stack
pub type InterfaceReceiver =
    eusb_0_4_client::Receiver<CriticalSectionRawMutex, AppDriver, OUT_QUEUE_SIZE, CsCoord>;

/// Statically store our netstack
pub static STACK: NetStack<CriticalSectionRawMutex, EUsbInterfaceManager> = NetStack::new();
/// Statically store our USB app buffers
pub static STORAGE: AppStorage = AppStorage::new();
/// Statically store our outgoing packet buffer
static OUTQ: OutQueue = OutQueue::new();

// Define some endpoints
endpoint!(LedEndpoint, bool, (), "led/set");

bind_interrupts!(pub struct Irqs {
    USBCTRL_IRQ => usb::InterruptHandler<USB>;
});

fn usb_config(serial: &'static str) -> Config<'static> {
    let mut config = Config::new(0x16c0, 0x27DD);
    config.manufacturer = Some("OneVariable");
    config.product = Some("ergot-pico");
    config.serial_number = Some(serial);

    // Required for windows compatibility.
    // https://developer.nordicsemi.com/nRF_Connect_SDK/doc/1.9.1/kconfig/CONFIG_CDC_ACM_IAD.html#help
    config.device_class = 0xEF;
    config.device_sub_class = 0x02;
    config.device_protocol = 0x01;
    config.composite_with_iads = true;

    config
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // SYSTEM INIT
    info!("Start");
    let mut p = embassy_rp::init(Default::default());
    // Obtain the flash ID
    let unique_id = unique_id::get_unique_id(p.FLASH.reborrow()).unwrap();
    static SERIAL_STRING: StaticCell<[u8; 16]> = StaticCell::new();
    let mut ser_buf = [b' '; 16];
    // This is a simple number-to-hex formatting
    unique_id
        .to_be_bytes()
        .iter()
        .zip(ser_buf.chunks_exact_mut(2))
        .for_each(|(b, chs)| {
            let mut b = *b;
            for c in chs {
                *c = match b >> 4 {
                    v @ 0..10 => b'0' + v,
                    v @ 10..16 => b'A' + (v - 10),
                    _ => b'X',
                };
                b <<= 4;
            }
        });
    let ser_buf = SERIAL_STRING.init(ser_buf);
    let ser_buf = core::str::from_utf8(ser_buf.as_slice()).unwrap();

    // USB/RPC INIT
    let driver = usb::Driver::new(p.USB, Irqs);
    let config = usb_config(ser_buf);
    let (device, tx_impl, ep_out) = STORAGE.init_ergot(driver, config);

    static RX_BUF: ConstStaticCell<[u8; MAX_PACKET_SIZE]> =
        ConstStaticCell::new([0u8; MAX_PACKET_SIZE]);
    let rxvr = InterfaceReceiver::new(&OUTQ, STACK.base(), ep_out);
    spawner.must_spawn(usb_task(device));
    spawner.must_spawn(run_tx(tx_impl, OUTQ.framed_consumer()));
    spawner.must_spawn(run_rx(rxvr, RX_BUF.take()));
    spawner.must_spawn(pingserver());
    spawner.must_spawn(yeeter());
    spawner.must_spawn(led_server(Output::new(p.PIN_25, Level::High)));

    let mut ticker = Ticker::every(Duration::from_millis(500));
    loop {
        ticker.next().await;
        let _ = STACK
            .req_resp::<LedEndpoint>(Address::unknown(), &true, Some("led"))
            .await;
        ticker.next().await;
        let _ = STACK
            .req_resp::<LedEndpoint>(Address::unknown(), &false, Some("led"))
            .await;
    }
}

topic!(YeetTopic, u64, "topic/yeet");

#[task]
async fn pingserver() {
    let server = STACK.stack_bounded_endpoint_server::<ErgotPingEndpoint, 4>(None);
    let server = pin!(server);
    let mut server_hdl = server.attach();
    loop {
        server_hdl
            .serve_blocking(|req: &u32| {
                info!("Serving ping {=u32}", req);
                *req
            })
            .await
            .unwrap();
    }
}

#[task]
async fn yeeter() {
    let mut ctr = 0;
    Timer::after(Duration::from_secs(3)).await;
    loop {
        Timer::after(Duration::from_secs(5)).await;
        warn!("Sending broadcast message");
        let _ = STACK.broadcast_topic::<YeetTopic>(&ctr, None).await;
        ctr += 1;
    }
}

/// This handles the low level USB management
#[embassy_executor::task]
pub async fn usb_task(mut usb: UsbDevice<'static, AppDriver>) {
    usb.run().await;
}

#[task]
async fn run_rx(rcvr: InterfaceReceiver, recv_buf: &'static mut [u8]) {
    rcvr.run(recv_buf, USB_FS_MAX_PACKET_SIZE).await;
}

#[task]
async fn run_tx(
    mut ep_in: <AppDriver as Driver<'static>>::EndpointIn,
    rx: FramedConsumer<&'static OutQueue>,
) {
    eusb_0_4_client::tx_worker::<AppDriver, OUT_QUEUE_SIZE, CsCoord>(
        &mut ep_in,
        rx,
        DEFAULT_TIMEOUT_MS_PER_FRAME,
        USB_FS_MAX_PACKET_SIZE,
    )
    .await;
}

#[task]
async fn led_server(mut led: Output<'static>) {
    let socket = STACK.stack_bounded_endpoint_server::<LedEndpoint, 2>(Some("led"));
    let socket = pin!(socket);
    let mut hdl = socket.attach();

    loop {
        let _ = hdl
            .serve(async |on| {
                defmt::info!("LED set {=bool}", *on);
                if *on {
                    led.set_low();
                } else {
                    led.set_high();
                }
            })
            .await;
    }
}

/// Helper to get unique ID from flash
mod unique_id {
    use embassy_rp::{
        flash::{Blocking, Flash},
        peripherals::FLASH,
        Peri,
    };

    /// This function retrieves the unique ID of the external flash memory.
    ///
    /// The RP2040 has no internal unique ID register, but most flash chips do,
    /// So we use that instead.
    pub fn get_unique_id(flash: Peri<'_, FLASH>) -> Option<u64> {
        let mut flash: Flash<'_, FLASH, Blocking, { 16 * 1024 * 1024 }> =
            Flash::new_blocking(flash);
        let mut id = [0u8; core::mem::size_of::<u64>()];
        flash.blocking_unique_id(&mut id).ok()?;
        Some(u64::from_be_bytes(id))
    }
}
