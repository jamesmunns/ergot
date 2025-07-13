use ergot::{
    Address, NetStack,
    interface_manager::std_tcp_router::{StdTcpIm, register_interface},
    topic,
    well_known::ErgotPingEndpoint,
};
use log::{info, warn};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::{
    net::TcpListener,
    time::{interval, timeout},
};

use std::{io, pin::pin, time::Duration};

// Server
static STACK: NetStack<CriticalSectionRawMutex, StdTcpIm> = NetStack::new();

topic!(YeetTopic, u64, "topic/yeet");

#[tokio::main]
async fn main() -> io::Result<()> {
    env_logger::init();
    let listener = TcpListener::bind("127.0.0.1:2025").await?;

    tokio::task::spawn(ping_all());

    for i in 1..4 {
        tokio::task::spawn(yeet_listener(i));
    }

    // TODO: Should the library just do this for us? something like
    // `serve(listener).await`, or just `serve(&STACK, "127.0.0.1:2025").await`?
    loop {
        let (socket, addr) = listener.accept().await?;
        info!("Connect {addr:?}");
        let hdl = register_interface(STACK.base(), socket).unwrap();

        tokio::task::spawn(async move {
            let res = hdl.run().await;
            warn!("END: {res:?}");
        });
    }
}

async fn ping_all() {
    let mut ival = interval(Duration::from_secs(3));
    let mut ctr = 0u32;
    loop {
        ival.tick().await;
        let nets = STACK.with_interface_manager(|im| im.get_nets());
        info!("Nets to ping: {nets:?}");
        for net in nets {
            let pg = ctr;
            ctr = ctr.wrapping_add(1);
            let rr = STACK.req_resp::<ErgotPingEndpoint>(
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

async fn yeet_listener(id: u8) {
    let subber = STACK.std_bounded_topic_receiver::<YeetTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();

    loop {
        let msg = hdl.recv().await;
        info!("Listener id:{id} got {msg:?}");
    }
}
