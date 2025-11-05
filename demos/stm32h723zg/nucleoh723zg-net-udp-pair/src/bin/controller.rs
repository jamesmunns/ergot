#![no_std]
#![no_main]
extern crate alloc;

use alloc::boxed::Box;
use core::pin::pin;

use defmt::*;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpEndpoint, Ipv4Address};
use embassy_stm32::eth::PacketQueue;
use embassy_stm32::eth::{Ethernet, GenericPhy};
use embassy_stm32::pac::rcc::vals::{Pllm, Plln, Pllsrc};
use embassy_stm32::peripherals::ETH;
use embassy_stm32::rcc::mux::{
    Fdcansel, I2c4sel, I2c1235sel, Saisel, Sdmmcsel, Spi6sel, Spi45sel, Usart16910sel, Usart234578sel, Usbsel,
};
use embassy_stm32::rcc::{AHBPrescaler, APBPrescaler, LsConfig, PllDiv, Sysclk};
use embassy_stm32::rng::Rng;
use embassy_stm32::{Config, bind_interrupts, eth, peripherals, rcc, rng};
use embassy_time::{Duration, Ticker, Timer, WithTimeout};
use embedded_alloc::LlffHeap as Heap;
use ergot::exports::bbq2::traits::coordination::cas::AtomicCoord;
use ergot::interface_manager::profiles::direct_edge::embassy_net_udp_0_7::{RxTxWorker, UDP_OVER_ETH_ERGOT_FRAME_SIZE_MAX, UDP_OVER_ETH_ERGOT_PAYLOAD_SIZE_MAX};
use ergot::logging::log_v0_4::LogSink;
use ergot::toolkits::embassy_net_v0_7 as kit;
use ergot::well_known::ErgotPingEndpoint;
use ergot::{Address, topic};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use static_cell::{ConstStaticCell, StaticCell};
use {defmt_rtt as _, panic_probe as _};

const OUT_QUEUE_SIZE: usize = 4096;

type Stack = kit::EdgeStack<&'static Queue, CriticalSectionRawMutex>;
type Queue = kit::Queue<OUT_QUEUE_SIZE, AtomicCoord>;

/// Statically store our netstack
static STACK: Stack = kit::new_controller_stack(OUTQ.framed_producer(), UDP_OVER_ETH_ERGOT_FRAME_SIZE_MAX as u16);
/// Statically store our outgoing packet buffer
static OUTQ: Queue = kit::Queue::new();
/// Statically store receive buffers
static SCRATCH_BUF: ConstStaticCell<[u8; UDP_OVER_ETH_ERGOT_PAYLOAD_SIZE_MAX]> = ConstStaticCell::new([0u8; UDP_OVER_ETH_ERGOT_PAYLOAD_SIZE_MAX]);

static LOGSINK: LogSink<&'static Stack> = LogSink::new(&STACK);

#[global_allocator]
static HEAP: Heap = Heap::empty();

bind_interrupts!(struct Irqs {
    ETH => eth::InterruptHandler;
    RNG => rng::InterruptHandler<peripherals::RNG>;
});

