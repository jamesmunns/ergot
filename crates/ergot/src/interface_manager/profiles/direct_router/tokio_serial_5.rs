//! A Tokio Serial based DirectRouter
//!
//! This implementation can be used to connect to a number of direct edge serial devices.

use crate::logging::{debug, error, info, warn};
use crate::{
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::tokio_serial_cobs::TokioSerialInterface,
        profiles::direct_router::{DirectRouter, process_frame},
        utils::{
            cobs_stream::Sink,
            std::{
                ReceiverError, StdQueue,
                acc::{CobsAccumulator, FeedResult},
                new_std_queue,
            },
        },
    },
    net_stack::NetStackHandle,
};
use bbq2::{prod_cons::stream::StreamConsumer, traits::bbqhdl::BbqHandle};
use cobs::max_encoding_overhead;
use maitake_sync::WaitQueue;
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf},
    select,
};
use tokio_serial_v5::{SerialPortBuilderExt, SerialStream};

#[derive(Debug, PartialEq)]
pub enum Error {
    OutOfNetIds,
    Serial(String),
}

struct TxWorker {
    net_id: u16,
    tx: WriteHalf<SerialStream>,
    rx: StreamConsumer<StdQueue>,
    closer: Arc<WaitQueue>,
}

struct RxWorker<N>
where
    N: NetStackHandle<Profile = DirectRouter<TokioSerialInterface>>,
    N: Send + 'static,
{
    interface_id: u64,
    net_id: u16,
    nsh: N,
    skt: ReadHalf<SerialStream>,
    closer: Arc<WaitQueue>,
    mtu: u16,
}

impl TxWorker {
    async fn run(mut self) {
        self.run_inner().await;
        warn!("Closing interface {}", self.net_id);
        self.closer.close();
    }

    async fn run_inner(&mut self) {
        info!("Started tx_worker for net_id {}", self.net_id);
        loop {
            let rxf = self.rx.wait_read();
            let clf = self.closer.wait();

            let frame = select! {
                r = rxf => r,
                _c = clf => {
                    break;
                }
            };

            let len = frame.len();
            debug!("sending pkt len:{} on net_id {}", len, self.net_id);
            let res = self.tx.write_all(&frame).await;
            frame.release(len);
            if let Err(e) = res {
                error!("Err: {:?}", e);
                break;
            }
        }
    }
}

impl<N> RxWorker<N>
where
    N: NetStackHandle<Profile = DirectRouter<TokioSerialInterface>>,
    N: Send + 'static,
{
    async fn run(mut self) {
        let close = self.closer.clone();

        // Wait for the receiver to encounter an error, or wait for
        // the transmitter to signal that it observed an error
        select! {
            run = self.run_inner() => {
                // Halt the TX worker
                self.closer.close();
                error!("Receive Error: {:?}", run);
            },
            _clf = close.wait() => {},
        }

        // Remove this interface from the list
        self.nsh.stack().manage_profile(|im| {
            _ = im.deregister_interface(self.interface_id);
        });
    }

    pub async fn run_inner(&mut self) -> ReceiverError {
        let overhead = max_encoding_overhead(self.mtu as usize);
        let mut cobs_buf = CobsAccumulator::new(self.mtu as usize + overhead);
        let mut raw_buf = vec![0u8; 4096].into_boxed_slice();

        loop {
            let rd = self.skt.read(&mut raw_buf);
            let close = self.closer.wait();

            let ct = select! {
                r = rd => {
                    match r {
                        Ok(0) | Err(_) => {
                            warn!("recv run {} closed", self.net_id);
                            return ReceiverError::SocketClosed
                        },
                        Ok(ct) => ct,
                    }
                }
                _c = close => {
                    return ReceiverError::SocketClosed;
                }
            };

            let buf = &mut raw_buf[..ct];
            let mut window = buf;

            'cobs: while !window.is_empty() {
                window = match cobs_buf.feed_raw(window) {
                    FeedResult::Consumed => break 'cobs,
                    FeedResult::OverFull(new_wind) => new_wind,
                    FeedResult::DecodeError(new_wind) => new_wind,
                    FeedResult::Success { data, remaining }
                    | FeedResult::SuccessInput { data, remaining } => {
                        process_frame(self.net_id, data, &self.nsh, self.interface_id);
                        remaining
                    }
                };
            }
        }
    }
}

pub async fn register_interface<N>(
    stack: N,
    serial_path: &str,
    baud: u32,
    max_ergot_packet_size: u16,
    outgoing_buffer_size: usize,
) -> Result<u64, Error>
where
    N: NetStackHandle<Profile = DirectRouter<TokioSerialInterface>>,
    N: Send + 'static,
{
    let port = tokio_serial_v5::new(serial_path, baud)
        .open_native_async()
        .map_err(|e| Error::Serial(format!("Open Error: {:?}", e)))?;
    let (rx, tx) = tokio::io::split(port);
    let q: StdQueue = new_std_queue(outgoing_buffer_size);
    let res = stack.stack().manage_profile(|im| {
        let ident =
            im.register_interface(Sink::new_from_handle(q.clone(), max_ergot_packet_size))?;
        let state = im.interface_state(ident)?;
        match state {
            InterfaceState::Active { net_id, node_id: _ } => Some((ident, net_id)),
            _ => {
                _ = im.deregister_interface(ident);
                None
            }
        }
    });
    let Some((ident, net_id)) = res else {
        return Err(Error::OutOfNetIds);
    };
    let closer = Arc::new(WaitQueue::new());
    let rx_worker = RxWorker {
        nsh: stack.clone(),
        skt: rx,
        closer: closer.clone(),
        mtu: max_ergot_packet_size,
        interface_id: ident,
        net_id,
    };
    let tx_worker = TxWorker {
        net_id,
        tx,
        rx: <StdQueue as BbqHandle>::stream_consumer(&q),
        closer,
    };

    tokio::task::spawn(rx_worker.run());
    tokio::task::spawn(tx_worker.run());

    Ok(ident)
}
