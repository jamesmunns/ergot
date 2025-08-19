//! ergot null-interface demo

#![no_std]
#![no_main]

use core::pin::pin;

use defmt::{info, warn};
use embassy_executor::{task, Spawner};
use embassy_rp::{
    bind_interrupts,
    gpio::{Input, Level, Output},
    peripherals::USB,
    pwm::{self, Pwm, PwmOutput, SetDutyCycle},
    spi::{self, Spi},
    usb,
};
use embassy_time::{Delay, Duration, Instant, Ticker, Timer};
use embassy_usb::{driver::Driver, Config, UsbDevice};
use embedded_hal::spi::SpiDevice;
use embedded_hal_bus::spi::ExclusiveDevice;
use ergot::{
    ergot_base::interface_manager::Profile,
    exports::bbq2::{prod_cons::framed::FramedConsumer, traits::coordination::cs::CsCoord},
    fmt,
    interface_manager::InterfaceState,
    toolkits::embassy_usb_v0_5 as kit,
    topic,
    well_known::ErgotPingEndpoint,
};
use lsm6ds3tr::Acc;
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use shared_icd::tilt::{DataTopic, Datas, PwmSetEndpoint};
use static_cell::{ConstStaticCell, StaticCell};

use {defmt_rtt as _, panic_probe as _};

pub mod lsm6ds3tr;

const OUT_QUEUE_SIZE: usize = 4096;
const MAX_PACKET_SIZE: usize = 1024;

// Our rp2040-specific USB driver
pub type AppDriver = usb::Driver<'static, USB>;
// The type of our RX Worker
type RxWorker = kit::RxWorker<&'static Queue, CriticalSectionRawMutex, AppDriver>;
// The type of our netstack
type Stack = kit::Stack<&'static Queue, CriticalSectionRawMutex>;
// The type of our outgoing queue
type Queue = kit::Queue<OUT_QUEUE_SIZE, CsCoord>;

/// Statically store our netstack
static STACK: Stack = kit::new_target_stack(OUTQ.framed_producer(), MAX_PACKET_SIZE as u16);
/// Statically store our USB app buffers
static STORAGE: kit::WireStorage<256, 256, 64, 256> = kit::WireStorage::new();
/// Statically store our outgoing packet buffer
static OUTQ: Queue = kit::Queue::new();

bind_interrupts!(pub struct Irqs {
    USBCTRL_IRQ => usb::InterruptHandler<USB>;
});

