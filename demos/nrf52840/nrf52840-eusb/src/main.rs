//! ergot null-interface demo

#![no_std]
#![no_main]

use core::pin::pin;

use defmt::{info, warn};
use embassy_executor::{task, Spawner};
use embassy_nrf::{
    bind_interrupts,
    config::{Config as NrfConfig, HfclkSource},
    gpio::{Input, Level, Output, OutputDrive, Pull},
    pac::FICR,
    peripherals::USBD,
    usb::{self, vbus_detect::HardwareVbusDetect},
};
use embassy_time::{Duration, Timer, WithTimeout};
use embassy_usb::{driver::Driver, Config, UsbDevice};
use ergot::{
    endpoint,
    exports::bbq2::{prod_cons::framed::FramedConsumer, traits::coordination::cas::AtomicCoord},
    toolkits::embassy_usb_v0_5 as kit,
    topic, Address,
};
use mutex::raw_impls::single_core_thread_mode::ThreadModeRawMutex;
use static_cell::{ConstStaticCell, StaticCell};

use {defmt_rtt as _, panic_probe as _};

const OUT_QUEUE_SIZE: usize = 4096;
const MAX_PACKET_SIZE: usize = 1024;

// Our nrf52840-specific USB driver
type AppDriver = usb::Driver<'static, HardwareVbusDetect>;
// The type of our RX Worker
type RxWorker = kit::RxWorker<&'static Queue, ThreadModeRawMutex, AppDriver>;
// The type of our netstack
type Stack = kit::Stack<&'static Queue, ThreadModeRawMutex>;
// The type of our outgoing queue
type Queue = kit::Queue<OUT_QUEUE_SIZE, AtomicCoord>;

/// Statically store our netstack
static STACK: Stack = kit::new_target_stack(OUTQ.framed_producer(), MAX_PACKET_SIZE as u16);
/// Statically store our USB app buffers
static STORAGE: kit::WireStorage<256, 256, 64, 256> = kit::WireStorage::new();
/// Statically store our outgoing packet buffer
static OUTQ: Queue = kit::Queue::new();

// Define some endpoints
endpoint!(LedEndpoint, bool, (), "led/set");
topic!(ButtonPressedTopic, u8, "button/press");

bind_interrupts!(pub struct Irqs {
    USBD => usb::InterruptHandler<USBD>;
    CLOCK_POWER => usb::vbus_detect::InterruptHandler;
});

