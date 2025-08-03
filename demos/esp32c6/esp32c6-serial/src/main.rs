#![no_std]
#![no_main]

use core::pin::pin;

use defmt::info;
use embassy_executor::Spawner;
use ergot::{
    exports::bbq2::traits::coordination::cas::AtomicCoord,
    toolkits::embedded_io_async_v0_6::{self as kit, tx_worker},
    well_known::ErgotPingEndpoint,
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

// Our nrf52840-specific USB driver
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

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    // rtt_target::rtt_init_defmt!();

    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    let timer0 = SystemTimer::new(p.SYSTIMER);
    esp_hal_embassy::init(timer0.alarm0);

    let (rx, tx) = UsbSerialJtag::new(p.USB_DEVICE).into_async().split();

    static RECV_BUF: ConstStaticCell<[u8; MAX_PACKET_SIZE]> =
        ConstStaticCell::new([0u8; MAX_PACKET_SIZE]);
    static SCRATCH_BUF: ConstStaticCell<[u8; 64]> = ConstStaticCell::new([0u8; 64]);
    let rx = RxWorker::new(STACK.base(), rx);
    spawner.must_spawn(run_rx(rx, RECV_BUF.take(), SCRATCH_BUF.take()));
    spawner.must_spawn(run_tx(tx));
    spawner.must_spawn(pingserver());
}

#[embassy_executor::task]
async fn run_rx(mut rcvr: RxWorker, recv_buf: &'static mut [u8], scratch_buf: &'static mut [u8]) {
    loop {
        _ = rcvr.run(recv_buf, scratch_buf).await;
    }
}

#[embassy_executor::task]
async fn run_tx(mut tx: UsbSerialJtagTx<'static, Async>) {
    loop {
        _ = tx_worker(&mut tx, OUTQ.stream_consumer()).await;
    }
}

#[embassy_executor::task]
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
