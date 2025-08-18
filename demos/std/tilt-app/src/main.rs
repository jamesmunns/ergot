use ergot::{
    Address,
    toolkits::nusb_v0_1::{RouterStack, find_new_devices, register_router_interface},
    topic,
    well_known::{ErgotFmtRxOwnedTopic, ErgotPingEndpoint},
};
use log::{info, warn};
use shared_icd::tilt::DataTopic;
use tokio::time::sleep;
use tokio::time::{interval, timeout};

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{self, Write},
    pin::pin,
    process::exit,
    time::{Duration, Instant},
};

const MTU: u16 = 1024;
const OUT_BUFFER_SIZE: usize = 4096;

// Server
topic!(YeetTopic, u64, "topic/yeet");

#[tokio::main]
async fn main() -> io::Result<()> {
    env_logger::init();
    let stack: RouterStack = RouterStack::new();

    tokio::task::spawn(ping_all(stack.clone()));
    tokio::task::spawn(log_collect(stack.clone()));
    tokio::task::spawn(data_collect(stack.clone()));

    for i in 1..4 {
        tokio::task::spawn(yeet_listener(stack.clone(), i));
    }

    let mut seen = HashSet::new();

    loop {
        let devices = find_new_devices(&HashSet::new()).await;

        for dev in devices {
            let info = dev.info.clone();
            info!("Found {info:?}, registering");
            let _hdl = register_router_interface(&stack, dev, MTU, OUT_BUFFER_SIZE)
                .await
                .unwrap();
            seen.insert(info);
        }

        sleep(Duration::from_secs(3)).await;
    }
}

async fn ping_all(stack: RouterStack) {
    let mut ival = interval(Duration::from_secs(3));
    let mut ctr = 0u32;
    // Attempt to remember the ping port
    let mut portmap: HashMap<u16, u8> = HashMap::new();

    loop {
        ival.tick().await;
        let nets = stack.manage_profile(|im| im.get_nets());
        info!("Nets to ping: {nets:?}");
        for net in nets {
            let pg = ctr;
            ctr = ctr.wrapping_add(1);

            let addr = if let Some(port) = portmap.get(&net) {
                Address {
                    network_id: net,
                    node_id: 2,
                    port_id: *port,
                }
            } else {
                Address {
                    network_id: net,
                    node_id: 2,
                    port_id: 0,
                }
            };

            let start = Instant::now();
            let rr = stack.req_resp_full::<ErgotPingEndpoint>(addr, &pg, None);
            let fut = timeout(Duration::from_millis(100), rr);
            let res = fut.await;
            let elapsed = start.elapsed();
            warn!("ping {net}.2 w/ {pg}: {res:?}, took: {elapsed:?}");
            if let Ok(Ok(msg)) = res {
                assert_eq!(msg.t, pg);
                portmap.insert(net, msg.hdr.src.port_id);
            } else {
                portmap.remove(&net);
            }
        }
    }
}

async fn yeet_listener(stack: RouterStack, id: u8) {
    let subber = stack.std_bounded_topic_receiver::<YeetTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();

    loop {
        let msg = hdl.recv().await;
        info!("Listener id:{id} got {msg:?}");
    }
}

async fn log_collect(stack: RouterStack) {
    let subber = stack.std_bounded_topic_receiver::<ErgotFmtRxOwnedTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();

    loop {
        let msg = hdl.recv().await;
        println!(
            "({}.{}:{}) {:?}: {}",
            msg.hdr.src.network_id,
            msg.hdr.src.node_id,
            msg.hdr.src.port_id,
            msg.t.level,
            msg.t.inner,
        );
    }
}

// async fn data_collect(stack: RouterStack) {
//     let subber = stack.std_bounded_topic_receiver::<DataTopic>(64, None);
//     let subber = pin!(subber);
//     let mut hdl = subber.subscribe();

//     loop {
//         let msg = hdl.recv().await;
//         println!(
//             "({}.{}:{})::({}):",
//             msg.hdr.src.network_id,
//             msg.hdr.src.node_id,
//             msg.hdr.src.port_id,
//             msg.t.time,
//         );
//         for dat in msg.t.inner {
//             println!("  {dat:?}");
//         }
//     }
// }

async fn data_collect(stack: RouterStack) {
    let subber = stack.std_bounded_topic_receiver::<DataTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();
    let mut ctr = 0;
    let mut last = Instant::now();
    let mut got_data = false;
    let mut buf = Vec::with_capacity(6666 * 10);
    let start = Instant::now();

    loop {
        let Ok(msg) = timeout(Duration::from_secs(3), hdl.recv()).await else {
            if got_data {
                break;
            } else {
                continue;
            }
        };
        ctr += 4;
        let dtime = start.elapsed().as_micros() as u64;
        let elapsed = last.elapsed();
        if elapsed >= Duration::from_secs(1) {
            got_data |= ctr >= 1000;
            println!("{ctr} RPS, most recent record:");
            println!("  {:?}", msg.t.inner[3]);
            ctr = 0;
            // last += Duration::from_secs(1);
            last = Instant::now();
        }
        buf.push((dtime, msg));
    }

    let mut f = File::create_new("./data.csv").unwrap();
    _ = writeln!(
        &mut f,
        "DTIMEus, ETIMEus, FTIME, ACCL_X, ACCL_Y, ACCL_Z, GYRO_P, GYRO_R, GYRO_Y"
    );
    for (dtime, d) in buf {
        for r in d.t.inner {
            _ = writeln!(
                &mut f,
                "{}, {}, {}, {}, {}, {}, {}, {}, {}",
                dtime,
                d.t.mcu_timestamp,
                r.imu_timestamp,
                r.accl_x,
                r.accl_y,
                r.accl_z,
                r.gyro_p,
                r.gyro_r,
                r.gyro_y,
            );
        }
    }
    _ = f.flush();
    drop(f);
    exit(0);
}
