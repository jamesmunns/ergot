use ergot::{
    Address,
    toolkits::nusb_v0_1::{RouterStack, find_new_devices, register_router_interface},
    topic,
    well_known::{ErgotFmtRxOwnedTopic, ErgotPingEndpoint},
};
use log::{info, warn};
use shared_icd::tilt::{Data, DataTopic, PwmSetEndpoint};
use tokio::time::sleep;
use tokio::time::{interval, timeout};

use std::{
    collections::{HashMap, HashSet},
    io,
    pin::pin,
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
    tokio::task::spawn(fake_control_loop(stack.clone()));

    for i in 1..4 {
        tokio::task::spawn(yeet_listener(stack.clone(), i));
    }

    let mut seen = HashSet::new();

    loop {
        let devices = find_new_devices(&HashSet::new()).await;

        for dev in devices {
            let info = dev.info.clone();
            info!("Found {:?}, registering", info);
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
        info!("Nets to ping: {:?}", nets);
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
            let rr = stack
                .endpoints()
                .request_full::<ErgotPingEndpoint>(addr, &pg, None);
            let fut = timeout(Duration::from_millis(100), rr);
            let res = fut.await;
            let elapsed = start.elapsed();
            warn!("ping {}.2 w/ {}: {:?}, took: {:?}", net, pg, res, elapsed);
            if let Ok(Ok(msg)) = res {
                assert_eq!(msg.t, pg);
                portmap.insert(net, msg.hdr.src.port_id);
            } else {
                portmap.remove(&net);
            }
        }
    }
}

/// This is a very silly control loop that simulates a "position holding"
/// quadrotor loop, where the device attempts to hold steady.
///
/// Right now, this is basically a proportional-only loop with some light
/// filtering. I don't expect this to actually work.
async fn fake_control_loop(stack: RouterStack) {
    let subber = stack.topics().heap_bounded_receiver::<DataTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();
    let mut last_update = Instant::now();
    let mut pitch = 0.0;
    let mut roll = 0.0;
    let mut yaw = 0.0;

    loop {
        let msg = hdl.recv().await;
        for data in msg.t.inner {
            let Data {
                gyro_p,
                gyro_r,
                gyro_y,
                ..
            } = data;
            let gyro_p = gyro_p as f32 / (i16::MAX as f32);
            let gyro_r = gyro_r as f32 / (i16::MAX as f32);
            let gyro_y = gyro_y as f32 / (i16::MAX as f32);
            pitch = (pitch * 0.99) + (gyro_p * 0.01);
            roll = (roll * 0.99) + (gyro_r * 0.01);
            yaw = (yaw * 0.99) + (gyro_y * 0.01);
        }
        pitch = pitch.clamp(-1.0, 1.0);
        roll = roll.clamp(-1.0, 1.0);
        yaw = yaw.clamp(-1.0, 1.0);

        if last_update.elapsed() > Duration::from_millis(5) {
            let mut l1 = 0.0f32; // front right
            let mut l2 = 0.0f32; // back right
            let mut l3 = 0.0f32; // back left
            let mut l4 = 0.0f32; // front left

            // Pitch is positive - make front "props" go up
            if pitch >= 0.0 {
                l1 += pitch / 3.0;
                l4 += pitch / 3.0;
                l2 -= pitch / 3.0;
                l3 -= pitch / 3.0;
            } else {
                l2 += pitch.abs() / 3.0;
                l3 += pitch.abs() / 3.0;
                l1 -= pitch.abs() / 3.0;
                l4 -= pitch.abs() / 3.0;
            }

            // Roll is positive (left roll), make left
            // "props" go positive
            if roll >= 0.0 {
                l3 += roll / 3.0;
                l4 += roll / 3.0;
                l1 -= roll / 3.0;
                l2 -= roll / 3.0;
            } else {
                l1 += roll.abs() / 3.0;
                l2 += roll.abs() / 3.0;
                l3 -= roll.abs() / 3.0;
                l4 -= roll.abs() / 3.0;
            }

            // Yaw is positive, make 1/3 positive, and 2/4 negative
            if yaw >= 0.0 {
                l1 += yaw / 3.0;
                l3 += yaw / 3.0;
                l2 -= yaw / 3.0;
                l4 -= yaw / 3.0;
            } else {
                l2 += yaw.abs() / 3.0;
                l4 += yaw.abs() / 3.0;
                l1 -= yaw.abs() / 3.0;
                l3 -= yaw.abs() / 3.0;
            }

            l1 = l1.clamp(0.0, 1.0);
            l2 = l2.clamp(0.0, 1.0);
            l3 = l3.clamp(0.0, 1.0);
            l4 = l4.clamp(0.0, 1.0);

            let todo = [
                (l1, "channel1"),
                (l2, "channel2"),
                (l3, "channel3"),
                (l4, "channel4"),
            ];

            for (val, name) in todo {
                _ = timeout(
                    Duration::from_millis(10),
                    stack.endpoints().request::<PwmSetEndpoint>(
                        Address {
                            network_id: 1,
                            node_id: 2,
                            port_id: 0,
                        },
                        &val,
                        Some(name),
                    ),
                )
                .await;
            }
            last_update = Instant::now();
        }
    }
}

async fn yeet_listener(stack: RouterStack, id: u8) {
    let subber = stack.topics().heap_bounded_receiver::<YeetTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();

    loop {
        let msg = hdl.recv().await;
        info!("Listener id:{} got {:?}", id, msg);
    }
}

async fn log_collect(stack: RouterStack) {
    let subber = stack
        .topics()
        .heap_bounded_receiver::<ErgotFmtRxOwnedTopic>(64, None);
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
