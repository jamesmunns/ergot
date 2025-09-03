#![no_std]
#![no_main]

use core::pin::pin;

use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::{Level, Output, Pull, Speed};
use embassy_time::{Duration, Timer, with_timeout};
use ergot::interface_manager::profiles::null::Null;
use ergot::socket::endpoint::stack_vec::ServerHandle;
use ergot::{Address, NetStack, endpoint};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};
use {defmt_rtt as _, panic_probe as _};

pub static STACK: NetStack<CriticalSectionRawMutex, Null> = NetStack::new();

struct Leds<'a> {
    leds: [Output<'a>; 8],
    direction: i8,
    current_led: usize,
}

impl<'a> Leds<'a> {
    fn new(pins: [Output<'a>; 8]) -> Self {
        Self {
            leds: pins,
            direction: 1,
            current_led: 0,
        }
    }

    fn change_direction(&mut self) {
        self.direction *= -1;
    }

    fn move_next(&mut self) {
        if self.direction > 0 {
            self.current_led = (self.current_led + 1) & 7;
        } else {
            self.current_led = (8 + self.current_led - 1) & 7;
        }
    }

    async fn show(&mut self) {
        async fn recv(
            hdl: &mut ServerHandle<'_, ButtonEndpoint, &NetStack<CriticalSectionRawMutex, Null>, 2>,
        ) -> Option<ButtonEvent> {
            let mut ret = None;
            let _ = hdl.serve(async |x| ret = Some(x.clone())).await;
            ret
        }
        let socket = STACK
            .endpoints()
            .bounded_server::<ButtonEndpoint, 2>(Some("button"));
        let socket = pin!(socket);
        let mut hdl = socket.attach();
        self.leds[self.current_led].set_high();

        if let Ok(Some(new_message)) =
            with_timeout(Duration::from_millis(500), recv(&mut hdl)).await
        {
            self.leds[self.current_led].set_low();
            self.process_event(new_message).await;
        } else {
            self.leds[self.current_led].set_low();
            if let Ok(Some(new_message)) =
                with_timeout(Duration::from_millis(200), recv(&mut hdl)).await
            {
                self.process_event(new_message).await;
            }
        }
    }

    async fn flash(&mut self) {
        for _ in 0..3 {
            for led in &mut self.leds {
                led.set_high();
            }
            Timer::after_millis(500).await;
            for led in &mut self.leds {
                led.set_low();
            }
            Timer::after_millis(200).await;
        }
    }

    async fn process_event(&mut self, event: ButtonEvent) {
        match event {
            ButtonEvent::SingleClick => {
                self.move_next();
            }
            ButtonEvent::DoubleClick => {
                self.change_direction();
                self.move_next();
            }
            ButtonEvent::Hold => {
                self.flash().await;
            }
        }
    }
}

#[derive(Clone, Format, Schema, Serialize, Deserialize)]
pub enum ButtonEvent {
    SingleClick,
    DoubleClick,
    Hold,
}

endpoint!(ButtonEndpoint, ButtonEvent, (), "event/button");

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());
    let button = ExtiInput::new(p.PA0, p.EXTI0, Pull::Down);
    info!("Press the USER button...");
    let leds = [
        Output::new(p.PE9, Level::Low, Speed::Low),
        Output::new(p.PE10, Level::Low, Speed::Low),
        Output::new(p.PE11, Level::Low, Speed::Low),
        Output::new(p.PE12, Level::Low, Speed::Low),
        Output::new(p.PE13, Level::Low, Speed::Low),
        Output::new(p.PE14, Level::Low, Speed::Low),
        Output::new(p.PE15, Level::Low, Speed::Low),
        Output::new(p.PE8, Level::Low, Speed::Low),
    ];
    let leds = Leds::new(leds);

    spawner.spawn(button_waiter(button)).unwrap();
    spawner.spawn(led_blinker(leds)).unwrap();
}

#[embassy_executor::task]
async fn led_blinker(mut leds: Leds<'static>) {
    loop {
        leds.show().await;
    }
}

#[embassy_executor::task]
async fn button_waiter(mut button: ExtiInput<'static>) {
    const DOUBLE_CLICK_DELAY: u64 = 250;
    const HOLD_DELAY: u64 = 1000;

    let client = STACK
        .endpoints()
        .client::<ButtonEndpoint>(Address::unknown(), Some("button"));

    button.wait_for_rising_edge().await;
    loop {
        if with_timeout(
            Duration::from_millis(HOLD_DELAY),
            button.wait_for_falling_edge(),
        )
        .await
        .is_err()
        {
            info!("Hold");
            let _ = client.request(&ButtonEvent::Hold).await;
            button.wait_for_falling_edge().await;
        } else if with_timeout(
            Duration::from_millis(DOUBLE_CLICK_DELAY),
            button.wait_for_rising_edge(),
        )
        .await
        .is_err()
        {
            info!("Single click");
            let _ = client.request(&ButtonEvent::SingleClick).await;
        } else {
            info!("Double click");
            let _ = client.request(&ButtonEvent::DoubleClick).await;
            button.wait_for_falling_edge().await;
        }
        button.wait_for_rising_edge().await;
    }
}