fn usb_config(serial: &'static str) -> Config<'static> {
    let mut config = Config::new(0x16c0, 0x27DD);
    config.manufacturer = Some("OneVariable");
    config.product = Some("ergot-pico");
    config.serial_number = Some(serial);

    // Required for windows compatibility.
    // https://developer.nordicsemi.com/nRF_Connect_SDK/doc/1.9.1/kconfig/CONFIG_CDC_ACM_IAD.html#help
    config.device_class = 0xEF;
    config.device_sub_class = 0x02;
    config.device_protocol = 0x01;
    config.composite_with_iads = true;

    config
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // SYSTEM INIT
    info!("Start");
    let p = embassy_rp::init(Default::default());
    // Obtain the flash ID
    let unique_id: u64 = embassy_rp::otp::get_chipid().unwrap();
    static SERIAL_STRING: StaticCell<[u8; 16]> = StaticCell::new();
    let mut ser_buf = [b' '; 16];
    // This is a simple number-to-hex formatting
    unique_id
        .to_be_bytes()
        .iter()
        .zip(ser_buf.chunks_exact_mut(2))
        .for_each(|(b, chs)| {
            let mut b = *b;
            for c in chs {
                *c = match b >> 4 {
                    v @ 0..10 => b'0' + v,
                    v @ 10..16 => b'A' + (v - 10),
                    _ => b'X',
                };
                b <<= 4;
            }
        });
    let ser_buf = SERIAL_STRING.init(ser_buf);
    let ser_buf = core::str::from_utf8(ser_buf.as_slice()).unwrap();

    let imu_int1 = p.PIN_6;
    let _imu_int2 = p.PIN_7;
    let imu_sdo = p.PIN_8;
    let imu_cs = p.PIN_9;
    let imu_sck = p.PIN_10;
    let imu_sdi = p.PIN_11;

    let scope_0 = p.PIN_1;
    let scope_1 = p.PIN_3;
    let scope_2 = p.PIN_0;
    let scope_3 = p.PIN_2;

    let mut scope_0 = Output::new(scope_0, Level::High);
    let mut scope_1 = Output::new(scope_1, Level::High);
    let mut scope_2 = Output::new(scope_2, Level::High);
    let mut scope_3 = Output::new(scope_3, Level::High);

    let spi = Spi::new(p.SPI1, imu_sck, imu_sdi, imu_sdo, p.DMA_CH0, p.DMA_CH1, {
        let mut cfg = spi::Config::default();
        cfg.frequency = 10_000_000;
        cfg
    });
    let spi = ExclusiveDevice::new(spi, Output::new(imu_cs, Level::High), Delay).unwrap();
    Timer::after_millis(100).await;

    // USB/RPC INIT
    let driver = usb::Driver::new(p.USB, Irqs);
    let config = usb_config(ser_buf);
    let (device, tx_impl, ep_out) = STORAGE.init_ergot(driver, config);

    static RX_BUF: ConstStaticCell<[u8; MAX_PACKET_SIZE]> =
        ConstStaticCell::new([0u8; MAX_PACKET_SIZE]);
    let rxvr: RxWorker = kit::RxWorker::new(STACK.base(), ep_out);
    spawner.must_spawn(usb_task(device));
    spawner.must_spawn(run_tx(tx_impl, OUTQ.framed_consumer()));
    spawner.must_spawn(run_rx(rxvr, RX_BUF.take()));
    spawner.must_spawn(pingserver());
    spawner.must_spawn(yeeter());

    // 6A/B
    let mut config = pwm::Config::default();
    config.invert_a = false;
    config.invert_b = false;
    config.phase_correct = false;
    config.enable = true;
    config.compare_a = 0xFFFF;
    config.compare_b = 0xFFFF;
    config.top = 0xFFFF;

    let (Some(pwm1), Some(pwm5)) =
        Pwm::new_output_ab(p.PWM_SLICE6, p.PIN_12, p.PIN_13, config.clone()).split()
    else {
        todo!()
    };
    // 7A/B
    let (Some(pwm2), Some(pwm6)) =
        Pwm::new_output_ab(p.PWM_SLICE7, p.PIN_14, p.PIN_15, config.clone()).split()
    else {
        todo!()
    };
    // 0A/B
    let (Some(pwm3), Some(pwm7)) =
        Pwm::new_output_ab(p.PWM_SLICE0, p.PIN_16, p.PIN_17, config.clone()).split()
    else {
        todo!()
    };
    // 1A/B
    let (Some(pwm4), Some(pwm8)) =
        Pwm::new_output_ab(p.PWM_SLICE1, p.PIN_18, p.PIN_19, config.clone()).split()
    else {
        todo!()
    };

    let pwms = [
        ("channel1", pwm1),
        ("channel2", pwm2),
        ("channel3", pwm3),
        ("channel4", pwm4),
        ("channel5", pwm5),
        ("channel6", pwm6),
        ("channel7", pwm7),
        ("channel8", pwm8),
    ];

    // Wait for connection
    let mut ticker = Ticker::every(Duration::from_millis(500));
    loop {
        ticker.next().await;
        let has_addr = STACK.manage_profile(|p| {
            let Some(state) = p.interface_state(()) else {
                return false;
            };
            let InterfaceState::Active { net_id, .. } = state else {
                return false;
            };
            net_id != 0
        });

        if has_addr {
            STACK.info_fmt(fmt!("Connected :)"));
            break;
        }
    }

    for (channel, pwm) in pwms {
        spawner.must_spawn(pwm_server(pwm, channel));
    }

    let mut imu_int1 = Input::new(imu_int1, embassy_rp::gpio::Pull::Up);
    let mut imu = lsm6ds3tr::Acc { d: spi };
    imu.james_setup().await.unwrap();
    let mut datas = Datas::default();

    // Drain data in fifo if any
    loop {
        let [remain_l, remain_h, _pat, _unk2, _data, _datb] =
            imu.read_one_fifo_with_bits().unwrap();

        let remain = u16::from_le_bytes([remain_l, remain_h & 0b0000_0111]);
        if remain == 0 {
            break;
        }
    }

    loop {
        // Reset all Pins
        scope_0.set_high();
        scope_1.set_high();
        scope_2.set_high();
        scope_3.set_high();

        // Wait for the sensor to say "data is ready"
        imu_int1.wait_for_low().await;
        scope_0.set_low();

        // Drain data from the sensor
        let res = filler(&mut datas, &mut imu);
        scope_1.set_low();

        match res {
            Ok(()) => {}
            Err(ReadErr::Err(_e)) => todo!(),
            Err(ReadErr::UnexpectedEnd) => {
                defmt::error!("UNEXPECTED");
                continue;
            }
            Err(ReadErr::WrongPat) => {
                defmt::error!("WRONG PAT");
                continue;
            }
        }
        scope_2.set_low();

        // Publish data
        _ = STACK.broadcast_topic::<DataTopic>(&datas, None);
        scope_3.set_low();
    }
}

enum ReadErr<E> {
    Err(E),
    UnexpectedEnd,
    WrongPat,
}

impl<E> From<E> for ReadErr<E> {
    fn from(value: E) -> Self {
        Self::Err(value)
    }
}

fn take_one<T: SpiDevice>(
    acc: &mut Acc<T>,
    exp_pat: u8,
    is_last: bool,
) -> Result<i16, ReadErr<T::Error>> {
    let [remain_l, remain_h, pat, _unk2, data, datb] = acc.read_one_fifo_with_bits()?;
    // defmt::info!("r:{=u8}, ep:{=u8}, ap:{=u8}, il:{=bool}", remain, exp_pat, pat, is_last);
    let remain = u16::from_le_bytes([remain_l, remain_h & 0b0000_0111]);
    if !is_last && remain <= 1 {
        return Err(ReadErr::UnexpectedEnd);
    }
    if exp_pat != pat {
        return Err(ReadErr::WrongPat);
    }
    let dat = i16::from_le_bytes([data, datb]);
    Ok(dat)
}

fn filler<T: SpiDevice>(datas: &mut Datas, acc: &mut Acc<T>) -> Result<(), ReadErr<T::Error>> {
    datas.mcu_timestamp = Instant::now().as_micros();
    let _len = datas.inner.len();
    let mut first = Some(loop {
        let v = take_one(acc, 0, false);
        match v {
            Ok(v) => break v,
            Err(ReadErr::WrongPat) => continue,
            Err(e) => return Err(e),
        }
    });
    for data in datas.inner.iter_mut() {
        data.gyro_p = if let Some(v) = first.take() {
            v
        } else {
            take_one(acc, 0, false)?
        };
        data.gyro_r = take_one(acc, 1, false)?;
        data.gyro_y = take_one(acc, 2, false)?;
        data.accl_x = take_one(acc, 3, false)?;
        data.accl_y = take_one(acc, 4, false)?;
        data.accl_z = take_one(acc, 5, false)?;
        let ts0 = take_one(acc, 6, false)?;
        let ts1 = take_one(acc, 7, false)?;
        let _ts2 = take_one(acc, 8, true)?;
        let [ts00, ts01] = ts0.to_le_bytes();
        let [_ts10, ts11] = ts1.to_le_bytes();
        data.imu_timestamp = u32::from_le_bytes([ts11, ts00, ts01, 0]);
    }
    Ok(())
}

topic!(YeetTopic, u64, "topic/yeet");

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
async fn yeeter() {
    let mut ctr = 0;
    Timer::after(Duration::from_secs(3)).await;
    loop {
        Timer::after(Duration::from_secs(5)).await;
        warn!("Sending broadcast message");
        let _ = STACK.broadcast_topic::<YeetTopic>(&ctr, None);
        ctr += 1;
    }
}

/// This handles the low level USB management
#[embassy_executor::task]
pub async fn usb_task(mut usb: UsbDevice<'static, AppDriver>) {
    usb.run().await;
}

#[task]
async fn run_rx(rcvr: RxWorker, recv_buf: &'static mut [u8]) {
    rcvr.run(recv_buf, kit::USB_FS_MAX_PACKET_SIZE).await;
}

#[task]
async fn run_tx(
    mut ep_in: <AppDriver as Driver<'static>>::EndpointIn,
    rx: FramedConsumer<&'static Queue>,
) {
    kit::tx_worker::<AppDriver, OUT_QUEUE_SIZE, CsCoord>(
        &mut ep_in,
        rx,
        kit::DEFAULT_TIMEOUT_MS_PER_FRAME,
        kit::USB_FS_MAX_PACKET_SIZE,
    )
    .await;
}

#[task(pool_size = 8)]
async fn pwm_server(mut pwm: PwmOutput<'static>, name: &'static str) {
    let socket = STACK.stack_bounded_endpoint_server::<PwmSetEndpoint, 2>(Some(name));
    let socket = pin!(socket);
    let mut hdl = socket.attach();

    loop {
        let _ = hdl
            .serve_blocking(|data| {
                let val = data.clamp(0.0, 1.0);
                let val = val * const { u16::MAX as f32 };
                let val = val as u16;
                _ = pwm.set_duty_cycle(val);
                Instant::now().as_ticks()
            })
            .await;
    }
}
