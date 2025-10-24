use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use log::{info, warn};
use stream_plotting::{StreamPlottingApp, send_simulated_data};

const MTU: u16 = 1024;
const OUT_BUFFER_SIZE: usize = 4096;

use ergot::{
    Address,
    toolkits::nusb_v0_1::{RouterStack, find_new_devices, register_router_interface},
    well_known::ErgotPingEndpoint,
};
use tokio::time::{interval, sleep, timeout};

#[tokio::main]
async fn main() {
    env_logger::init();
    let stack: RouterStack = RouterStack::new();

    tokio::task::spawn(ping_all(stack.clone()));
    tokio::task::spawn(manage_connections(stack.clone()));
    tokio::task::spawn(send_simulated_data(stack.clone()));

    let mut native_options = eframe::NativeOptions::default();
    native_options.viewport.min_inner_size = Some(eframe::egui::Vec2 { x: 900., y: 600. }); // empirical
    eframe::run_native(
        "Ergot Plotting Demo",
        native_options,
        Box::new(|cc| Ok(Box::new(StreamPlottingApp::new(cc, stack.clone())))),
    )
    .unwrap();
}

async fn manage_connections(stack: RouterStack) {
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
