//! Simple UART echo test on NUCLEO-G474RE
//!
//! Uses LPUART1 on PA2(TX)/PA3(RX) via ST-Link VCP.
//! First sends "hello" every second, then echoes received bytes.
//!
//! Test: picocom /dev/ttyACM0 -b 115200

#![no_std]
#![no_main]

use defmt::info;
use embassy_executor::Spawner;
use embassy_stm32::usart::{self, BufferedInterruptHandler, BufferedUart};
use embassy_stm32::{bind_interrupts, peripherals};
use embassy_time::{Duration, Timer};
use embedded_io_async::{Read, Write};
use ergot::logging::defmt_sink;
use static_cell::StaticCell;
use stm32g474_demos::init_rtt_channels;

use panic_probe as _;

bind_interrupts!(struct Irqs {
    LPUART1 => BufferedInterruptHandler<peripherals::LPUART1>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

    let (defmt_ch, _ergot_up, _ergot_down) = init_rtt_channels();
    static RTT_DEFMT: StaticCell<rtt_target::UpChannel> = StaticCell::new();
    let rtt_defmt = RTT_DEFMT.init(defmt_ch);
    defmt_sink::init_network_and_rtt(rtt_defmt);

    info!("UART echo test on NUCLEO-G474RE starting");

    let mut usart_config = usart::Config::default();
    usart_config.baudrate = 115200;

    static TX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    let tx_buf = &mut TX_BUF.init([0; 256])[..];
    let rx_buf = &mut RX_BUF.init([0; 256])[..];

    let mut uart = BufferedUart::new(
        p.LPUART1,
        p.PA3, // RX
        p.PA2, // TX
        tx_buf,
        rx_buf,
        Irqs,
        usart_config,
    )
    .unwrap();

    info!("LPUART1 initialized at 115200 baud");

    // Send a hello first
    let _ = uart.write_all(b"hello from STM32G474!\r\n").await;
    info!("Sent hello");

    // Echo loop
    let mut buf = [0u8; 64];
    loop {
        match uart.read(&mut buf).await {
            Ok(n) if n > 0 => {
                info!("RX: {} bytes", n);
                let _ = uart.write_all(&buf[..n]).await;
            }
            Ok(_) => {}
            Err(e) => {
                info!("RX error: {:?}", e);
                Timer::after(Duration::from_millis(100)).await;
            }
        }
    }
}
