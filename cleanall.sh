#!/usr/bin/env bash

set -euxo pipefail

# clean all the crates
cargo clean --manifest-path=./crates/ergot/Cargo.toml
cargo clean --manifest-path=./crates/cobs-acc/Cargo.toml

# clean all the demo workspaces
cargo clean --manifest-path=./demos/shared-icd/Cargo.toml
cargo clean --manifest-path=./demos/shared-icd/Cargo.toml
cargo clean --manifest-path=./demos/std/Cargo.toml
cargo clean --manifest-path=./demos/microbit/Cargo.toml
cargo clean --manifest-path=./demos/nrf52840/Cargo.toml
cargo clean --manifest-path=./demos/rp2040/Cargo.toml
cargo clean --manifest-path=./demos/esp32c6/Cargo.toml
cargo clean --manifest-path=./demos/rp2350/Cargo.toml
cargo clean --manifest-path=./demos/stm32f303vc/Cargo.toml
