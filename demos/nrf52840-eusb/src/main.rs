//! ergot null-interface demo

#![no_std]
#![no_main]

use core::pin::pin;

use bbq2::{
    prod_cons::framed::FramedConsumer,
    queue::BBQueue,
    traits::{coordination::cas::AtomicCoord, notifier::maitake::MaiNotSpsc, storage::Inline},
};
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
    interface_manager::eusb_0_4_client::{
        self, EmbassyUsbManager, WireStorage, DEFAULT_TIMEOUT_MS_PER_FRAME, USB_FS_MAX_PACKET_SIZE,
    },
    socket::{endpoint::stack_vec::Server, topic::stack_vec::Receiver},
    well_known::ErgotPingEndpoint,
    Address, NetStack,
};
use mutex::raw_impls::single_core_thread_mode::ThreadModeRawMutex;
use postcard_rpc::{endpoint, topic};
use static_cell::{ConstStaticCell, StaticCell};

use {defmt_rtt as _, panic_probe as _};

const OUT_QUEUE_SIZE: usize = 4096;
const MAX_PACKET_SIZE: usize = 1024;

// Our nrf52840-specific USB driver
pub type AppDriver = usb::Driver<'static, USBD, HardwareVbusDetect>;
// Our USB buffer sizes that we can store as a static
pub type AppStorage = WireStorage<256, 256, 64, 256>;
// The type of our output message queue, sent from the net stack to the tx worker
pub type OutQueue = BBQueue<Inline<OUT_QUEUE_SIZE>, AtomicCoord, MaiNotSpsc>;
// The type of our Interface Manager using Embassy USB
pub type EUsbInterfaceManager = EmbassyUsbManager<OUT_QUEUE_SIZE, AtomicCoord>;
// The type of our receiver that processes incoming data and feeds it to the net stack
pub type InterfaceReceiver =
    eusb_0_4_client::Receiver<ThreadModeRawMutex, AppDriver, OUT_QUEUE_SIZE, AtomicCoord>;

/// Statically store our netstack
pub static STACK: NetStack<ThreadModeRawMutex, EUsbInterfaceManager> = NetStack::new();
/// Statically store our USB app buffers
pub static STORAGE: AppStorage = AppStorage::new();
/// Statically store our
static OUTQ: OutQueue = OutQueue::new();

// Define some endpoints
endpoint!(Led1Endpoint, bool, (), "leds/1/set");
endpoint!(Led2Endpoint, bool, (), "leds/2/set");
endpoint!(Led3Endpoint, bool, (), "leds/3/set");
endpoint!(Led4Endpoint, bool, (), "leds/4/set");
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
    let rxvr = InterfaceReceiver::new(&OUTQ, STACK.base(), ep_out);
    spawner.must_spawn(usb_task(device));
    spawner.must_spawn(run_tx(tx_impl, OUTQ.framed_consumer()));
    spawner.must_spawn(run_rx(rxvr, RX_BUF.take()));
    spawner.must_spawn(pingserver());
    spawner.must_spawn(yeeter());

    // Start the led servers first
    spawner.must_spawn(led_one(Output::new(
        p.P0_13,
        Level::High,
        OutputDrive::Standard,
    )));
    spawner.must_spawn(led_two(Output::new(
        p.P0_14,
        Level::High,
        OutputDrive::Standard,
    )));
    spawner.must_spawn(led_three(Output::new(
        p.P0_15,
        Level::High,
        OutputDrive::Standard,
    )));
    spawner.must_spawn(led_four(Output::new(
        p.P0_16,
        Level::High,
        OutputDrive::Standard,
    )));

    // Start the button listeners next
    spawner.must_spawn(button_one(Input::new(p.P0_11, Pull::Up)));
    spawner.must_spawn(button_two(Input::new(p.P0_12, Pull::Up)));
    spawner.must_spawn(button_three(Input::new(p.P0_24, Pull::Up)));
    spawner.must_spawn(button_four(Input::new(p.P0_25, Pull::Up)));

    // Then start two tasks that just both listen to every button press event
    spawner.must_spawn(press_listener(1));
    spawner.must_spawn(press_listener(2));
}

topic!(YeetTopic, u64, "topic/yeet");

