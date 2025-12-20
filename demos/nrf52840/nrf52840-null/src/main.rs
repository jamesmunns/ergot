//! ergot null-interface demo

#![no_std]
#![no_main]

use core::pin::pin;

use defmt::info;
use embassy_executor::{task, Spawner};
use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pull};
use embassy_time::{Duration, WithTimeout};
use ergot::{endpoint, interface_manager::profiles::null::Null, topic, Address, NetStack};
use mutex::raw_impls::cs::CriticalSectionRawMutex;

use {defmt_rtt as _, panic_probe as _};

pub static STACK: NetStack<CriticalSectionRawMutex, Null> = NetStack::new();

// Define some endpoints
endpoint!(LedEndpoint, bool, (), "led/set");
topic!(ButtonPressedTopic, u8, "button/press");

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // SYSTEM INIT
    info!("Start");
    let p = embassy_nrf::init(Default::default());

    // Tasks continue running after main returns.

    // Start the led servers first
    let led_pins = [
        Output::new(p.P0_13, Level::High, OutputDrive::Standard),
        Output::new(p.P0_14, Level::High, OutputDrive::Standard),
        Output::new(p.P0_15, Level::High, OutputDrive::Standard),
        Output::new(p.P0_16, Level::High, OutputDrive::Standard),
    ];
    let btn_pins = [
        Input::new(p.P0_11, Pull::Up),
        Input::new(p.P0_12, Pull::Up),
        Input::new(p.P0_24, Pull::Up),
        Input::new(p.P0_25, Pull::Up),
    ];
    let names = ["LED1", "LED2", "LED3", "LED4"];

    for (name, led) in names.iter().zip(led_pins.into_iter()) {
        spawner.must_spawn(led_server(name, led));
    }

    for (name, btn) in names.iter().zip(btn_pins.into_iter()) {
        spawner.must_spawn(button_worker(btn, name));
    }

    // Then start two tasks that just both listen to every button press event
    spawner.must_spawn(press_listener(1));
    spawner.must_spawn(press_listener(2));
}

#[task(pool_size = 2)]
async fn press_listener(idx: u8) {
    let recv = STACK
        .topics()
        .bounded_receiver::<ButtonPressedTopic, 4>(None);
    let recv = pin!(recv);
    let mut recv = recv.subscribe();

    loop {
        let msg = recv.recv().await;
        defmt::info!("Listener #{=u8}, button {=u8} pressed", idx, msg.t);
    }
}

#[task(pool_size = 4)]
async fn led_server(name: &'static str, mut led: Output<'static>) {
    let socket = STACK
        .endpoints()
        .bounded_server::<LedEndpoint, 4>(Some(name));
    let socket = pin!(socket);
    let mut hdl = socket.attach();

    loop {
        let _ = hdl
            .serve(async |on| {
                defmt::info!("{=str} set {=bool}", name, *on);
                if *on {
                    led.set_low();
                } else {
                    led.set_high();
                }
            })
            .await;
    }
}

#[task(pool_size = 4)]
async fn button_worker(mut btn: Input<'static>, name: &'static str) {
    let client = STACK
        .endpoints()
        .client::<LedEndpoint>(Address::unknown(), Some(name));
    loop {
        btn.wait_for_low().await;
        let res = btn
            .wait_for_high()
            .with_timeout(Duration::from_millis(5))
            .await;
        if res.is_ok() {
            continue;
        }
        client.request(&true).await.unwrap();
        let _ = STACK.topics().broadcast::<ButtonPressedTopic>(&1, None);
        btn.wait_for_high().await;
        client.request(&false).await.unwrap();
    }
}
