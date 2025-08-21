use ergot::{
    toolkits::tokio_tcp::{EdgeStack, new_std_queue, new_target_stack, register_edge_interface},
    topic,
};
use log::{info, warn};
use tokio::net::TcpStream;

use std::{io, pin::pin, time::Duration};

topic!(YeetTopic, u64, "topic/yeet");

#[tokio::main]
async fn main() -> io::Result<()> {
    let queue = new_std_queue(4096);
    let stack: EdgeStack = new_target_stack(&queue, 1024);

    env_logger::init();
    let socket = TcpStream::connect("127.0.0.1:2025").await.unwrap();

    tokio::task::spawn(pingserver(stack.clone()));
    tokio::task::spawn(yeeter(stack.clone()));
    for i in 1..4 {
        tokio::task::spawn(yeet_listener(stack.clone(), i));
    }

    register_edge_interface(&stack, socket, &queue)
        .await
        .unwrap();
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn pingserver(stack: EdgeStack) {
    stack.services().ping_handler::<4>().await;
}

async fn yeeter(stack: EdgeStack) {
    let mut ctr = 0;
    tokio::time::sleep(Duration::from_secs(3)).await;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        warn!("Sending broadcast message");
        stack.broadcast_topic::<YeetTopic>(&ctr, None).unwrap();
        ctr += 1;
    }
}

async fn yeet_listener(stack: EdgeStack, id: u8) {
    let subber = stack.std_bounded_topic_receiver::<YeetTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();

    loop {
        let msg = hdl.recv().await;
        info!("Listener id:{id} got {msg:?}");
    }
}
