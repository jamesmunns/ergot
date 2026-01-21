//! Ergot over serial (USART2) + defmt forwarded over ergot network
//!
//! Ergot protocol runs over USART2 (PA2/PA3, ST-Link VCP).
//! defmt frames are buffered and forwarded as ergot messages on
//! ergot/.well-known/defmt topic. Also written to RTT for local debug.
//!
//! Host: connect via serial port to receive ergot + defmt frames

#![no_std]
#![no_main]

use core::pin::pin;

use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::usart::{self, BufferedInterruptHandler, BufferedUart};
use embassy_stm32::{bind_interrupts, peripherals};
use embassy_time::{Duration, Ticker};
use ergot::{
    endpoint,
    exports::bbqueue::traits::coordination::cas::AtomicCoord,
    logging::defmt_sink::{self, DefmtConsumer},
    toolkits::embedded_io_async_v0_7::{self, Queue, RxWorker, Stack, tx_worker},
    well_known::ErgotPingEndpoint,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use static_cell::StaticCell;
use stm32g474_demos::init_rtt_channels;

use panic_probe as _;

bind_interrupts!(struct Irqs {
    LPUART1 => BufferedInterruptHandler<peripherals::LPUART1>;
});

const ERGOT_MTU: u16 = 512;

static TX_QUEUE: StaticCell<Queue<2048, AtomicCoord>> = StaticCell::new();
static STACK: StaticCell<Stack<&'static Queue<2048, AtomicCoord>, CriticalSectionRawMutex>> =
    StaticCell::new();

endpoint!(LedEndpoint, bool, (), "led/set");

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

    // RTT for local defmt output
    let (defmt_ch, _ergot_up, _ergot_down) = init_rtt_channels();

    // Hybrid defmt: RTT (local) + bbqueue (network forwarding)
    static RTT_DEFMT: StaticCell<rtt_target::UpChannel> = StaticCell::new();
    let rtt_defmt = RTT_DEFMT.init(defmt_ch);
    let defmt_consumer = defmt_sink::init_network_and_rtt(rtt_defmt);

    info!("STM32G474 serial-defmt demo starting");

    // LPUART1 (PA2=TX, PA3=RX) via ST-Link VCP on NUCLEO-G474RE
    let mut usart_config = usart::Config::default();
    usart_config.baudrate = 115200;

    static TX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    let tx_buf = &mut TX_BUF.init([0; 256])[..];
    let rx_buf = &mut RX_BUF.init([0; 256])[..];

    let uart = BufferedUart::new(
        p.LPUART1,
        p.PA3, // RX
        p.PA2, // TX
        tx_buf,
        rx_buf,
        Irqs,
        usart_config,
    )
    .unwrap();
    let (uart_tx, uart_rx) = uart.split();

    info!("LPUART1 initialized at 115200 baud");

    // Ergot stack
    let tx_queue = TX_QUEUE.init(Queue::new());
    let stack = STACK.init(embedded_io_async_v0_7::new_target_stack(
        tx_queue.stream_producer(),
        None,
        ERGOT_MTU,
    ));

    info!("Ergot stack initialized");

    spawner.must_spawn(rx_task(stack, uart_rx));
    spawner.must_spawn(tx_task(tx_queue, uart_tx));
    spawner.must_spawn(ping_handler(stack));
    spawner.must_spawn(defmt_forwarder(stack, defmt_consumer));

    let led = Output::new(p.PA5, Level::Low, Speed::Low);
    spawner.must_spawn(led_server(stack, led));

    info!("All tasks spawned");

    // Main loop: periodic heartbeat logs
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

#[embassy_executor::task]
async fn rx_task(
    stack: &'static Stack<&'static Queue<2048, AtomicCoord>, CriticalSectionRawMutex>,
    uart_rx: usart::BufferedUartRx<'static>,
) {
    let mut worker = RxWorker::new_target(stack, uart_rx, ());
    let mut frame_buf = [0u8; (ERGOT_MTU as usize) + 64];
    let mut scratch_buf = [0u8; 256];
    let _ = worker.run(&mut frame_buf, &mut scratch_buf).await;
}

#[embassy_executor::task]
async fn tx_task(
    tx_queue: &'static Queue<2048, AtomicCoord>,
    mut uart_tx: usart::BufferedUartTx<'static>,
) {
    let _ = tx_worker(&mut uart_tx, tx_queue.stream_consumer()).await;
}

#[embassy_executor::task]
async fn defmt_forwarder(
    stack: &'static Stack<&'static Queue<2048, AtomicCoord>, CriticalSectionRawMutex>,
    consumer: DefmtConsumer,
) {
    defmt_sink::forward_to_ergot_topic(&consumer, stack, None).await;
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