#[embassy_executor::main]
async fn main(spawner: Spawner) -> ! {
    let mut config = Config::default();
    config.rcc.hse = Some(rcc::Hse {
        freq: embassy_stm32::time::Hertz(8_000_000),
        mode: rcc::HseMode::Oscillator,
    });
    config.rcc.ls = LsConfig::off();
    config.rcc.hsi48 = Some(Default::default()); // needed for RNG
    config.rcc.sys = Sysclk::PLL1_P;
    config.rcc.d1c_pre = AHBPrescaler::DIV1;
    config.rcc.ahb_pre = AHBPrescaler::DIV2;
    config.rcc.apb1_pre = APBPrescaler::DIV2;
    config.rcc.apb2_pre = APBPrescaler::DIV2;
    config.rcc.apb3_pre = APBPrescaler::DIV2;
    config.rcc.apb4_pre = APBPrescaler::DIV2;
    config.rcc.timer_prescaler = rcc::TimerPrescaler::DefaultX2;

    config.rcc.voltage_scale = rcc::VoltageScale::Scale0;
    config.rcc.pll1 = Some(rcc::Pll {
        source: Pllsrc::HSE,
        prediv: Pllm::DIV4,
        mul: Plln::MUL260,
        // 520Mhz
        divp: Some(PllDiv::DIV1),
        // 130Mhz
        divq: Some(PllDiv::DIV4),
        divr: None,
    });
    config.rcc.pll2 = Some(rcc::Pll {
        source: Pllsrc::HSE,

        prediv: Pllm::DIV4,
        mul: Plln::MUL200,
        // 200Mhz
        divp: Some(PllDiv::DIV2),
        // 100Mhz
        divq: Some(PllDiv::DIV4),
        // 200Mhz
        divr: Some(PllDiv::DIV2),
    });
    config.rcc.pll3 = Some(rcc::Pll {
        source: Pllsrc::HSE,
        prediv: Pllm::DIV4,
        mul: Plln::MUL192,
        // 192Mhz
        divp: Some(PllDiv::DIV2),
        // 48Mhz
        divq: Some(PllDiv::DIV8),
        // 92Mhz
        divr: Some(PllDiv::DIV4),
    });

    // 200mhz
    //config.rcc.mux.octospisel = Octospisel::PLL2_R; // no support in embassy-stm32 it seems.

    // 200mhz
    config.rcc.mux.sdmmcsel = Sdmmcsel::PLL2_R;
    // 100mhz
    config.rcc.mux.fdcansel = Fdcansel::PLL2_Q;
    // 48mhz from crystal (not RC48)
    config.rcc.mux.usbsel = Usbsel::PLL3_Q;
    // 100mhz
    config.rcc.mux.usart234578sel = Usart234578sel::PLL2_Q;
    // 100mhz
    config.rcc.mux.usart16910sel = Usart16910sel::PLL2_Q;
    // 130mhz
    config.rcc.mux.i2c1235sel = I2c1235sel::PCLK1;
    // 130mhz
    config.rcc.mux.i2c4sel = I2c4sel::PCLK4;
    // 200mhz
    config.rcc.mux.spi123sel = Saisel::PLL2_P;
    // 100mhz
    config.rcc.mux.spi45sel = Spi45sel::PLL2_Q;
    // 100mhz
    config.rcc.mux.spi6sel = Spi6sel::PLL2_Q;

    let p = embassy_stm32::init(config);

    init_heap();

    // Generate random seed.
    info!("Initializing RNG");
    let mut rng = Rng::new(p.RNG, Irqs);
    let mut seed = [0; 8];
    rng.fill_bytes(&mut seed);
    let seed = u64::from_le_bytes(seed);

    info!("Initializing ETH");
    // TODO generate mac address from CPU ID
    //      potentially using this algorythm (C): https://github.com/zephyrproject-rtos/zephyr/issues/59993#issuecomment-1644030438
    let mac_addr = [0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF];

    static PACKETS: StaticCell<PacketQueue<8, 8>> = StaticCell::new();
    // warning: Not all STM32H7 devices have the exact same pins here
    // for STM32H747XIH, replace p.PB13 for PG12
    let device = Ethernet::new(
        PACKETS.init(PacketQueue::<8, 8>::new()),
        p.ETH,
        Irqs,
        p.PA1,  // ref_clk
        p.PA2,  // mdio
        p.PC1,  // eth_mdc
        p.PA7,  // CRS_DV: Carrier Sense
        p.PC4,  // RX_D0: Received Bit 0
        p.PC5,  // RX_D1: Received Bit 1
        p.PG13, // TX_D0: Transmit Bit 0
        p.PB13, // TX_D1: Transmit Bit 1
        p.PG11, // TX_EN: Transmit Enable
        GenericPhy::new_auto(),
        mac_addr,
    );

    let config = embassy_net::Config::dhcpv4(Default::default());
    //let config = embassy_net::Config::ipv4_static(embassy_net::StaticConfigV4 {
    //    address: Ipv4Cidr::new(Ipv4Address::new(10, 42, 0, 61), 24),
    //    dns_servers: Vec::new(),
    //    gateway: Some(Ipv4Address::new(10, 42, 0, 1)),
    //});

    // Init network stack
    static RESOURCES: StaticCell<StackResources<5>> = StaticCell::new();
    let (embassy_net_stack, runner) = embassy_net::new(device, config, RESOURCES.init(StackResources::new()), seed);

    // Launch network task
    spawner.must_spawn(embassy_net_task(runner));

    // Ensure DHCP configuration is up before trying connect
    let mut attempts: u32 = 0;
    let config = loop {
        if let Some(config) = embassy_net_stack.config_v4() {
            break config;
        }

        if attempts % 10 == 0 {
            info!("Waiting for DHCP address allocation...");
        }

        attempts = attempts.wrapping_add(1);
        Timer::after(Duration::from_millis(100)).await;
    };

    info!(
        "IP address: {}, gateway: {}, dns: {}",
        config.address, config.dns_servers, config.gateway
    );

    let rx_meta = [PacketMetadata::EMPTY; 1];
    let rx_buffer = [0; 4096];
    let tx_meta = [PacketMetadata::EMPTY; 1];
    let tx_buffer = [0; 4096];

    // move the buffers into the heap, so they don't get dropped
    let rx_meta = Box::new(rx_meta);
    let rx_meta = Box::leak(rx_meta);
    let tx_meta = Box::new(tx_meta);
    let tx_meta = Box::leak(tx_meta);
    let rx_buffer = Box::new(rx_buffer);
    let rx_buffer = Box::leak(rx_buffer);
    let tx_buffer = Box::new(tx_buffer);
    let tx_buffer = Box::leak(tx_buffer);
    // You need to start a server on the host machine, for example: `nc -lu 8000`

    let mut socket = UdpSocket::new(embassy_net_stack, rx_meta, rx_buffer, tx_meta, tx_buffer);

    let port = 8000_u16;
    let remote_endpoint = IpEndpoint::new(Ipv4Address::new(192, 168, 18, 65).into(), port);
    let local_endpoint = IpEndpoint::new(config.address.address().into(), port);
    socket
        .bind(local_endpoint)
        .expect("bound");

    info!(
        "capacity, receive: {}, send: {}",
        socket.packet_recv_capacity(),
        socket.packet_send_capacity()
    );

    // Spawn I/O worker tasks
    spawner.must_spawn(run_socket(socket, SCRATCH_BUF.take(), remote_endpoint));

    // Spawn socket using tasks
    spawner.must_spawn(pingserver());
    spawner.must_spawn(pinger());

    spawner.must_spawn(yeeter());
    spawner.must_spawn(yeet_listener(0));

    LOGSINK.register_static(log::LevelFilter::Info);

    let mut tckr = Ticker::every(Duration::from_secs(2));
    let mut ct = 0;
    loop {
        tckr.next().await;
        log::info!("log # {ct}");
        ct += 1;
    }
}

