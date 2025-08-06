//! Embassy DHCP Example
//!
//!
//! Set SSID and PASSWORD env variable before running this example.
//!
//! This gets an ip address via DHCP then performs an HTTP get request to some
//! "random" server
//!
//! Because of the huge task-arena size configured this won't work on ESP32-S2

//% FEATURES: embassy esp-radio esp-radio/wifi esp-hal/unstable
//% CHIPS: esp32 esp32s2 esp32s3 esp32c2 esp32c3 esp32c6

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Ticker, Timer};
use esp_alloc as _;
use esp_hal::{
    Async,
    clock::CpuClock,
    timer::{systimer::SystemTimer, timg::TimerGroup},
    usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx, UsbSerialJtagTx},
};
use esp_radio::{
    Controller,
    wifi::{ClientConfiguration, Configuration, WifiController, WifiDevice, WifiEvent, WifiState},
};

use core::pin::pin;

use ergot::{
    exports::bbq2::traits::coordination::cas::AtomicCoord,
    fmt,
    toolkits::embedded_io_async_v0_6::{self as kit, tx_worker},
    well_known::ErgotPingEndpoint,
};

use mutex::raw_impls::cs::CriticalSectionRawMutex;
use static_cell::ConstStaticCell;

esp_bootloader_esp_idf::esp_app_desc!();

// When you are okay with using a nightly compiler it's better to use https://docs.rs/static_cell/2.1.0/static_cell/macro.make_static.html
macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

const OUT_QUEUE_SIZE: usize = 4096;
const MAX_PACKET_SIZE: usize = 1024;

// Our esp32c6-specific IO driver
type AppDriver = UsbSerialJtagRx<'static, Async>;
// The type of our RX Worker
type RxWorker = kit::RxWorker<&'static Queue, CriticalSectionRawMutex, AppDriver>;
// The type of our netstack
type Stack = kit::Stack<&'static Queue, CriticalSectionRawMutex>;
// The type of our outgoing queue
type Queue = kit::Queue<OUT_QUEUE_SIZE, AtomicCoord>;

/// Statically store our netstack
static STACK: Stack = kit::new_target_stack(OUTQ.stream_producer(), MAX_PACKET_SIZE as u16);
/// Statically store our outgoing packet buffer
static OUTQ: Queue = kit::Queue::new();
/// Statically store receive buffers
static RECV_BUF: ConstStaticCell<[u8; MAX_PACKET_SIZE]> =
    ConstStaticCell::new([0u8; MAX_PACKET_SIZE]);
static SCRATCH_BUF: ConstStaticCell<[u8; 64]> = ConstStaticCell::new([0u8; 64]);

