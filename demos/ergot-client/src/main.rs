use ergot::{
    NetStack,
    interface_manager::std_tcp_client::{StdTcpClientIm, register_interface},
    topic,
    well_known::ErgotPingEndpoint,
};
use log::{info, warn};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::net::TcpStream;

use std::{io, pin::pin, time::Duration};

topic!(YeetTopic, u64, "topic/yeet");

// Client
static STACK: NetStack<CriticalSectionRawMutex, StdTcpClientIm> = NetStack::new();

#[tokio::main]
async fn main() -> io::Result<()> {
    env_logger::init();
    let socket = TcpStream::connect("127.0.0.1:2025").await.unwrap();

    tokio::task::spawn(pingserver());
    tokio::task::spawn(yeeter());
    for i in 1..4 {
        tokio::task::spawn(yeet_listener(i));
    }

    let hdl = register_interface(STACK.base(), socket).unwrap();
    tokio::task::spawn(async move {
        hdl.run().await.unwrap();
    });
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn pingserver() {
    let server = STACK.std_bounded_endpoint_server::<ErgotPingEndpoint>(16, None);
    let server = pin!(server);
    let mut server_hdl = server.attach();
    loop {
        server_hdl
            .serve_blocking(|req: &u32| {
                info!("Serving ping {req}");
                *req
            })
            .await
            .unwrap();
    }
}

async fn yeeter() {
    let mut ctr = 0;
    tokio::time::sleep(Duration::from_secs(3)).await;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        warn!("Sending broadcast message");
        STACK
            .broadcast_topic::<YeetTopic>(&ctr, None)
            .await
            .unwrap();
        ctr += 1;
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
