#![no_std]
#![no_main]

use core::pin::pin;

use embassy_executor::{Spawner, task};
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_time::{Duration, Ticker};
use ergot::{Address, NetStack, endpoint, interface_manager::profiles::null::Null};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use {defmt_rtt as _, panic_probe as _};

pub static STACK: NetStack<CriticalSectionRawMutex, Null> = NetStack::new();

// Define some endpoints
endpoint!(LedEndpoint, bool, (), "led/set");

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

    spawner.must_spawn(led_server(Output::new(p.PB14, Level::High, Speed::Low)));

    let mut ticker = Ticker::every(Duration::from_millis(500));
    let client = STACK
        .endpoints()
        .client::<LedEndpoint>(Address::unknown(), Some("led"));
    loop {
        ticker.next().await;
        let _ = client.request(&true).await;
        ticker.next().await;
        let _ = client.request(&false).await;
    }
}

#[task]
async fn led_server(mut led: Output<'static>) {
    let socket = STACK
        .endpoints()
        .bounded_server::<LedEndpoint, 2>(Some("led"));
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
