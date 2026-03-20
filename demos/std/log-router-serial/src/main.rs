use clap::Parser;
use ergot::{
    Address,
    toolkits::tokio_serial_v5::{RouterStack, register_router_interface},
    well_known::ErgotPingEndpoint,
};
use log::info;
use tokio::time::{interval, sleep, timeout};

use std::{io, time::Duration};

// Server
const MAX_ERGOT_PACKET_SIZE: u16 = 1024;
const TX_BUFFER_SIZE: usize = 4096;

struct NopLogger;

impl log::Log for NopLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        false
    }

    fn log(&self, _: &log::Record) {}
    fn flush(&self) {}
}

#[derive(Parser)]
struct MyArgs {
    port: String,
    baud: u32,
}

#[tokio::main]
async fn main() -> io::Result<()> {
    env_logger::init();
    let stack: RouterStack = RouterStack::new();

    // TODO: We still need pinging because edge router doesn't have any
    // other way to be assigned a net
    tokio::task::spawn(ping_all(stack.clone()));
    tokio::task::spawn(log_collect(stack.clone()));

    let args = MyArgs::parse();

    // TODO: Should the library just do this for us? something like
    let port = &args.port;
    let baud = args.baud;

    ergot::logging::log_v0_4::set_ergot_internal_log_sink(&NopLogger);

    register_router_interface(&stack, port, baud, MAX_ERGOT_PACKET_SIZE, TX_BUFFER_SIZE)
        .await
        .unwrap();

    // Spawn a worker task to handle incoming pings
    tokio::task::spawn(stack.services().ping_handler::<4>());

    loop {
        sleep(Duration::from_secs(1)).await;
    }
}

async fn ping_all(stack: RouterStack) {
    let mut ival = interval(Duration::from_secs(3));
    let mut ctr = 0u32;
    loop {
        ival.tick().await;
        let nets = stack.manage_profile(|im| im.get_nets());
        info!("Nets to ping: {:?}", nets);
        for net in nets {
            let pg = ctr;
            ctr = ctr.wrapping_add(1);
            let rr = stack.endpoints().request::<ErgotPingEndpoint>(
                Address {
                    network_id: net,
                    node_id: 2,
                    port_id: 0,
                },
                &pg,
                None,
            );
            let fut = timeout(Duration::from_millis(100), rr);
            let res = fut.await;
            info!("ping {}.2 w/ {}: {:?}", net, pg, res);
            if let Ok(Ok(msg)) = res {
                assert_eq!(msg, pg);
            }
        }
    }
}

async fn log_collect(stack: RouterStack) {
    stack.services().log_handler(64).await
}
