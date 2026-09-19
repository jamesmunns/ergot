#![cfg(feature = "std")]

use core::sync::atomic::{AtomicU8, Ordering};

use maitake_sync::WaitQueue;

#[tokio::test]
async fn wait_for_value_observes_a_state_change_before_waiter_registration() {
    let notify = WaitQueue::new();
    let state = AtomicU8::new(0);
    let last_state = state.load(Ordering::Acquire);

    // Model an interface transition that races just ahead of the monitor.
    // A bare `wait().await` would now sleep until an unrelated later change.
    state.store(1, Ordering::Release);
    notify.wake_all();

    let observed = notify
        .wait_for_value(|| {
            let current = state.load(Ordering::Acquire);
            (current != last_state).then_some(current)
        })
        .await
        .unwrap();
    assert_eq!(observed, 1);
}
