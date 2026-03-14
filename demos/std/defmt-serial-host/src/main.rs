//! Serial ergot router with defmt decoding
//!
//! Connects to an embedded device via serial port, routes ergot messages,
//! and decodes defmt frames received on the ergot/.well-known/defmt topic.
//!
//! Usage:
//!   defmt-serial-host --port /dev/ttyACM0 --elf path/to/firmware.elf

use std::{path::PathBuf, pin::pin, time::Duration};

use anyhow::{Context, Result};
use clap::Parser;
use ergot::{
    Address,
    toolkits::tokio_serial_v5::{RouterStack, register_router_interface},
    well_known::{ErgotDefmtRxOwnedTopic, ErgotPingEndpoint},
};
use log::{error, info, warn};
use tokio::time::{interval, timeout};

#[derive(Parser, Debug)]
#[command(name = "defmt-serial-host")]
#[command(about = "Serial ergot router with defmt frame decoding")]
struct Args {
    /// Serial port path
    #[arg(short, long)]
    port: String,

    /// Baud rate
    #[arg(short, long, default_value = "115200")]
    baud: u32,

    /// Path to device ELF file for defmt decoding
    #[arg(short, long)]
    elf: PathBuf,
}

const MAX_ERGOT_PACKET_SIZE: u16 = 1024;
const TX_BUFFER_SIZE: usize = 4096;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    // Load defmt table
    let elf_bytes = std::fs::read(&args.elf)
        .with_context(|| format!("Failed to read ELF: {}", args.elf.display()))?;
    let table = defmt_decoder::Table::parse(&elf_bytes)
        .context("Failed to parse defmt table from ELF")?
        .ok_or_else(|| anyhow::anyhow!("No .defmt section in ELF file"))?;
    info!("defmt table loaded from {}", args.elf.display());

    let stack: RouterStack = RouterStack::new();

    // Connect serial
    info!("Connecting to {} at {} baud", args.port, args.baud);
    register_router_interface(
        &stack,
        &args.port,
        args.baud,
        MAX_ERGOT_PACKET_SIZE,
        TX_BUFFER_SIZE,
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to register serial interface: {:?}", e))?;
    info!("Serial interface registered");

    // Ping handler
    tokio::spawn(stack.services().ping_handler::<4>());

    // Log handler
    tokio::spawn({
        let stack = stack.clone();
        async move { stack.services().log_handler(64).await }
    });

    // Ping task
    tokio::spawn({
        let stack = stack.clone();
        async move {
            let mut ival = interval(Duration::from_secs(5));
            let mut counter = 0u32;

            loop {
                ival.tick().await;
                let nets = stack.manage_profile(|im| im.get_nets());

                for net in nets {
                    let ping_val = counter;
                    counter = counter.wrapping_add(1);

                    let result = timeout(
                        Duration::from_millis(500),
                        stack.endpoints().request::<ErgotPingEndpoint>(
                            Address {
                                network_id: net,
                                node_id: 2,
                                port_id: 0,
                            },
                            &ping_val,
                            None,
                        ),
                    )
                    .await;

                    match result {
                        Ok(Ok(response)) if response == ping_val => {
                            info!("Ping {}.2 OK: {}", net, ping_val);
                        }
                        Ok(Ok(response)) => {
                            warn!(
                                "Ping {}.2 mismatch: got {} expected {}",
                                net, response, ping_val
                            );
                        }
                        Ok(Err(e)) => {
                            warn!("Ping {}.2 error: {:?}", net, e);
                        }
                        Err(_) => {
                            warn!("Ping {}.2 timeout", net);
                        }
                    }
                }
            }
        }
    });

    // defmt decoder task
    tokio::spawn({
        let stack = stack.clone();
        async move {
            let sub = stack
                .topics()
                .heap_bounded_receiver::<ErgotDefmtRxOwnedTopic>(64, None);
            let sub = pin!(sub);
            let mut hdl = sub.subscribe();
            let mut stream_decoder = table.new_stream_decoder();

            info!("Waiting for defmt frames on ergot topic...");
            loop {
                let msg = hdl.recv().await;
                stream_decoder.received(&msg.t.frame);
                loop {
                    match stream_decoder.decode() {
                        Ok(frame) => {
                            println!(
                                "[defmt {}.{}] {}",
                                msg.hdr.src.network_id,
                                msg.hdr.src.node_id,
                                frame.display(true)
                            );
                        }
                        Err(defmt_decoder::DecodeError::UnexpectedEof) => break,
                        Err(defmt_decoder::DecodeError::Malformed) => {
                            error!("Malformed defmt frame");
                            break;
                        }
                    }
                }
            }
        }
    });

    info!("Press Ctrl+C to exit");
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}
