//! ergot null-interface demo

#![no_std]
#![no_main]

use core::pin::pin;

use defmt::info;
use embassy_executor::{task, Spawner};
use embassy_rp::gpio::{Level, Output};
use embassy_time::{Duration, Ticker};
use ergot::{endpoint, interface_manager::null::NullInterfaceManager, Address, NetStack};
use mutex::raw_impls::cs::CriticalSectionRawMutex;

use {defmt_rtt as _, panic_probe as _};

pub static STACK: NetStack<CriticalSectionRawMutex, NullInterfaceManager> = NetStack::new();

// Define some endpoints
endpoint!(LedEndpoint, bool, (), "led/set");

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // SYSTEM INIT
    info!("Start");
    let p = embassy_rp::init(Default::default());

    spawner.must_spawn(led_server(Output::new(p.PIN_25, Level::High)));

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
