use ergot::{
    toolkits::tokio_tcp::{RouterStack, register_router_interface},
    topic,
    well_known::DeviceInfo,
};
use log::{info, warn};
use tokio::{net::TcpListener, select, time::interval};

use std::{collections::HashSet, io, pin::pin, time::Duration};

// Server
const MAX_ERGOT_PACKET_SIZE: u16 = 1024;
const TX_BUFFER_SIZE: usize = 4096;

topic!(YeetTopic, u64, "topic/yeet");

#[tokio::main]
async fn main() -> io::Result<()> {
    env_logger::init();
    let listener = TcpListener::bind("127.0.0.1:2025").await?;
    let stack: RouterStack = RouterStack::new();

    tokio::task::spawn(basic_services(stack.clone()));

    for i in 1..4 {
        tokio::task::spawn(yeet_listener(stack.clone(), i));
    }

    // TODO: Should the library just do this for us? something like
    loop {
        let (socket, addr) = listener.accept().await?;
        info!("Connect {:?}", addr);
        register_router_interface(&stack, socket, MAX_ERGOT_PACKET_SIZE, TX_BUFFER_SIZE)
            .await
            .unwrap();
    }
}

async fn basic_services(stack: RouterStack) {
    let info = DeviceInfo {
        name: Some("Ergot router".try_into().unwrap()),
        description: Some("A central router".try_into().unwrap()),
        unique_id: 2025,
    };
    // allow for discovery
    let disco_answer = stack.services().device_info_handler::<4>(&info);
    // handle incoming ping requests
    let ping_answer = stack.services().ping_handler::<4>();
    // custom service for doing discovery on a set interval, in parallel
    let disco_req = tokio::spawn(do_discovery(stack.clone()));
    // forward log messages to the log crate output
    let log_handler = stack.services().log_handler(16);

    // These all run together, we run them in a single task
    select! {
        _ = disco_answer => {},
        _ = ping_answer => {},
        _ = disco_req => {},
        _ = log_handler => {},
    }
}

async fn do_discovery(stack: RouterStack) {
    let mut max = 16;
    let mut seen = HashSet::new();
    let mut ticker = interval(Duration::from_millis(5000));
    loop {
        ticker.tick().await;
        let new_seen = stack
            .discovery()
            .discover(max, Duration::from_millis(2500))
            .await;
        max = max.max(seen.len() * 2);
        let new_seen = HashSet::from_iter(new_seen);
        let added = new_seen.difference(&seen);
        for add in added {
            warn!("Added:   {:?}", add);
        }
        let removed = seen.difference(&new_seen);
        for rem in removed {
            warn!("Removed: {:?}", rem);
        }
        seen = new_seen;

        ticker.tick().await;
    }
}

async fn yeet_listener(stack: RouterStack, id: u8) {
    let subber = stack.topics().heap_bounded_receiver::<YeetTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();

    loop {
        let msg = hdl.recv().await;
        info!("{}: Listener id:{} got {}", msg.hdr, id, msg.t);
    }
}
