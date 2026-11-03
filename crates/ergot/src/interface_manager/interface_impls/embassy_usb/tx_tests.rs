// Exercise both adapters against the same endpoint behavior. A pending write
// must survive a host pause even when the estimated frame deadline expires.
macro_rules! tests_for {
    ($version:literal, $module:ident, $usb:ident) => {
        #[cfg(feature = $version)]
        mod $module {
            use core::{
                future::Future,
                task::{Context, Waker},
            };
            use std::sync::{
                Arc, Mutex,
                atomic::{AtomicBool, AtomicUsize, Ordering},
            };

            use tokio::sync::Notify;
            use $usb::driver::{
                Endpoint, EndpointAddress, EndpointError, EndpointIn, EndpointInfo, EndpointType,
            };

            use super::super::TransmitError;

            struct State {
                packets: Mutex<Vec<Vec<u8>>>,
                attempts: AtomicUsize,
                cancelled: AtomicUsize,
                disabled: AtomicBool,
                resume: Notify,
            }

            struct MockEndpoint {
                info: EndpointInfo,
                state: Arc<State>,
                block_at: Option<usize>,
            }

            impl MockEndpoint {
                fn new(block_at: Option<usize>) -> Self {
                    Self {
                        info: EndpointInfo {
                            addr: EndpointAddress::from(0x81),
                            ep_type: EndpointType::Bulk,
                            max_packet_size: 64,
                            interval_ms: 0,
                        },
                        state: Arc::new(State {
                            packets: Mutex::new(Vec::new()),
                            attempts: AtomicUsize::new(0),
                            cancelled: AtomicUsize::new(0),
                            disabled: AtomicBool::new(false),
                            resume: Notify::new(),
                        }),
                        block_at,
                    }
                }
            }

            // Detect cancellation of the endpoint future, including a timeout
            // that restarts the same packet with a fresh write operation.
            struct PendingWrite<'a> {
                state: &'a State,
                completed: bool,
            }

            impl Drop for PendingWrite<'_> {
                fn drop(&mut self) {
                    if !self.completed {
                        self.state.cancelled.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }

            impl Endpoint for MockEndpoint {
                fn info(&self) -> &EndpointInfo {
                    &self.info
                }

                async fn wait_enabled(&mut self) {}
            }

            impl EndpointIn for MockEndpoint {
                async fn write(&mut self, buf: &[u8]) -> Result<(), EndpointError> {
                    let attempt = self.state.attempts.fetch_add(1, Ordering::Relaxed);
                    if self.block_at == Some(attempt) {
                        let mut pending = PendingWrite {
                            state: &self.state,
                            completed: false,
                        };
                        self.state.resume.notified().await;
                        pending.completed = true;
                    }
                    if self.state.disabled.load(Ordering::Relaxed) {
                        return Err(EndpointError::Disabled);
                    }
                    self.state.packets.lock().unwrap().push(buf.to_vec());
                    Ok(())
                }
            }

            #[tokio::test(start_paused = true)]
            async fn host_pause_does_not_cancel_a_packet_or_drop_the_suffix() {
                let data: Vec<u8> = (0..255).collect();
                // Pause before the first, a middle, or the final short packet.
                for block_at in [0, 1, 3] {
                    let mut ep = MockEndpoint::new(Some(block_at));
                    let state = ep.state.clone();
                    let mut pending_frame = false;
                    let mut send = Box::pin(super::super::$module::send_all(
                        &mut ep,
                        &data,
                        &mut pending_frame,
                        2,
                        64,
                    ));
                    let mut cx = Context::from_waker(Waker::noop());
                    assert!(send.as_mut().poll(&mut cx).is_pending());
                    tokio::time::advance(core::time::Duration::from_millis(200)).await;
                    assert!(
                        send.as_mut().poll(&mut cx).is_pending(),
                        "a slow host must apply backpressure, not terminate the message"
                    );
                    assert_eq!(state.cancelled.load(Ordering::Relaxed), 0);
                    assert_eq!(state.attempts.load(Ordering::Relaxed), block_at + 1);
                    state.resume.notify_one();
                    assert!(send.await.is_ok());
                    assert!(!pending_frame);
                    let packets = state.packets.lock().unwrap();
                    assert_eq!(
                        packets.iter().map(Vec::len).collect::<Vec<_>>(),
                        [64, 64, 64, 63]
                    );
                    assert_eq!(packets.concat(), data);
                    assert_eq!(state.cancelled.load(Ordering::Relaxed), 0);
                }
            }

            #[tokio::test(start_paused = true)]
            async fn delayed_zlp_is_not_cancelled_or_duplicated() {
                let mut ep = MockEndpoint::new(Some(2));
                let state = ep.state.clone();
                let mut pending_frame = false;
                let mut send = Box::pin(super::super::$module::send_all(
                    &mut ep,
                    &[0x5a; 128],
                    &mut pending_frame,
                    2,
                    64,
                ));
                let mut cx = Context::from_waker(Waker::noop());
                assert!(send.as_mut().poll(&mut cx).is_pending());
                tokio::time::advance(core::time::Duration::from_millis(200)).await;
                assert!(send.as_mut().poll(&mut cx).is_pending());
                assert_eq!(state.cancelled.load(Ordering::Relaxed), 0);
                state.resume.notify_one();
                assert!(send.await.is_ok());
                assert!(!pending_frame);
                assert_eq!(
                    state
                        .packets
                        .lock()
                        .unwrap()
                        .iter()
                        .map(Vec::len)
                        .collect::<Vec<_>>(),
                    [64, 64, 0]
                );
            }

            #[tokio::test(start_paused = true)]
            async fn disabled_endpoint_ends_a_blocked_transfer() {
                let mut ep = MockEndpoint::new(Some(1));
                let state = ep.state.clone();
                let mut pending_frame = false;
                let mut send = Box::pin(super::super::$module::send_all(
                    &mut ep,
                    &[0x5a; 255],
                    &mut pending_frame,
                    2,
                    64,
                ));
                let mut cx = Context::from_waker(Waker::noop());
                assert!(send.as_mut().poll(&mut cx).is_pending());
                tokio::time::advance(core::time::Duration::from_millis(200)).await;
                assert!(send.as_mut().poll(&mut cx).is_pending());
                state.disabled.store(true, Ordering::Relaxed);
                state.resume.notify_one();
                assert!(matches!(send.await, Err(TransmitError::ConnectionClosed)));
                assert_eq!(state.packets.lock().unwrap().len(), 1);
                assert_eq!(state.cancelled.load(Ordering::Relaxed), 0);
            }

            #[tokio::test]
            async fn short_and_aligned_messages_keep_their_boundaries() {
                for len in [0, 1, 63, 64, 65, 128, 255, 512] {
                    let mut ep = MockEndpoint::new(None);
                    let mut pending_frame = false;
                    let data = vec![0x5a; len];
                    assert!(
                        super::super::$module::send_all(&mut ep, &data, &mut pending_frame, 2, 64)
                            .await
                            .is_ok()
                    );
                    let packets = ep.state.packets.lock().unwrap();
                    assert_eq!(packets.concat(), data);
                    let zlps = packets.iter().filter(|p| p.is_empty()).count();
                    assert_eq!(zlps, usize::from(len != 0 && len % 64 == 0));
                    assert!(!pending_frame);
                }
            }
        }
    };
}

tests_for!("embassy-usb-v0_5", eusb_0_5, embassy_usb_0_5);
tests_for!("embassy-usb-v0_6", eusb_0_6, embassy_usb_0_6);
