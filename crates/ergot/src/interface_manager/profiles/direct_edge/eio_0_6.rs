use bbq2::traits::bbqhdl::BbqHandle;
use cobs_acc::{CobsAccumulator, FeedResult};
use defmt::{debug, warn};
use embedded_io_async_0_6::Read;

use super::DirectEdge;
use crate::{
    Header,
    interface_manager::{
        InterfaceState, Profile, interface_impls::embedded_io::IoInterface,
        profiles::direct_edge::EDGE_NODE_ID,
    },
    net_stack::NetStackHandle,
    wire_frames::de_frame,
};

pub type EmbeddedIoManager<Q> = DirectEdge<IoInterface<Q>>;

pub struct RxWorker<Q, N, R>
where
    N: NetStackHandle<Profile = EmbeddedIoManager<Q>>,
    Q: BbqHandle + 'static,
    R: Read,
{
    nsh: N,
    rx: R,
    net_id: Option<u16>,
}

impl<Q, N, R> RxWorker<Q, N, R>
where
    N: NetStackHandle<Profile = EmbeddedIoManager<Q>>,
    Q: BbqHandle + 'static,
    R: Read,
{
    pub fn new(net: N, rx: R) -> Self {
        Self {
            nsh: net,
            rx,
            net_id: None,
        }
    }

    pub async fn run(&mut self, frame: &mut [u8], scratch: &mut [u8]) -> Result<(), R::Error> {
        // Mark the interface as established
        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state((), InterfaceState::Inactive));
        let res = self.run_inner(frame, scratch).await;
        _ = self
            .nsh
            .stack()
            .manage_profile(|im| im.set_interface_state((), InterfaceState::Down));
        res
    }

    pub async fn run_inner(
        &mut self,
        frame: &mut [u8],
        scratch: &mut [u8],
    ) -> Result<(), R::Error> {
        let mut acc = CobsAccumulator::new(frame);
        let Self { nsh, rx, net_id } = self;
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
                        process_frame(net_id, data, nsh);
                        remain = remaining;
                    }
                }
            }
        }
    }
}

fn process_frame<Q, N>(net_id: &mut Option<u16>, data: &[u8], nsh: &mut N)
where
    Q: BbqHandle + 'static,
    N: NetStackHandle<Profile = EmbeddedIoManager<Q>>,
{
    let Some(mut frame) = de_frame(data) else {
        warn!(
            "Decode error! Ignoring frame on net_id {}",
            net_id.unwrap_or(0)
        );
        return;
    };

    debug!("Got Frame!");

    let take_net = net_id.is_none()
        || net_id.is_some_and(|n| frame.hdr.dst.network_id != 0 && n != frame.hdr.dst.network_id);

    if take_net {
        nsh.stack().manage_profile(|im| {
            _ = im.set_interface_state(
                (),
                InterfaceState::Active {
                    net_id: frame.hdr.dst.network_id,
                    node_id: EDGE_NODE_ID,
                },
            );
        });
        *net_id = Some(frame.hdr.dst.network_id);
    }

    // If the message comes in and has a src net_id of zero,
    // we should rewrite it so it isn't later understood as a
    // local packet.
    //
    // TODO: accept any packet if we don't have a net_id yet?
    if let Some(net) = net_id.as_ref()
        && frame.hdr.src.network_id == 0
    {
        assert_ne!(frame.hdr.src.node_id, 0, "we got a local packet remotely?");
        assert_ne!(frame.hdr.src.node_id, 2, "someone is pretending to be us?");

        frame.hdr.src.network_id = *net;
    }

    // TODO: if the destination IS self.net_id, we could rewrite the
    // dest net_id as zero to avoid a pass through the interface manager.
    //
    // If the dest is 0, should we rewrite the dest as self.net_id? This
    // is the opposite as above, but I dunno how that will work with responses
    let hdr = frame.hdr.clone();
    let hdr: Header = hdr.into();
    let res = match frame.body {
        Ok(body) => nsh.stack().send_raw(&hdr, frame.hdr_raw, body),
        Err(e) => nsh.stack().send_err(&hdr, e),
    };

    match res {
        Ok(()) => {}
        Err(e) => {
            // TODO: match on error, potentially try to send NAK?
            warn!("send error: {}", e);
        }
    }
}

impl<Q, N, R> Drop for RxWorker<Q, N, R>
where
    N: NetStackHandle<Profile = EmbeddedIoManager<Q>>,
    Q: BbqHandle + 'static,
    R: Read,
{
    fn drop(&mut self) {
        // No receiver? Drop the interface.
        self.nsh.stack().manage_profile(|im| {
            _ = im.set_interface_state((), InterfaceState::Down);
        })
    }
}
