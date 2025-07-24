//! RP2040 example with two devices connected by UART

#![no_std]
#![no_main]

use core::pin::pin;

use bbq2::{
    queue::BBQueue,
    traits::{coordination::cs::CsCoord, notifier::maitake::MaiNotSpsc, storage::Inline},
};
use defmt::info;
use embassy_executor::{task, Spawner};
use embassy_rp::{
    bind_interrupts,
    gpio::{Level, Output},
    peripherals::UART0,
    uart::{self, Uart},
};
use embassy_time::{Duration, Ticker, WithTimeout};
use ergot::{endpoint, well_known::ErgotPingEndpoint, Address, NetStack};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use rp2040_serial_pair::{PairedUartProfile, RxWorker, TxWorker};
use static_cell::ConstStaticCell;

use {defmt_rtt as _, panic_probe as _};

pub const TX_QUEUE_LEN: usize = 4096;
pub const RX_BUF_LEN: usize = 512;

pub type TxQueue = BBQueue<Inline<TX_QUEUE_LEN>, CsCoord, MaiNotSpsc>;
pub type Stack = NetStack<CriticalSectionRawMutex, PairedUartProfile<&'static TxQueue>>;

pub static TX_QUEUE: TxQueue = TxQueue::new();
pub static STACK: Stack = PairedUartProfile::new_controller_stack::<CriticalSectionRawMutex>(
    TX_QUEUE.framed_producer(),
    RX_BUF_LEN as u16,
);
pub static RX_BUF: ConstStaticCell<[u8; RX_BUF_LEN]> = ConstStaticCell::new([0u8; RX_BUF_LEN]);

// Define some endpoints
endpoint!(LedEndpoint, bool, (), "led/set");

bind_interrupts!(struct Irqs {
    UART0_IRQ => uart::InterruptHandler<UART0>;
});

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // SYSTEM INIT
    info!("Start");
    let p = embassy_rp::init(Default::default());
    let mut cfg = uart::Config::default();
    cfg.baudrate = 1_000_000;
    cfg.data_bits = uart::DataBits::DataBits8;
    cfg.stop_bits = uart::StopBits::STOP1;
    cfg.parity = uart::Parity::ParityNone;

    let uart = Uart::new(p.UART0, p.PIN_0, p.PIN_1, Irqs, p.DMA_CH0, p.DMA_CH1, cfg);
    let (tx, rx) = uart.split();
    let tx_worker = TxWorker::new_controller(&STACK, &TX_QUEUE, tx)
        .map_err(drop)
        .unwrap();

    let rx_worker = RxWorker::new_controller(&STACK, rx, RX_BUF.take());

    spawner.must_spawn(led_server(Output::new(p.PIN_25, Level::High)));
    spawner.must_spawn(tx_task(tx_worker));
    spawner.must_spawn(rx_task(rx_worker));
    spawner.must_spawn(pingserver());
    spawner.must_spawn(pinger());

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

#[task]
async fn pinger() {
    let mut ticker = Ticker::every(Duration::from_secs(1));
    let mut ctr = 0u32;
    loop {
        ticker.next().await;
        let res = STACK
            .req_resp::<ErgotPingEndpoint>(
                Address {
                    network_id: 1,
                    node_id: 2,
                    port_id: 0,
                },
                &ctr,
                None,
            )
            .with_timeout(Duration::from_millis(100))
            .await;
        match res {
            Ok(Ok(n)) => {
                defmt::info!("Got ping {=u32} -> {=u32}", ctr, n);
                ctr = ctr.wrapping_add(1);
            }
            Ok(Err(_e)) => {
                defmt::warn!("Net stack ping error");
            }
            Err(_) => {
                defmt::warn!("Ping timeout");
            }
        }
    }
}

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
async fn tx_task(
    mut txw: TxWorker<&'static TxQueue, &'static Stack, uart::UartTx<'static, uart::Async>>,
) {
    loop {
        txw.run_until_err().await;
    }
}

#[task]
async fn rx_task(
    mut rxw: RxWorker<
        'static,
        &'static TxQueue,
        &'static Stack,
        uart::UartRx<'static, uart::Async>,
    >,
) {
    rxw.run().await;
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
