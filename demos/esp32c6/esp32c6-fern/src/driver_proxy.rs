use core::{
    future::{pending, poll_fn}, pin::pin, task::Poll
};

use bbq2::{queue::BBQueue, traits::{coordination::cas::AtomicCoord, notifier::maitake::MaiNotSpsc, storage::Inline}};
use embassy_executor::task;
use embassy_futures::select::select3;
use embassy_net_driver::Driver;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use esp_radio::wifi::WifiDevice;
use fern_icd::{AllDriverMetadata, DriverPutTxEndpoint, FifoFull, Frame, MetadataStateChangedTopic};

use crate::STACK;

#[task]
pub async fn run_proxy(device: WifiDevice<'static>) {
    let mutex: Mutex<NoopRawMutex, _> = Mutex::new(device);
    let state_fut = manage_state(&mutex);
    let _ = select3(
        state_fut,
        pending::<()>(), // todo: rx
        pending::<()>(), // todo: tx
    ).await;
}

async fn manage_outgoing(device: &Mutex<NoopRawMutex, WifiDevice<'static>>) {
    static INQ: BBQueue<Inline<8192>, AtomicCoord, MaiNotSpsc> = BBQueue::new();
    let server = STACK.stack_bounded_endpoint_server_bor_req::<_, DriverPutTxEndpoint>(
        &INQ,
        1700,
        None,
    );
    let server = pin!(server);
    let mut hdl = server.attach();
    let prod = INQ.framed_producer();

    // TODO: run notification?

    loop {
        let mut rqst = hdl.recv_manual().await;
        let Some(req) = rqst.decode() else {
            continue;
        };
    }
}

async fn manage_state(device: &Mutex<NoopRawMutex, WifiDevice<'static>>) {
    let mut capabilities;
    let mut hw_addr;
    let mut link_state;

    // Prime the link state by manually getting it once, pretty much just ignoring
    // the waker stuff
    {
        let mut guard = device.lock().await;
        link_state = poll_fn(|cx| Poll::Ready(guard.link_state(cx))).await;
    }
    loop {
        // We JUST updated the link state (either initial or we were notified),
        // so also grab the latest capabilities/hw_addr info too
        {
            let guard = device.lock().await;
            capabilities = guard.capabilities();
            hw_addr = guard.hardware_address();
        }
        _ = STACK.broadcast_topic::<MetadataStateChangedTopic>(
            &AllDriverMetadata {
                capabilities: capabilities.clone().into(),
                link_state: link_state.into(),
                hw_addr: hw_addr.try_into().unwrap(),
            },
            None,
        );

        link_state = poll_fn(|cx| {
            // this is awful
            let Ok(mut guard) = device.try_lock() else {
                // welp
                cx.waker().wake_by_ref();
                return Poll::Pending;
            };

            let new_state = guard.link_state(cx);
            if new_state == link_state {
                Poll::Pending
            } else {
                Poll::Ready(new_state)
            }
        })
        .await;
    }
}
