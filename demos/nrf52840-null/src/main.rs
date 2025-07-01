//! ergot null-interface demo

#![no_std]
#![no_main]

use core::pin::pin;

use defmt::info;
use embassy_executor::{task, Spawner};
use embassy_nrf::{
    config::{Config as NrfConfig, HfclkSource},
    gpio::{Input, Level, Output, OutputDrive, Pull},
};
use embassy_time::{Duration, WithTimeout};
use ergot::{
    interface_manager::null::NullInterfaceManager,
    socket::{endpoint::stack_vec::Server, topic::stack_vec::Receiver},
    Address, NetStack,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use postcard_rpc::{endpoint, topic};

use {defmt_rtt as _, panic_probe as _};

pub static STACK: NetStack<CriticalSectionRawMutex, NullInterfaceManager> = NetStack::new();

// Define some endpoints
endpoint!(Led1Endpoint, bool, (), "leds/1/set");
endpoint!(Led2Endpoint, bool, (), "leds/2/set");
endpoint!(Led3Endpoint, bool, (), "leds/3/set");
endpoint!(Led4Endpoint, bool, (), "leds/4/set");
topic!(ButtonPressedTopic, u8, "button/press");

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // SYSTEM INIT
    info!("Start");
    let mut config = NrfConfig::default();
    config.hfclk_source = HfclkSource::ExternalXtal;
    let p = embassy_nrf::init(Default::default());

    // Tasks continue running after main returns.

    // Start the led servers first
    spawner.must_spawn(led_one(Output::new(
        p.P0_13,
        Level::High,
        OutputDrive::Standard,
    )));
    spawner.must_spawn(led_two(Output::new(
        p.P0_14,
        Level::High,
        OutputDrive::Standard,
    )));
    spawner.must_spawn(led_three(Output::new(
        p.P0_15,
        Level::High,
        OutputDrive::Standard,
    )));
    spawner.must_spawn(led_four(Output::new(
        p.P0_16,
        Level::High,
        OutputDrive::Standard,
    )));

    // Start the button listeners next
    spawner.must_spawn(button_one(Input::new(p.P0_11, Pull::Up)));
    spawner.must_spawn(button_two(Input::new(p.P0_12, Pull::Up)));
    spawner.must_spawn(button_three(Input::new(p.P0_24, Pull::Up)));
    spawner.must_spawn(button_four(Input::new(p.P0_25, Pull::Up)));

    // Then start two tasks that just both listen to every button press event
    spawner.must_spawn(press_listener(1));
    spawner.must_spawn(press_listener(2));
}

#[task(pool_size = 2)]
async fn press_listener(idx: u8) {
    let recv: Receiver<ButtonPressedTopic, _, _, 4> = Receiver::new(&STACK);
    let recv = pin!(recv);
    let mut recv = recv.subscribe();

    loop {
        let msg = recv.recv().await;
        defmt::info!("Listener #{=u8}, button {=u8} pressed", idx, msg.t);
    }
}

#[task]
async fn led_one(mut led: Output<'static>) {
    let socket: Server<Led1Endpoint, _, _, 4> = Server::new(&STACK);
    let socket = pin!(socket);
    let mut hdl = socket.attach();

    loop {
        let _ = hdl
            .serve(async |on| {
                defmt::info!("LED1 set {=bool}", *on);
                if *on {
                    led.set_low();
                } else {
                    led.set_high();
                }
            })
            .await;
    }
}

#[task]
async fn button_one(mut btn: Input<'static>) {
    loop {
        btn.wait_for_low().await;
        let res = btn
            .wait_for_high()
            .with_timeout(Duration::from_millis(5))
            .await;
        if res.is_ok() {
            continue;
        }
        let _ = STACK
            .req_resp::<Led1Endpoint>(Address::unknown(), &true)
            .await;
        let _ = STACK.broadcast_topic::<ButtonPressedTopic>(&1).await;
        btn.wait_for_high().await;
        let _ = STACK
            .req_resp::<Led1Endpoint>(Address::unknown(), &false)
            .await;
    }
}

#[task]
async fn led_two(mut led: Output<'static>) {
    let socket: Server<Led2Endpoint, _, _, 4> = Server::new(&STACK);
    let socket = pin!(socket);
    let mut hdl = socket.attach();

    loop {
        let _ = hdl
            .serve(async |on| {
                defmt::info!("LED2 set {=bool}", *on);
                if *on {
                    led.set_low();
                } else {
                    led.set_high();
                }
            })
            .await;
    }
}

#[task]
async fn button_two(mut btn: Input<'static>) {
    loop {
        btn.wait_for_low().await;
        let res = btn
            .wait_for_high()
            .with_timeout(Duration::from_millis(5))
            .await;
        if res.is_ok() {
            continue;
        }
        let _ = STACK
            .req_resp::<Led2Endpoint>(Address::unknown(), &true)
            .await;
        let _ = STACK.broadcast_topic::<ButtonPressedTopic>(&2).await;
        btn.wait_for_high().await;
        let _ = STACK
            .req_resp::<Led2Endpoint>(Address::unknown(), &false)
            .await;
    }
}

#[task]
async fn led_three(mut led: Output<'static>) {
    let socket: Server<Led3Endpoint, _, _, 4> = Server::new(&STACK);
    let socket = pin!(socket);
    let mut hdl = socket.attach();

    loop {
        let _ = hdl
            .serve(async |on| {
                defmt::info!("LED3 set {=bool}", *on);
                if *on {
                    led.set_low();
                } else {
                    led.set_high();
                }
            })
            .await;
    }
}

#[task]
async fn button_three(mut btn: Input<'static>) {
    loop {
        btn.wait_for_low().await;
        let res = btn
            .wait_for_high()
            .with_timeout(Duration::from_millis(5))
            .await;
        if res.is_ok() {
            continue;
        }
        let _ = STACK
            .req_resp::<Led3Endpoint>(Address::unknown(), &true)
            .await;
        let _ = STACK.broadcast_topic::<ButtonPressedTopic>(&3).await;
        btn.wait_for_high().await;
        let _ = STACK
            .req_resp::<Led3Endpoint>(Address::unknown(), &false)
            .await;
    }
}

#[task]
async fn led_four(mut led: Output<'static>) {
    let socket: Server<Led4Endpoint, _, _, 4> = Server::new(&STACK);
    let socket = pin!(socket);
    let mut hdl = socket.attach();

    loop {
        let _ = hdl
            .serve(async |on| {
                defmt::info!("LED4 set {=bool}", *on);
                if *on {
                    led.set_low();
                } else {
                    led.set_high();
                }
            })
            .await;
    }
}

#[task]
async fn button_four(mut btn: Input<'static>) {
    loop {
        btn.wait_for_low().await;
        let res = btn
            .wait_for_high()
            .with_timeout(Duration::from_millis(5))
            .await;
        if res.is_ok() {
            continue;
        }
        let _ = STACK
            .req_resp::<Led4Endpoint>(Address::unknown(), &true)
            .await;
        let _ = STACK.broadcast_topic::<ButtonPressedTopic>(&4).await;
        btn.wait_for_high().await;
        let _ = STACK
            .req_resp::<Led4Endpoint>(Address::unknown(), &false)
            .await;
    }
}
