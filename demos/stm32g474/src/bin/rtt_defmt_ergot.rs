//! Ergot over RTT + defmt forwarded over ergot network
//!
//! Tests: defmt-sink-network, defmt-sink-rtt (hybrid), defmtlog,
//!        forward_to_ergot_topic(), RttReader, RttWriter
//!
//! defmt frames are buffered into bbqueue, then sent as ergot messages
//! on ergot/.well-known/defmt topic. Also written to RTT for local debug.
//!
//! Host: ergot-rtt-host --chip STM32G474RETx --up-channel 1 --down-channel 0 \
//!         --elf target/thumbv7em-none-eabihf/release/rtt-defmt-ergot --defmt-channel 0

#![no_std]
#![no_main]

use core::pin::pin;

use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_time::{Duration, Ticker};
use ergot::{
    endpoint,
    exports::bbqueue::traits::coordination::cas::AtomicCoord,
    logging::defmt_sink::{self, DefmtConsumer},
    toolkits::embedded_io_async_v0_7::{self, Queue, RxWorker, Stack, tx_worker},
    transport::rtt::{RttReader, RttWriter},
    well_known::ErgotPingEndpoint,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use static_cell::StaticCell;
use stm32g474_demos::init_rtt_channels;

use panic_probe as _;

const ERGOT_MTU: u16 = 512;

static TX_QUEUE: StaticCell<Queue<2048, AtomicCoord>> = StaticCell::new();
static STACK: StaticCell<Stack<&'static Queue<2048, AtomicCoord>, CriticalSectionRawMutex>> =
    StaticCell::new();
static RTT_DEFMT: StaticCell<rtt_target::UpChannel> = StaticCell::new();
static RTT_UP: StaticCell<rtt_target::UpChannel> = StaticCell::new();
static RTT_DOWN: StaticCell<rtt_target::DownChannel> = StaticCell::new();

endpoint!(LedEndpoint, bool, (), "led/set");

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

    let (defmt_ch, ergot_up, ergot_down) = init_rtt_channels();

    // Hybrid init: defmt writes to both RTT (local) and bbqueue (network)
    let rtt_defmt = RTT_DEFMT.init(defmt_ch);
    let defmt_consumer = defmt_sink::init_network_and_rtt(rtt_defmt);

    info!("STM32G474 rtt-defmt-ergot demo starting");

    let rtt_up = RTT_UP.init(ergot_up);
    let rtt_down = RTT_DOWN.init(ergot_down);

    let tx_queue = TX_QUEUE.init(Queue::new());
    let stack = STACK.init(embedded_io_async_v0_7::new_target_stack(
        tx_queue.stream_producer(),
        ERGOT_MTU,
    ));

    info!("Ergot stack initialized");

    spawner.must_spawn(rx_task(stack, rtt_down));
    spawner.must_spawn(tx_task(tx_queue, rtt_up));
    spawner.must_spawn(ping_handler(stack));
    spawner.must_spawn(defmt_forwarder(stack, defmt_consumer));

    let led = Output::new(p.PA5, Level::Low, Speed::Low);
    spawner.must_spawn(led_server(stack, led));

    info!("All tasks spawned");

    let mut ticker = Ticker::every(Duration::from_secs(2));
    let mut counter: u32 = 0;
    loop {
        ticker.next().await;
        counter = counter.wrapping_add(1);
        info!("Heartbeat #{}", counter);
        if counter % 5 == 0 {
            warn!("Every 5th: count={}", counter);
        }
    }
}

/// Forward defmt frames from bbqueue to ergot network topic
#[embassy_executor::task]
async fn defmt_forwarder(
    stack: &'static Stack<&'static Queue<2048, AtomicCoord>, CriticalSectionRawMutex>,
    consumer: DefmtConsumer,
) {
    defmt_sink::forward_to_ergot_topic(&consumer, stack, None).await;
}

#[embassy_executor::task]
async fn rx_task(
    stack: &'static Stack<&'static Queue<2048, AtomicCoord>, CriticalSectionRawMutex>,
    rtt_down: &'static mut rtt_target::DownChannel,
) {
    let reader = RttReader::new(rtt_down);
    let mut worker = RxWorker::new_target(stack, reader, ());
    let mut frame_buf = [0u8; (ERGOT_MTU as usize) + 64];
    let mut scratch_buf = [0u8; 256];
    let _ = worker.run(&mut frame_buf, &mut scratch_buf).await;
}

#[embassy_executor::task]
async fn tx_task(
    tx_queue: &'static Queue<2048, AtomicCoord>,
    rtt_up: &'static mut rtt_target::UpChannel,
) {
    let mut writer = RttWriter::new(rtt_up);
    let _ = tx_worker(&mut writer, tx_queue.stream_consumer()).await;
}

#[embassy_executor::task]
async fn ping_handler(
    stack: &'static Stack<&'static Queue<2048, AtomicCoord>, CriticalSectionRawMutex>,
) {
    let server = stack
        .endpoints()
        .bounded_server::<ErgotPingEndpoint, 4>(Some("ping"));
    let server = pin!(server);
    let mut hdl = server.attach();
    loop {
        let _ = hdl
            .serve(|val: &u32| {
                info!("Ping received: {}", val);
                let v = *val;
                async move { v }
            })
            .await;
    }
}

#[embassy_executor::task]
async fn led_server(
    stack: &'static Stack<&'static Queue<2048, AtomicCoord>, CriticalSectionRawMutex>,
    mut led: Output<'static>,
) {
    let server = stack
        .endpoints()
        .bounded_server::<LedEndpoint, 4>(Some("led"));
    let server = pin!(server);
    let mut hdl = server.attach();
    loop {
        let _ = hdl
            .serve(async |on: &bool| {
                info!("LED set: {}", on);
                if *on {
                    led.set_high();
                } else {
                    led.set_low();
                }
            })
            .await;
    }
}