fn usb_config(serial: &'static str) -> Config<'static> {
    let mut config = Config::new(0x16c0, 0x27DD);
    config.manufacturer = Some("OneVariable");
    config.product = Some("ergot-nrf");
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
    let mut config = NrfConfig::default();
    config.hfclk_source = HfclkSource::ExternalXtal;
    let p = embassy_nrf::init(Default::default());
    // Obtain the device ID
    let unique_id = get_unique_id();

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
    let driver = usb::Driver::new(p.USBD, Irqs, HardwareVbusDetect::new(Irqs));
    let config = usb_config(ser_buf);
    let (device, tx_impl, ep_out) = STORAGE.init_ergot(driver, config);

    static RX_BUF: ConstStaticCell<[u8; MAX_PACKET_SIZE]> =
        ConstStaticCell::new([0u8; MAX_PACKET_SIZE]);
    let rxvr: RxWorker = kit::RxWorker::new(&STACK, ep_out);
    spawner.must_spawn(usb_task(device));
    spawner.must_spawn(run_tx(tx_impl, OUTQ.framed_consumer()));
    spawner.must_spawn(run_rx(rxvr, RX_BUF.take()));
    spawner.must_spawn(pingserver());
    spawner.must_spawn(yeeter());

    // Start the led servers first
    let led_pins = [
        Output::new(p.P0_13, Level::High, OutputDrive::Standard),
        Output::new(p.P0_14, Level::High, OutputDrive::Standard),
        Output::new(p.P0_15, Level::High, OutputDrive::Standard),
        Output::new(p.P0_16, Level::High, OutputDrive::Standard),
    ];
    let btn_pins = [
        Input::new(p.P0_11, Pull::Up),
        Input::new(p.P0_12, Pull::Up),
        Input::new(p.P0_24, Pull::Up),
        Input::new(p.P0_25, Pull::Up),
    ];
    let names = ["LED1", "LED2", "LED3", "LED4"];

    for (name, led) in names.iter().zip(led_pins.into_iter()) {
        spawner.must_spawn(led_server(name, led));
    }

    for (name, btn) in names.iter().zip(btn_pins.into_iter()) {
        spawner.must_spawn(button_worker(btn, name));
    }

    // Then start two tasks that just both listen to every button press event
    spawner.must_spawn(press_listener(1));
    spawner.must_spawn(press_listener(2));
}

topic!(YeetTopic, u64, "topic/yeet");

#[task]
async fn pingserver() {
    STACK.services().ping_handler::<4>().await;
}

#[task]
async fn yeeter() {
    let mut ctr = 0;
    Timer::after(Duration::from_secs(3)).await;
    loop {
        Timer::after(Duration::from_secs(5)).await;
        warn!("Sending broadcast message");
        let _ = STACK.topics().broadcast::<YeetTopic>(&ctr, None);
        ctr += 1;
    }
}

/// This handles the low level USB management
#[embassy_executor::task]
pub async fn usb_task(mut usb: UsbDevice<'static, AppDriver>) {
    usb.run().await;
}

#[task]
async fn run_rx(rcvr: RxWorker, recv_buf: &'static mut [u8]) {
    rcvr.run(recv_buf, kit::USB_FS_MAX_PACKET_SIZE).await;
}

#[task]
async fn run_tx(
    mut ep_in: <AppDriver as Driver<'static>>::EndpointIn,
    rx: FramedConsumer<&'static Queue>,
) {
    kit::tx_worker::<AppDriver, OUT_QUEUE_SIZE, AtomicCoord>(
        &mut ep_in,
        rx,
        kit::DEFAULT_TIMEOUT_MS_PER_FRAME,
        kit::USB_FS_MAX_PACKET_SIZE,
    )
    .await;
}

#[task(pool_size = 2)]
async fn press_listener(idx: u8) {
    let recv = STACK
        .topics()
        .bounded_receiver::<ButtonPressedTopic, 4>(None);
    let recv = pin!(recv);
    let mut recv = recv.subscribe();

    loop {
        let msg = recv.recv().await;
        defmt::info!("Listener #{=u8}, button {=u8} pressed", idx, msg.t);
    }
}

#[task(pool_size = 4)]
async fn led_server(name: &'static str, mut led: Output<'static>) {
    let socket = STACK
        .endpoints()
        .bounded_server::<LedEndpoint, 4>(Some(name));
    let socket = pin!(socket);
    let mut hdl = socket.attach();

    loop {
        let _ = hdl
            .serve(async |on| {
                defmt::info!("{=str} set {=bool}", name, *on);
                if *on {
                    led.set_low();
                } else {
                    led.set_high();
                }
            })
            .await;
    }
}

#[task(pool_size = 4)]
async fn button_worker(mut btn: Input<'static>, name: &'static str) {
    let client = STACK
        .endpoints()
        .client::<LedEndpoint>(Address::unknown(), Some(name));
    loop {
        btn.wait_for_low().await;
        let res = btn
            .wait_for_high()
            .with_timeout(Duration::from_millis(5))
            .await;
        if res.is_ok() {
            continue;
        }
        client.request(&true).await.unwrap();
        let _ = STACK.topics().broadcast::<ButtonPressedTopic>(&1, None);
        btn.wait_for_high().await;
        client.request(&false).await.unwrap();
    }
}

fn get_unique_id() -> u64 {
    let lower = FICR.deviceid(0).read() as u64;
    let upper = FICR.deviceid(1).read() as u64;
    (upper << 32) | lower
}
