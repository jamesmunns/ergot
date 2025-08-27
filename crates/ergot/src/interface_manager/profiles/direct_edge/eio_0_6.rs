use cobs_acc::{CobsAccumulator, FeedResult};
use embedded_io_async_0_6::Read;

use crate::{
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::embedded_io::IoInterface,
        profiles::direct_edge::{DirectEdge, process_frame},
    },
    net_stack::NetStackHandle,
};

pub type EmbeddedIoManager<Q> = DirectEdge<IoInterface<Q>>;

pub struct RxWorker<N, R>
where
    N: NetStackHandle,
    R: Read,
{
    nsh: N,
    rx: R,
    net_id: Option<u16>,
    ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
}

impl<N, R> RxWorker<N, R>
where
    N: NetStackHandle,
    R: Read,
{
    pub fn new(
        net: N,
        rx: R,
        ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
    ) -> Self {
        Self {
            nsh: net,
            rx,
            net_id: None,
            ident,
        }
    }

    pub async fn run(&mut self, frame: &mut [u8], scratch: &mut [u8]) -> Result<(), R::Error> {
        // Mark the interface as established
        _ = self.nsh.stack().manage_profile(|im| {
            im.set_interface_state(self.ident.clone(), InterfaceState::Inactive)
        });
        let res = self.run_inner(frame, scratch).await;
        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state(self.ident.clone(), InterfaceState::Down));
        res
    }

    pub async fn run_inner(
        &mut self,
        frame: &mut [u8],
        scratch: &mut [u8],
    ) -> Result<(), R::Error> {
        let mut acc = CobsAccumulator::new(frame);
        let Self {
            nsh,
            rx,
            net_id,
            ident,
        } = self;
        'outer: loop {
            let used = rx.read(scratch).await?;
            let mut remain = &mut scratch[..used];

            loop {
                match acc.feed_raw(remain) {
                    FeedResult::Consumed => continue 'outer,
                    FeedResult::OverFull(items) => {
                        remain = items;
                    }
                    FeedResult::DecodeError(items) => {
                        remain = items;
                    }
                    FeedResult::Success { data, remaining }
                    | FeedResult::SuccessInput { data, remaining } => {
                        process_frame(net_id, data, nsh, ident.clone());
                        remain = remaining;
                    }
                }
            }
        }
    }
}

impl<N, R> Drop for RxWorker<N, R>
where
    N: NetStackHandle,
    R: Read,
{
    fn drop(&mut self) {
        // No receiver? Drop the interface.
        self.nsh.stack().manage_profile(|im| {
            _ = im.set_interface_state(self.ident.clone(), InterfaceState::Down);
        })
    }
}
