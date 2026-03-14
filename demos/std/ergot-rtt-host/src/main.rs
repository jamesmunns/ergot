//! Host-side ergot router using RTT transport via probe-rs
//!
//! This demo shows how to connect to an embedded device via RTT (Real-Time Transfer)
//! using a debug probe (ST-Link, J-Link, etc.) and communicate using the ergot protocol.
//!
//! Optionally decodes defmt frames from a separate RTT channel or from
//! the ergot network (ergot/.well-known/defmt topic).
//!
//! Usage:
//!   ergot-rtt-host --chip STM32G431KBTx --up-channel 1 --down-channel 0
//!   ergot-rtt-host --chip STM32G431KBTx --elf firmware.elf --defmt-channel 0
//!   ergot-rtt-host --chip STM32G431KBTx --elf firmware.elf --defmt-via-ergot

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use cobs_acc::{CobsAccumulator, FeedResult};
use ergot::{
    Address,
    interface_manager::{
        InterfaceState,
        interface_impls::tokio_serial_cobs::TokioSerialInterface,
        profiles::direct_edge::{DirectEdge, process_frame as ergot_edge_process_frame},
        utils::{cobs_stream::Sink as ErgotSink, std::new_std_queue},
    },
    net_stack::ArcNetStack,
    well_known::ErgotPingEndpoint,
};
use log::{error, info, warn};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use probe_rs::{
    Permissions,
    probe::list::Lister,
    rtt::{Rtt, ScanRegion},
};
use std::time::Duration;
use tokio::time::{interval, timeout};

/// Host-side ergot router using RTT transport
#[derive(Parser, Debug)]
#[command(name = "ergot-rtt-host")]
#[command(about = "Connect to embedded devices via RTT and route ergot messages")]
struct Args {
    /// Target chip name (e.g., STM32G431KBTx, nRF52840_xxAA)
    #[arg(short, long)]
    chip: String,

    /// Probe selector in format VID:PID or VID:PID:SERIAL
    #[arg(short, long)]
    probe: Option<String>,

    /// RTT up channel index for ergot data (device -> host)
    #[arg(long, default_value = "1")]
    up_channel: usize,

    /// RTT down channel index for ergot data (host -> device)
    #[arg(long, default_value = "0")]
    down_channel: usize,

    /// Path to device ELF file for defmt decoding
    #[arg(long)]
    elf: Option<PathBuf>,

    /// RTT up channel index for defmt data (default: 0, disabled if no --elf)
    #[arg(long, default_value = "0")]
    defmt_channel: usize,

    /// Receive defmt frames via ergot network topic instead of raw RTT channel
    #[arg(long)]
    defmt_via_ergot: bool,
}

const ERGOT_MTU: u16 = 512;

fn connect_probe(probe_selector: Option<&str>, chip: &str) -> Result<probe_rs::Session> {
    info!("Connecting to chip: {}", chip);

    let lister = Lister::new();

    let probe = if let Some(selector) = probe_selector {
        info!("Using probe selector: {}", selector);
        let probes = lister.list_all();

        let parts: Vec<&str> = selector.split(':').collect();
        let (vid, pid, serial) = match parts.len() {
            2 => {
                let vid = u16::from_str_radix(parts[0], 16)
                    .with_context(|| format!("Invalid VID: {}", parts[0]))?;
                let pid = u16::from_str_radix(parts[1], 16)
                    .with_context(|| format!("Invalid PID: {}", parts[1]))?;
                (vid, pid, None)
            }
            3 => {
                let vid = u16::from_str_radix(parts[0], 16)
                    .with_context(|| format!("Invalid VID: {}", parts[0]))?;
                let pid = u16::from_str_radix(parts[1], 16)
                    .with_context(|| format!("Invalid PID: {}", parts[1]))?;
                (vid, pid, Some(parts[2]))
            }
            _ => {
                return Err(anyhow!(
                    "Invalid probe selector. Use VID:PID or VID:PID:SERIAL"
                ));
            }
        };

        let probe_info = probes
            .into_iter()
            .find(|p| {
                p.vendor_id == vid
                    && p.product_id == pid
                    && (serial.is_none() || p.serial_number.as_deref() == serial)
            })
            .ok_or_else(|| anyhow!("No matching probe found for: {}", selector))?;

        probe_info.open().context("Failed to open probe")?
    } else {
        let probes = lister.list_all();
        if probes.is_empty() {
            return Err(anyhow!("No debug probes found"));
        }
        info!("Auto-selecting first probe: {:?}", probes[0]);
        probes[0].open().context("Failed to open probe")?
    };

    let session = probe
        .attach(chip, Permissions::default())
        .context("Failed to attach to target")?;

    info!("Attached to target");
    Ok(session)
}

