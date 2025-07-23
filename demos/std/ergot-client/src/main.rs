use ergot::{
    interface_manager::{
        interface_impls::std_tcp::StdTcpInterface,
        profiles::direct_edge::{DirectEdge, std_tcp::register_interface},
        utils::{cobs_stream, std::new_std_queue},
    },
    net_stack::ArcNetStack,
    topic,
    well_known::ErgotPingEndpoint,
};
use log::{info, warn};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::net::TcpStream;

use std::{io, pin::pin, time::Duration};

topic!(YeetTopic, u64, "topic/yeet");

// Client
type Stack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<StdTcpInterface>>;

#[tokio::main]
async fn main() -> io::Result<()> {
    let queue = new_std_queue(4096);
    let stack: Stack = Stack::new_with_profile(DirectEdge::new_target(
        cobs_stream::Sink::new_from_handle(queue.clone(), 1024),
    ));

    env_logger::init();
    let socket = TcpStream::connect("127.0.0.1:2025").await.unwrap();

    tokio::task::spawn(pingserver(stack.clone()));
    tokio::task::spawn(yeeter(stack.clone()));
    for i in 1..4 {
        tokio::task::spawn(yeet_listener(stack.clone(), i));
    }

    let hdl = register_interface(stack.base(), socket, queue.clone()).unwrap();
    tokio::task::spawn(async move {
        hdl.run().await.unwrap();
    });
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn pingserver(stack: Stack) {
    let server = stack.std_bounded_endpoint_server::<ErgotPingEndpoint>(16, None);
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

async fn yeeter(stack: Stack) {
    let mut ctr = 0;
    tokio::time::sleep(Duration::from_secs(3)).await;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        warn!("Sending broadcast message");
        stack
            .broadcast_topic::<YeetTopic>(&ctr, None)
            .await
            .unwrap();
        ctr += 1;
    }
}

async fn yeet_listener(stack: Stack, id: u8) {
    let subber = stack.std_bounded_topic_receiver::<YeetTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();

    loop {
        let msg = hdl.recv().await;
        info!("Listener id:{id} got {msg:?}");
    }
}
