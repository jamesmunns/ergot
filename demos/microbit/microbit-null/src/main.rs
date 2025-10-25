//! ergot null-interface demo for microbit

#![no_std]
#![no_main]

use core::{mem, pin::pin};

use defmt::info;
use embassy_executor::{task, Spawner};
use embassy_nrf::{
    config::{Config as NrfConfig, HfclkSource},
    gpio::{Input, Level, Output, OutputDrive, Pull},
};
use embassy_time::{Duration, WithTimeout};
use ergot::{endpoint, interface_manager::profiles::null::Null, topic, Address, NetStack};
use mutex::raw_impls::cs::CriticalSectionRawMutex;

use {defmt_rtt as _, panic_probe as _};

pub static STACK: NetStack<CriticalSectionRawMutex, Null> = NetStack::new();

// Define an endpoint for toggling an LED and a topic for button presses
endpoint!(LedEndpoint, bool, (), "led/set");
topic!(ButtonPressedTopic, u8, "button/press");

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // SYSTEM INIT
    info!("Start");
    let mut config = NrfConfig::default();
    config.hfclk_source = HfclkSource::ExternalXtal;
    let p = embassy_nrf::init(Default::default());

    // Tasks continue running after main returns.

    // Configure the LED pins that we use.
    let led_pins = [
        Output::new(p.P0_28, Level::High, OutputDrive::Standard), // LED col 1
        Output::new(p.P0_31, Level::High, OutputDrive::Standard), // LED col 3
        Output::new(p.P0_30, Level::High, OutputDrive::Standard), // LED col 5
    ];

    let led_names = ["LED1", "LED2", "LED3"];

    // Put common anode to high and prevent it from being dropped when main returns.
    let row3 = Output::new(p.P0_15, Level::High, OutputDrive::Standard);
    mem::forget(row3);

    // Configure the button pins that we use.
    let btn_pins = [
        Input::new(p.P0_14, Pull::Up),   // Button A = 1
        Input::new(p.P1_04, Pull::None), // Touch Logo = 2, must be floating
        Input::new(p.P0_23, Pull::Up),   // Button B = 3
    ];

    // Spawn the LED servers, which will toggle the LEDs when adequate messages are received.
    for (name, led) in led_names.iter().zip(led_pins.into_iter()) {
        spawner.must_spawn(led_server(name, led));
    }

    // Spawn the button workers, which will listen for button presses and message these events
    for ((btn_idx, name), btn) in led_names.iter().enumerate().zip(btn_pins.into_iter()) {
        // Note: the following `usize` to `u8` cast is safe, as we have only 3 buttons
        spawner.must_spawn(button_worker(btn, name, (btn_idx + 1) as u8));
    }

    // Then start two tasks that just both listen and log every button press event
    spawner.must_spawn(press_listener(1));
    spawner.must_spawn(press_listener(2));
}

/// A task that listens to button press broadcast messages.
///
/// It subscribes to the `ButtonPressedTopic` and logs every message it receives.
#[task(pool_size = 2)]
async fn press_listener(idx: u8) {
    // Declare and subscribe to the button pressed topic
    let recv = STACK
        .topics()
        .bounded_receiver::<ButtonPressedTopic, 4>(None);
    let recv = pin!(recv);
    let mut recv = recv.subscribe();

    loop {
        // Now we wait for messages and log them when they arrive
        let msg = recv.recv().await;
        defmt::info!("Listener #{=u8}, button {=u8} pressed", idx, msg.t);
    }
}

/// A task to run the LED server.
///
/// Creates a socket for the `LedEndpoint` and waits for the incoming messages.
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

/// A task to handle button presses.
///
/// When a button is pressed, it sends a message to the corresponding `LedEndpoint` to turn the
/// specific LED on/off. It also broadcasts a message to the `ButtonPressedTopic`.
#[task(pool_size = 4)]
async fn button_worker(mut btn: Input<'static>, name: &'static str, btn_idx: u8) {
    // Create a client for the corresponding `LedENdpoint`.
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

        // So the button was pressed, turn on the LED by sending a message...
        client.request(&true).await.unwrap();
        // ... and broadcast to the `ButtonPressedTopic`
        let _ = STACK
            .topics()
            .broadcast::<ButtonPressedTopic>(&btn_idx, None);

        btn.wait_for_high().await;
        // Button released, turn off the LED
        client.request(&false).await.unwrap();
    }
}