#[task]
async fn pingserver() {
    let server = Server::<ErgotPingEndpoint, _, _, 4>::new(&STACK);
    let server = pin!(server);
    let mut server_hdl = server.attach();
    loop {
        server_hdl
            .serve(async |req| {
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
        STACK.broadcast_topic::<YeetTopic>(&ctr).await.unwrap();
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
    eusb_0_4_client::tx_worker::<AppDriver, OUT_QUEUE_SIZE, AtomicCoord>(
        &mut ep_in,
        rx,
        DEFAULT_TIMEOUT_MS_PER_FRAME,
        USB_FS_MAX_PACKET_SIZE,
    )
    .await;
}

#[task(pool_size = 2)]
async fn press_listener(idx: u8) {
    let recv: Receiver<ButtonPressedTopic, _, _, 4> = Receiver::new(&STACK);
    let recv = pin!(recv);
    let mut recv = recv.subscribe();

    loop {
        let msg = recv.recv().await;
        defmt::info!("Listener #{=u8}, button {=u8} pressed", idx, msg.t);
    }
}

#[task]
async fn led_one(mut led: Output<'static>) {
    let socket: Server<Led1Endpoint, _, _, 4> = Server::new(&STACK);
    let socket = pin!(socket);
    let mut hdl = socket.attach();

    loop {
        let _ = hdl
            .serve(async |on| {
                defmt::info!("LED1 set {=bool}", *on);
                if *on {
                    led.set_low();
                } else {
                    led.set_high();
                }
            })
            .await;
    }
}

#[task]
async fn button_one(mut btn: Input<'static>) {
    loop {
        btn.wait_for_low().await;
        let res = btn
            .wait_for_high()
            .with_timeout(Duration::from_millis(5))
            .await;
        if res.is_ok() {
            continue;
        }
        let _ = STACK
            .req_resp::<Led1Endpoint>(Address::unknown(), &true)
            .await;
        let _ = STACK.broadcast_topic::<ButtonPressedTopic>(&1).await;
        btn.wait_for_high().await;
        let _ = STACK
            .req_resp::<Led1Endpoint>(Address::unknown(), &false)
            .await;
    }
}

#[task]
async fn led_two(mut led: Output<'static>) {
    let socket: Server<Led2Endpoint, _, _, 4> = Server::new(&STACK);
    let socket = pin!(socket);
    let mut hdl = socket.attach();

    loop {
        let _ = hdl
            .serve(async |on| {
                defmt::info!("LED2 set {=bool}", *on);
                if *on {
                    led.set_low();
                } else {
                    led.set_high();
                }
            })
            .await;
    }
}

#[task]
async fn button_two(mut btn: Input<'static>) {
    loop {
        btn.wait_for_low().await;
        let res = btn
            .wait_for_high()
            .with_timeout(Duration::from_millis(5))
            .await;
        if res.is_ok() {
            continue;
        }
        let _ = STACK
            .req_resp::<Led2Endpoint>(Address::unknown(), &true)
            .await;
        let _ = STACK.broadcast_topic::<ButtonPressedTopic>(&2).await;
        btn.wait_for_high().await;
        let _ = STACK
            .req_resp::<Led2Endpoint>(Address::unknown(), &false)
            .await;
    }
}

#[task]
async fn led_three(mut led: Output<'static>) {
    let socket: Server<Led3Endpoint, _, _, 4> = Server::new(&STACK);
    let socket = pin!(socket);
    let mut hdl = socket.attach();

    loop {
        let _ = hdl
            .serve(async |on| {
                defmt::info!("LED3 set {=bool}", *on);
                if *on {
                    led.set_low();
                } else {
                    led.set_high();
                }
            })
            .await;
    }
}

#[task]
async fn button_three(mut btn: Input<'static>) {
    loop {
        btn.wait_for_low().await;
        let res = btn
            .wait_for_high()
            .with_timeout(Duration::from_millis(5))
            .await;
        if res.is_ok() {
            continue;
        }
        let _ = STACK
            .req_resp::<Led3Endpoint>(Address::unknown(), &true)
            .await;
        let _ = STACK.broadcast_topic::<ButtonPressedTopic>(&3).await;
        btn.wait_for_high().await;
        let _ = STACK
            .req_resp::<Led3Endpoint>(Address::unknown(), &false)
            .await;
    }
}

#[task]
async fn led_four(mut led: Output<'static>) {
    let socket: Server<Led4Endpoint, _, _, 4> = Server::new(&STACK);
    let socket = pin!(socket);
    let mut hdl = socket.attach();

    loop {
        let _ = hdl
            .serve(async |on| {
                defmt::info!("LED4 set {=bool}", *on);
                if *on {
                    led.set_low();
                } else {
                    led.set_high();
                }
            })
            .await;
    }
}

#[task]
async fn button_four(mut btn: Input<'static>) {
    loop {
        btn.wait_for_low().await;
        let res = btn
            .wait_for_high()
            .with_timeout(Duration::from_millis(5))
            .await;
        if res.is_ok() {
            continue;
        }
        let _ = STACK
            .req_resp::<Led4Endpoint>(Address::unknown(), &true)
            .await;
        let _ = STACK.broadcast_topic::<ButtonPressedTopic>(&4).await;
        btn.wait_for_high().await;
        let _ = STACK
            .req_resp::<Led4Endpoint>(Address::unknown(), &false)
            .await;
    }
}

fn get_unique_id() -> u64 {
    let lower = FICR.deviceid(0).read() as u64;
    let upper = FICR.deviceid(1).read() as u64;
    (upper << 32) | lower
}