type Device = Ethernet<'static, ETH, GenericPhy>;

#[embassy_executor::task]
async fn embassy_net_task(mut runner: embassy_net::Runner<'static, Device>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn run_socket(
    socket: UdpSocket<'static>,
    scratch_buf: &'static mut [u8],
    endpoint: IpEndpoint,
) {
    let consumer = OUTQ.framed_consumer();
    let mut rxtx = RxTxWorker::new_controller(&STACK, socket, (), consumer, endpoint);

    loop {
        _ = rxtx.run(scratch_buf).await;
    }
}

#[embassy_executor::task]
async fn pinger() {
    let mut ticker = Ticker::every(Duration::from_secs(1));
    let mut ctr = 0u32;
    let client = STACK
        .endpoints()
        .client::<ErgotPingEndpoint>(
            Address {
                network_id: 1,
                node_id: 2,
                port_id: 0,
            },
            None,
        );
    loop {
        ticker.next().await;
        let res = client
            .request(&ctr)
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

/// Respond to any incoming pings
#[embassy_executor::task]
async fn pingserver() {
    STACK
        .services()
        .ping_handler::<4>()
        .await;
}

topic!(YeetTopic, u64, "topic/yeet");

#[embassy_executor::task]
async fn yeeter() {
    let mut ctr = 0;
    info!("Yeeter started");
    Timer::after(Duration::from_secs(3)).await;
    loop {
        Timer::after(Duration::from_secs(5)).await;
        warn!("Sending broadcast message. ctr: {}", ctr);
        STACK
            .topics()
            .broadcast::<YeetTopic>(&ctr, None)
            .unwrap();
        ctr += 1;
    }
}

#[embassy_executor::task]
async fn yeet_listener(id: u8) {
    let subber = STACK
        .topics()
        .bounded_receiver::<YeetTopic, 4>(None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();

    info!("Yeet listener started");
    loop {
        let msg = hdl.recv().await;
        info!("{:?}: Listener id:{} got {}", msg.hdr, id, msg.t);
    }
}

#[allow(static_mut_refs)]
fn init_heap() {
    use core::mem::MaybeUninit;
    const HEAP_SIZE: usize = 65536;
    static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
    unsafe { HEAP.init(HEAP_MEM.as_ptr() as usize, HEAP_SIZE) }
}
