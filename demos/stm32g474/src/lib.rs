#![no_std]

use rtt_target::ChannelMode;

/// Common RTT channel layout for all demos:
/// - Up 0: defmt (size 1024)
/// - Up 1: ergot TX (size 1024)
/// - Down 0: ergot RX (size 512)
pub fn init_rtt_channels() -> (
    rtt_target::UpChannel,
    rtt_target::UpChannel,
    rtt_target::DownChannel,
) {
    let channels = rtt_target::rtt_init! {
        up: {
            0: { size: 1024, mode: ChannelMode::NoBlockSkip, name: "defmt" }
            1: { size: 1024, mode: ChannelMode::NoBlockSkip, name: "ergot_tx" }
        }
        down: {
            0: { size: 512, name: "ergot_rx" }
        }
    };
    (channels.up.0, channels.up.1, channels.down.0)
}