type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<TokioSerialInterface>>;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    let mut session = connect_probe(args.probe.as_deref(), &args.chip)?;

    // Load defmt table if ELF is provided
    let has_rtt_defmt = args.elf.is_some() && !args.defmt_via_ergot;
    let has_ergot_defmt = args.elf.is_some() && args.defmt_via_ergot;

    // Create ergot edge stack
    let queue = new_std_queue(4096);
    let stack: EdgeStack = ArcNetStack::new_with_profile(DirectEdge::new_controller(
        ErgotSink::new_from_handle(queue.clone(), ERGOT_MTU),
        InterfaceState::Active {
            net_id: 1,
            node_id: 1,
        },
    ));

    info!("Ergot RTT host started");

    // Single blocking I/O thread for all RTT operations.
    // probe-rs RTT is synchronous and holds Core/Rtt references that aren't Send,
    // so we run all RTT I/O in a dedicated thread and communicate via channels.
    let tx_queue: &'static _ = Box::leak(Box::new(queue.clone()));
    let ergot_up_ch = args.up_channel;
    let ergot_down_ch = args.down_channel;
    let defmt_ch = args.defmt_channel;

    let (ergot_rx_tx, mut ergot_rx_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let (ergot_tx_tx, ergot_tx_rx) = std::sync::mpsc::channel::<Vec<u8>>();

    // defmt frames sent from the RTT thread to tokio for decoding
    let (defmt_data_tx, mut defmt_data_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);

    // RTT I/O thread - owns Session, Core and Rtt, never re-attaches
    std::thread::spawn(move || {
        let mut core = session.core(0).expect("Failed to get core 0");
        let mut rtt =
            Rtt::attach_region(&mut core, &ScanRegion::Ram).expect("Failed to attach to RTT");

        info!("RTT attached successfully");
        for (idx, channel) in rtt.up_channels().iter().enumerate() {
            info!(
                "RTT up channel {}: {} (size: {})",
                idx,
                channel.name().unwrap_or("unnamed"),
                channel.buffer_size()
            );
        }
        for (idx, channel) in rtt.down_channels().iter().enumerate() {
            info!(
                "RTT down channel {}: {} (size: {})",
                idx,
                channel.name().unwrap_or("unnamed"),
                channel.buffer_size()
            );
        }

        let mut ergot_rx_buf = [0u8; 2048];
        let mut defmt_buf = [0u8; 4096];

        loop {
            let mut did_work = false;

            // 1. Read ergot data from device
            if let Some(channel) = rtt.up_channel(ergot_up_ch) {
                match channel.read(&mut core, &mut ergot_rx_buf) {
                    Ok(n) if n > 0 => {
                        did_work = true;
                        let _ = ergot_rx_tx.blocking_send(ergot_rx_buf[..n].to_vec());
                    }
                    Ok(_) => {}
                    Err(e) => error!("RTT ergot read error: {}", e),
                }
            }

            // 2. Write ergot data to device
            while let Ok(data) = ergot_tx_rx.try_recv() {
                if let Some(channel) = rtt.down_channel(ergot_down_ch) {
                    if let Err(e) = channel.write(&mut core, &data) {
                        error!("RTT ergot write error: {}", e);
                    }
                    did_work = true;
                }
            }

            // 3. Read defmt data from device (only when using raw RTT defmt)
            if has_rtt_defmt {
                match rtt.up_channel(defmt_ch) {
                    Some(channel) => match channel.read(&mut core, &mut defmt_buf) {
                        Ok(n) if n > 0 => {
                            did_work = true;
                            info!("defmt: read {} bytes", n);
                            let _ = defmt_data_tx.blocking_send(defmt_buf[..n].to_vec());
                        }
                        Ok(_) => {}
                        Err(e) => error!("RTT defmt read error: {}", e),
                    },
                    None => {
                        error!("defmt channel {} not available", defmt_ch);
                    }
                }
            }

            if !did_work {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    });

    // Ergot RX processing task
    tokio::spawn({
        let stack = stack.clone();
        async move {
            let mut cobs_acc = CobsAccumulator::new_boxslice((ERGOT_MTU as usize) + 64);
            let mut net_id = Some(1u16);

            while let Some(data) = ergot_rx_rx.recv().await {
                let mut data = data;
                let mut window = &mut data[..];
                while !window.is_empty() {
                    window = match cobs_acc.feed_raw(window) {
                        FeedResult::Consumed => break,
                        FeedResult::OverFull(rem) | FeedResult::DecodeError(rem) => rem,
                        FeedResult::Success { data, remaining }
                        | FeedResult::SuccessInput { data, remaining } => {
                            ergot_edge_process_frame(&mut net_id, data, &stack, ());
                            remaining
                        }
                    };
                }
            }
        }
    });

    // Ergot TX forwarding task
    tokio::spawn(async move {
        let tx_consumer = tx_queue.stream_consumer();
        loop {
            let frame = tx_consumer.wait_read().await;
            let len = frame.len();
            if len > 0 {
                let data = frame[..len].to_vec();
                frame.release(len);
                let _ = ergot_tx_tx.send(data);
            } else {
                frame.release(len);
            }
        }
    });

    // defmt decoder task
    if let Some(elf_path) = &args.elf {
        let elf_bytes = std::fs::read(elf_path)?;
        let table = defmt_decoder::Table::parse(&elf_bytes)
            .context("Failed to parse defmt table")?
            .ok_or_else(|| anyhow!("No .defmt section"))?;

        tokio::spawn(async move {
            let mut stream_decoder = table.new_stream_decoder();

            while let Some(data) = defmt_data_rx.recv().await {
                stream_decoder.received(&data);
                loop {
                    match stream_decoder.decode() {
                        Ok(frame) => {
                            println!("[defmt] {}", frame.display(true));
                        }
                        Err(defmt_decoder::DecodeError::UnexpectedEof) => break,
                        Err(defmt_decoder::DecodeError::Malformed) => {
                            warn!("Malformed defmt frame");
                            break;
                        }
                    }
                }
            }
        });
    }

    // defmt-via-ergot decoder task: subscribe to ergot/.well-known/defmt topic
    if has_ergot_defmt {
        let elf_bytes = std::fs::read(args.elf.as_ref().unwrap())?;
        let table = defmt_decoder::Table::parse(&elf_bytes)
            .context("Failed to parse defmt table")?
            .ok_or_else(|| anyhow!("No .defmt section"))?;

        info!("defmt-via-ergot decoder enabled");

        tokio::spawn({
            let stack = stack.clone();
            async move {
                use ergot::well_known::ErgotDefmtRxOwnedTopic;
                use std::pin::pin;

                let sub = stack
                    .topics()
                    .heap_bounded_receiver::<ErgotDefmtRxOwnedTopic>(64, None);
                let sub = pin!(sub);
                let mut hdl = sub.subscribe();
                let mut stream_decoder = table.new_stream_decoder();

                info!("Waiting for defmt frames on ergot topic...");
                loop {
                    let msg = hdl.recv().await;
                    info!(
                        "Got defmt frame via ergot: {} bytes from {}.{}",
                        msg.t.frame.len(),
                        msg.hdr.src.network_id,
                        msg.hdr.src.node_id
                    );
                    stream_decoder.received(&msg.t.frame);
                    loop {
                        match stream_decoder.decode() {
                            Ok(frame) => {
                                println!("[defmt-ergot] {}", frame.display(true));
                            }
                            Err(defmt_decoder::DecodeError::UnexpectedEof) => break,
                            Err(defmt_decoder::DecodeError::Malformed) => {
                                warn!("Malformed defmt frame from ergot");
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    // Spawn log handler
    tokio::spawn({
        let stack = stack.clone();
        async move { stack.services().log_handler(64).await }
    });

    // Spawn ping task
    tokio::spawn({
        let stack = stack.clone();
        async move {
            let mut ival = interval(Duration::from_secs(5));
            let mut counter = 0u32;

            tokio::time::sleep(Duration::from_secs(2)).await;

            loop {
                ival.tick().await;
                let ping_val = counter;
                counter = counter.wrapping_add(1);

                let device_addr = Address {
                    network_id: 1,
                    node_id: 2,
                    port_id: 0,
                };

                let result = timeout(
                    Duration::from_millis(500),
                    stack
                        .endpoints()
                        .request::<ErgotPingEndpoint>(device_addr, &ping_val, None),
                )
                .await;

                match result {
                    Ok(Ok(response)) if response == ping_val => {
                        info!("Ping OK: {}", ping_val);
                    }
                    Ok(Ok(response)) => {
                        warn!("Ping mismatch: got {} expected {}", response, ping_val);
                    }
                    Ok(Err(e)) => {
                        warn!("Ping error: {:?}", e);
                    }
                    Err(_) => {
                        warn!("Ping timeout");
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
