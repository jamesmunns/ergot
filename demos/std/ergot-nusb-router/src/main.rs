use ergot::{
    Address, NetStack,
    interface_manager::impls::nusb_0_1_router::{
        NusbManager, find_new_devices, register_interface,
    },
    topic,
    well_known::ErgotPingEndpoint,
};
use log::{info, warn};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
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
static STACK: NetStack<CriticalSectionRawMutex, NusbManager> = NetStack::new();
topic!(YeetTopic, u64, "topic/yeet");

#[tokio::main]
async fn main() -> io::Result<()> {
    env_logger::init();

    tokio::task::spawn(ping_all());

    for i in 1..4 {
        tokio::task::spawn(yeet_listener(i));
    }

    let mut seen = HashSet::new();

    loop {
        let devices = find_new_devices(&HashSet::new()).await;

        for dev in devices {
            let info = dev.info.clone();
            info!("Found {info:?}, registering");
            let hdl = register_interface(STACK.base(), dev, MTU, OUT_BUFFER_SIZE).unwrap();
            seen.insert(info);
            tokio::task::spawn(async move {
                let res = hdl.run().await;
                warn!("END: {res:?}");
            });
        }

        sleep(Duration::from_secs(3)).await;
    }
}

async fn ping_all() {
    let mut ival = interval(Duration::from_secs(3));
    let mut ctr = 0u32;
    // Attempt to remember the ping port
    let mut portmap: HashMap<u16, u8> = HashMap::new();

    loop {
        ival.tick().await;
        let nets = STACK.with_interface_manager(|im| im.get_nets());
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
            let rr = STACK.req_resp_full::<ErgotPingEndpoint>(addr, &pg, None);
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

async fn yeet_listener(id: u8) {
    let subber = STACK.std_bounded_topic_receiver::<YeetTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();

    loop {
        let msg = hdl.recv().await;
        info!("Listener id:{id} got {msg:?}");
    }
}
