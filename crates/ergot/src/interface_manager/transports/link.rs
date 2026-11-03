//! The interface-state side shared by the RX workers.

use maitake_sync::WaitQueue;

use crate::{
    interface_manager::{InterfaceState, Profile},
    logging::error,
    net_stack::NetStackHandle,
};

type Ident<N> = <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent;

/// A worker's hold on its interface: the stack, the interface ident and the
/// state observers. Sets the interface [`Down`](InterfaceState::Down) when
/// dropped, so a worker whose task is cancelled does not leave it up.
pub(crate) struct Link<N: NetStackHandle> {
    pub(crate) nsh: N,
    pub(crate) ident: Ident<N>,
    state_notify: Option<&'static WaitQueue>,
}

impl<N: NetStackHandle> Link<N> {
    pub(crate) fn new(nsh: N, ident: Ident<N>) -> Self {
        Self {
            nsh,
            ident,
            state_notify: None,
        }
    }

    pub(crate) fn set_state_notify(&mut self, notify: &'static WaitQueue) {
        self.state_notify = Some(notify);
    }

    /// Wake the state observers, if any.
    pub(crate) fn notify(&self) {
        if let Some(notify) = self.state_notify {
            notify.wake_all();
        }
    }

    /// Set the interface state and wake the observers.
    pub(crate) fn set_state(&self, state: InterfaceState) {
        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state(self.ident.clone(), state))
            .inspect_err(|_e| {
                error!("Error setting interface state: {:?}", _e);
            });
        self.notify();
    }

    /// Restore profile-owned addressing after a physical USB reconnect.
    #[cfg(any(feature = "embassy-usb-v0_5", feature = "embassy-usb-v0_6"))]
    pub(crate) fn establish(&self) {
        _ = self.nsh.stack().manage_profile(|im| {
            if matches!(
                im.interface_state(self.ident.clone()),
                Some(InterfaceState::Active { .. })
            ) {
                return Ok(());
            }
            let net_id = im.interface_net_id(self.ident.clone()).unwrap_or(0);
            let node_id = im
                .interface_node_id(self.ident.clone())
                .unwrap_or(crate::interface_manager::edge_port::EDGE_NODE_ID);
            im.set_interface_state(
                self.ident.clone(),
                InterfaceState::Active { net_id, node_id },
            )
        });
        self.notify();
    }

    /// Take the interface out of `Active` (a liveness timeout, a suspended
    /// link): to link-local addressing with the same node_id if `link_local`,
    /// otherwise to [`Inactive`](InterfaceState::Inactive). A no-op unless it
    /// is `Active`. Returns whether the state changed.
    pub(crate) fn deactivate(&self, link_local: bool) -> bool {
        let changed = self.nsh.stack().manage_profile(|im| {
            let current = im.interface_state(self.ident.clone());
            let Some(InterfaceState::Active { node_id, .. }) = current else {
                return false;
            };
            // Link-local keeps the node_id: a bus device must not fall back to
            // the point-to-point EDGE_NODE_ID.
            let target = if link_local {
                InterfaceState::link_local(node_id)
            } else {
                InterfaceState::Inactive
            };
            if current == Some(target) {
                return false;
            }
            _ = im.set_interface_state(self.ident.clone(), target);
            true
        });
        if changed {
            self.notify();
        }
        changed
    }

    /// Set the interface Down, unless it already is (or is gone).
    pub(crate) fn set_down(&self) {
        let changed = self.nsh.stack().manage_profile(|im| {
            if matches!(
                im.interface_state(self.ident.clone()),
                Some(InterfaceState::Down) | None
            ) {
                return false;
            }
            _ = im.set_interface_state(self.ident.clone(), InterfaceState::Down);
            true
        });
        if changed {
            self.notify();
        }
    }
}

impl<N: NetStackHandle> Drop for Link<N> {
    fn drop(&mut self) {
        self.set_down();
    }
}

#[cfg(all(
    test,
    feature = "tokio-std",
    any(feature = "embassy-usb-v0_5", feature = "embassy-usb-v0_6")
))]
mod tests {
    use super::*;
    use crate::interface_manager::{
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::{
            direct_edge::DirectEdge,
            router::{Router, UPSTREAM_IDENT},
        },
        utils::{cobs_stream, std::new_std_queue},
    };
    use crate::net_stack::ArcNetStack;
    use mutex::raw_impls::cs::CriticalSectionRawMutex;

    type RouterStack = ArcNetStack<
        CriticalSectionRawMutex,
        Router<TokioStreamInterface, rand::rngs::StdRng, 4, 4>,
    >;
    type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<TokioStreamInterface>>;

    fn sink() -> cobs_stream::Sink<crate::interface_manager::utils::std::StdQueue> {
        cobs_stream::Sink::new_from_handle(new_std_queue(4096), 512)
    }

    #[test]
    fn reconnect_restores_router_assignment_and_role() {
        let stack = RouterStack::new_with_profile(Router::new_std());
        let ident = stack
            .manage_profile(|im| im.register_interface(sink()))
            .unwrap();
        stack
            .manage_profile(|im| im.reassign_interface_net_id(ident, 42))
            .unwrap();
        let link = Link::new(stack.clone(), ident);
        for state in [InterfaceState::Down, InterfaceState::Inactive] {
            link.set_state(state);
            link.establish();
            assert_eq!(
                stack.manage_profile(|im| im.interface_state(ident)),
                Some(InterfaceState::Active {
                    net_id: 42,
                    node_id: 1
                })
            );
        }
    }

    #[test]
    fn pending_bridge_downlink_keeps_central_role() {
        let stack = RouterStack::new_with_profile(Router::new_bridge_std(sink()));
        let ident = stack
            .manage_profile(|im| im.register_interface_pending(sink()))
            .unwrap();
        let link = Link::new(stack.clone(), ident);
        link.establish();
        assert_eq!(
            stack.manage_profile(|im| im.interface_state(ident)),
            Some(InterfaceState::link_local(1))
        );

        let upstream = Link::new(stack.clone(), UPSTREAM_IDENT);
        upstream.establish();
        assert_eq!(
            stack.manage_profile(|im| im.interface_state(UPSTREAM_IDENT)),
            Some(InterfaceState::edge_link_local())
        );
    }

    #[test]
    fn target_reconnect_keeps_claimed_node_and_rediscovers_network() {
        let stack = EdgeStack::new_with_profile(DirectEdge::new_target(sink()));
        let link = Link::new(stack.clone(), ());
        link.set_state(InterfaceState::Active {
            net_id: 7,
            node_id: 30,
        });
        link.set_down();
        link.establish();
        assert_eq!(
            stack.manage_profile(|im| im.interface_state(())),
            Some(InterfaceState::link_local(30))
        );
    }

    #[test]
    fn established_assignment_is_unchanged() {
        let stack = EdgeStack::new_with_profile(DirectEdge::new_controller(
            sink(),
            InterfaceState::Active {
                net_id: 7,
                node_id: 1,
            },
        ));
        let link = Link::new(stack.clone(), ());
        link.establish();
        assert_eq!(
            stack.manage_profile(|im| im.interface_state(())),
            Some(InterfaceState::Active {
                net_id: 7,
                node_id: 1
            })
        );
        link.set_down();
        link.establish();
        assert_eq!(
            stack.manage_profile(|im| im.interface_state(())),
            Some(InterfaceState::link_local(1))
        );
    }
}
