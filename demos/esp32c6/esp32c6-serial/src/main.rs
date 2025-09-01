#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Ticker};
use ergot::{
    exports::bbq2::traits::coordination::cas::AtomicCoord,
    logging::log_v0_4::LogSink,
    toolkits::embedded_io_async_v0_6::{self as kit, tx_worker},
};
use esp_hal::{
    Async,
    clock::CpuClock,
    timer::systimer::SystemTimer,
    usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx, UsbSerialJtagTx},
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use panic_rtt_target as _;
use static_cell::ConstStaticCell;

esp_bootloader_esp_idf::esp_app_desc!();

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

static LOGSINK: LogSink<&'static Stack> = LogSink::new(&STACK);

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    let timer0 = SystemTimer::new(p.SYSTIMER);
    esp_hal_embassy::init(timer0.alarm0);

    // Create our USB-Serial interface, which implements the embedded-io-async traits
    let (rx, tx) = UsbSerialJtag::new(p.USB_DEVICE).into_async().split();
    let rx = RxWorker::new(&STACK, rx, ());

    // Spawn I/O worker tasks
    spawner.must_spawn(run_rx(rx, RECV_BUF.take(), SCRATCH_BUF.take()));
    spawner.must_spawn(run_tx(tx));

    // Spawn socket using tasks
    spawner.must_spawn(pingserver());
    spawner.must_spawn(logserver());

    LOGSINK.register_static(log::LevelFilter::Info);
}

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
        log::info!("log # {ct}");
        ct += 1;
    }
}

/// Respond to any incoming pings
#[embassy_executor::task]
async fn pingserver() {
    STACK.services().ping_handler::<4>().await;
}
