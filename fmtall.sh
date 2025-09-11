#!/usr/bin/env bash

set -euxo pipefail

# Format crates
cargo fmt --manifest-path=./crates/ergot/Cargo.toml
cargo fmt --manifest-path=./crates/cobs-acc/Cargo.toml

# Format all the demo workspaces
cargo fmt --manifest-path=./demos/shared-icd/Cargo.toml
cargo fmt --all --manifest-path=./demos/std/Cargo.toml
cargo fmt --all --manifest-path=./demos/microbit/Cargo.toml
cargo fmt --all --manifest-path=./demos/nrf52840/Cargo.toml
cargo fmt --all --manifest-path=./demos/rp2040/Cargo.toml
cargo fmt --all --manifest-path=./demos/rp2350/Cargo.toml
cargo fmt --all --manifest-path=./demos/esp32c6/Cargo.toml
cargo fmt --all --manifest-path=./demos/stm32f303vc/Cargo.toml
