use ergot::{
    toolkits::tokio_udp::{EdgeStack, new_std_queue, new_target_stack, register_edge_interface},
    topic,
    well_known::DeviceInfo,
};
use log::{debug, info};
use tokio::{net::UdpSocket, select, time, time::sleep};

use ergot::interface_manager::profiles::direct_edge::tokio_udp::InterfaceKind;
use ergot::logging::log_v0_4::LogSink;
use std::convert::TryInto;
use std::{io, pin::pin, time::Duration};

topic!(YeetTopic, u64, "topic/yeet");

#[tokio::main]
async fn main() -> io::Result<()> {
    //env_logger::init();

    let queue = new_std_queue(4096);
    let stack: EdgeStack = new_target_stack(&queue, 1024);

    let logger = Box::new(LogSink::new(stack.clone()));
    let logger = Box::leak(logger);
    logger.register_static(log::LevelFilter::Info);

    let udp_socket = UdpSocket::bind("127.0.0.1:8001").await.unwrap();
    let remote_addr = "127.0.0.1:8000";

    udp_socket.connect(remote_addr).await?;

    let port = udp_socket.local_addr().unwrap().port();

    tokio::task::spawn(basic_services(stack.clone(), port));
    tokio::task::spawn(yeeter(stack.clone()));
    tokio::task::spawn(yeet_listener(stack.clone(), 0));

    register_edge_interface(&stack, udp_socket, &queue, InterfaceKind::Target)
        .await
        .unwrap();

    loop {
        println!("Waiting for messages...");
        sleep(Duration::from_secs(1)).await;
    }
}

async fn basic_services(stack: EdgeStack, port: u16) {
    let info = DeviceInfo {
        name: Some("Ergot client".try_into().unwrap()),
        description: Some("An Ergot Client Device".try_into().unwrap()),
        unique_id: port.into(),
    };
    let do_pings = stack.services().ping_handler::<4>();
    let do_info = stack.services().device_info_handler::<4>(&info);

    select! {
        _ = do_pings => {}
        _ = do_info => {}
    }
}

async fn yeeter(stack: EdgeStack) {
    let mut ctr = 0;
    tokio::time::sleep(Duration::from_secs(1)).await;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        info!("Sending broadcast message from target");
        stack.topics().broadcast::<YeetTopic>(&ctr, None).unwrap();
        ctr += 1;
    }
}

async fn yeet_listener(stack: EdgeStack, id: u8) {
    let subber = stack.topics().heap_bounded_receiver::<YeetTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();

    let mut packets_this_interval = 0;
    let interval = Duration::from_secs(1);
    let mut ticker = time::interval(interval);
    loop {
        select! {
            _ = ticker.tick() => {
                info!("packet rate: {}/{:?}", packets_this_interval, interval);
                packets_this_interval = 0;
            }
            msg = hdl.recv() => {
                packets_this_interval += 1;
                debug!("{}: Listener id:{} got {}", msg.hdr, id, msg.t);
            }
        }
    }
}
