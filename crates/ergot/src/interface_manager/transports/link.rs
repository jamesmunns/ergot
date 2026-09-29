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
