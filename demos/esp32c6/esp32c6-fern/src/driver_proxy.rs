use core::{
    future::{pending, poll_fn},
    task::Poll,
};

use embassy_executor::task;
use embassy_futures::select::select3;
use embassy_net_driver::Driver;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use esp_radio::wifi::WifiDevice;
use fern_icd::{AllDriverMetadata, MetadataStateChangedTopic};

use crate::STACK;

#[task]
pub async fn run_proxy(device: WifiDevice<'static>) {
    let mutex: Mutex<NoopRawMutex, _> = Mutex::new(device);
    let state_fut = manage_state(&mutex);
    let _ = select3(
        state_fut,
        pending::<()>(),
        pending::<()>(),
    ).await;
}

async fn manage_outgoing(device: &Mutex<NoopRawMutex, WifiDevice<'static>>) {
    // let server = STACK.stack
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
