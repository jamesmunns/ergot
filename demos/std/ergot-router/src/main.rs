use ergot::{
    Address,
    toolkits::tokio_tcp::{RouterStack, register_router_interface},
    topic,
    well_known::ErgotPingEndpoint,
};
use log::info;
use tokio::{
    net::TcpListener,
    time::{interval, timeout},
};

use std::{io, pin::pin, time::Duration};

// Server
const MAX_ERGOT_PACKET_SIZE: u16 = 1024;
const TX_BUFFER_SIZE: usize = 4096;

topic!(YeetTopic, u64, "topic/yeet");

#[tokio::main]
async fn main() -> io::Result<()> {
    env_logger::init();
    let listener = TcpListener::bind("127.0.0.1:2025").await?;
    let stack: RouterStack = RouterStack::new();

    tokio::task::spawn(ping_all(stack.clone()));

    for i in 1..4 {
        tokio::task::spawn(yeet_listener(stack.clone(), i));
    }

    // TODO: Should the library just do this for us? something like
    loop {
        let (socket, addr) = listener.accept().await?;
        info!("Connect {addr:?}");
        register_router_interface(&stack, socket, MAX_ERGOT_PACKET_SIZE, TX_BUFFER_SIZE)
            .await
            .unwrap();
    }
}

async fn ping_all(stack: RouterStack) {
    let mut ival = interval(Duration::from_secs(3));
    let mut ctr = 0u32;
    loop {
        ival.tick().await;
        let nets = stack.manage_profile(|im| im.get_nets());
        info!("Nets to ping: {nets:?}");
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
            info!("ping {net}.2 w/ {pg}: {res:?}");
            if let Ok(Ok(msg)) = res {
                assert_eq!(msg, pg);
            }
        }
    }
}

async fn yeet_listener(stack: RouterStack, id: u8) {
    let subber = stack.topics().heap_bounded_receiver::<YeetTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();

    loop {
        let msg = hdl.recv().await;
        info!("Listener id:{id} got {msg:?}");
    }
}