const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("PASSWORD");

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) -> ! {

    // let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(size: 72 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_radio_preempt_baremetal::init(timg0.timer0);

    let esp_radio_ctrl = &*mk_static!(Controller<'static>, esp_radio::init().unwrap());

    let (controller, interfaces) = esp_radio::wifi::new(esp_radio_ctrl, peripherals.WIFI).unwrap();

    // This implements the Driver trait, we'll need to wrap this in a task that proxies
    let _wifi_interface: WifiDevice<'static> = interfaces.sta;

    // let timer0 = SystemTimer::new(p.SYSTIMER);
    // esp_hal_embassy::init(timer0.alarm0);
    let systimer = SystemTimer::new(peripherals.SYSTIMER);
    esp_hal_embassy::init(systimer.alarm0);

    // Create our USB-Serial interface, which implements the embedded-io-async traits
    let (rx, tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).into_async().split();
    let rx = RxWorker::new(STACK.base(), rx);

    // Spawn I/O worker tasks
    spawner.must_spawn(run_rx(rx, RECV_BUF.take(), SCRATCH_BUF.take()));
    spawner.must_spawn(run_tx(tx));

    // Spawn socket using tasks
    spawner.must_spawn(pingserver());
    spawner.must_spawn(logserver());

    // let rng = Rng::new();
    // let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    // Init network stack
    // let (stack, runner) = embassy_net::new(
    //     wifi_interface,
    //     config,
    //     mk_static!(StackResources<3>, StackResources::<3>::new()),
    //     seed,
    // );

    spawner.spawn(connection(controller)).ok();
    // spawner.spawn(net_task(runner)).ok();

    // let mut rx_buffer = [0; 4096];
    // let mut tx_buffer = [0; 4096];

    // loop {
    //     if stack.is_link_up() {
    //         break;
    //     }
    //     Timer::after(Duration::from_millis(500)).await;
    // }

    // println!("Waiting to get IP address...");
    // loop {
    //     if let Some(_config) = stack.config_v4() {
    //         // println!("Got IP: {}", config.address);
    //         break;
    //     }
    //     Timer::after(Duration::from_millis(500)).await;
    // }

    // loop {
    //     Timer::after(Duration::from_millis(1_000)).await;

    //     let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);

    //     socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));

    //     let remote_endpoint = (Ipv4Addr::new(142, 250, 185, 115), 80);
    //     // println!("connecting...");
    //     let r = socket.connect(remote_endpoint).await;
    //     if let Err(_e) = r {
    //         // println!("connect error: {:?}", e);
    //         continue;
    //     }
    //     // println!("connected!");
    //     let mut buf = [0; 1024];
    //     loop {
    //         use embedded_io_async::Write;
    //         let r = socket
    //             .write_all(b"GET / HTTP/1.0\r\nHost: www.mobile-j.de\r\n\r\n")
    //             .await;
    //         if let Err(_e) = r {
    //             // println!("write error: {:?}", e);
    //             break;
    //         }
    //         let _n = match socket.read(&mut buf).await {
    //             Ok(0) => {
    //                 // println!("read EOF");
    //                 break;
    //             }
    //             Ok(n) => n,
    //             Err(_e) => {
    //                 // println!("read error: {:?}", e);
    //                 break;
    //             }
    //         };
    //         // println!("{}", core::str::from_utf8(&buf[..n]).unwrap());
    //     }
    //     Timer::after(Duration::from_millis(3000)).await;
    // }
    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    Timer::after_secs(10).await;
    STACK.info_fmt(fmt!("start connection task"));
    STACK.info_fmt(fmt!("Device capabilities: {:?}", controller.capabilities()));
    loop {
        if esp_radio::wifi::wifi_state() == WifiState::StaConnected {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(5000)).await
        }
        if !matches!(controller.is_started(), Ok(true)) {
            let client_config = Configuration::Client(ClientConfiguration {
                ssid: SSID.into(),
                password: PASSWORD.into(),
                ..Default::default()
            });
            controller.set_configuration(&client_config).unwrap();
            STACK.info_fmt(fmt!("Starting wifi"));
            controller.start_async().await.unwrap();
            STACK.info_fmt(fmt!("Wifi started!"));

            STACK.info_fmt(fmt!("Scan"));
            let result = controller.scan_n_async(10).await.unwrap();
            for ap in result {
                STACK.info_fmt(fmt!("{:?}", ap));
            }
        }
        STACK.info_fmt(fmt!("About to connect..."));

        match controller.connect_async().await {
            Ok(_) => {
                STACK.info_fmt(fmt!("Wifi connected!"));
            },
            Err(e) => {
                STACK.info_fmt(fmt!("Failed to connect to wifi: {e:?}"));
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}

// #[embassy_executor::task]
// async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
//     runner.run().await
// }

/// Worker task for incoming data
#[embassy_executor::task]
async fn run_rx(mut rcvr: RxWorker, recv_buf: &'static mut [u8], scratch_buf: &'static mut [u8]) {
    loop {
        _ = rcvr.run(recv_buf, scratch_buf).await;
    }
}

/// Worker task for outgoing data
#[embassy_executor::task]
async fn run_tx(mut tx: UsbSerialJtagTx<'static, Async>) {
    loop {
        _ = tx_worker(&mut tx, OUTQ.stream_consumer()).await;
    }
}

/// Periodically send fmt'd logs over the USB-serial interface
#[embassy_executor::task]
async fn logserver() {
    let mut tckr = Ticker::every(Duration::from_secs(2));
    let mut ct = 0;
    loop {
        tckr.next().await;
        STACK.info_fmt(fmt!("log # {ct}"));
        ct += 1;
    }
}

/// Respond to any incoming pings
#[embassy_executor::task]
async fn pingserver() {
    let server = STACK.stack_bounded_endpoint_server::<ErgotPingEndpoint, 4>(None);
    let server = pin!(server);
    let mut server_hdl = server.attach();
    loop {
        server_hdl
            .serve_blocking(|req: &u32| {
                // info!("Serving ping {=u32}", req);
                STACK.info_fmt(fmt!("Serving ping {}", req));
                *req
            })
            .await
            .unwrap();
    }
}

// ---

use core::panic::PanicInfo;

#[inline(never)]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }
}
