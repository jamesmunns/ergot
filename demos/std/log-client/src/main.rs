use ergot::{
    fmt,
    toolkits::tokio_tcp::{EdgeStack, new_std_queue, new_target_stack, register_edge_interface},
};
use tokio::net::TcpStream;

use std::{io, time::Duration};

#[tokio::main]
async fn main() -> io::Result<()> {
    let queue = new_std_queue(4096);
    let stack: EdgeStack = new_target_stack(&queue, 1024);

    env_logger::init();
    let socket = TcpStream::connect("127.0.0.1:2025").await.unwrap();

    register_edge_interface(&stack, socket, &queue)
        .await
        .unwrap();

    let mut ctr = 0usize;
    let msg = "world";

    loop {
        stack.info_fmt(fmt!("Hello, {msg} - {ctr}"));
        ctr += 1;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
