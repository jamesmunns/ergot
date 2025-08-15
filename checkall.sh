#!/bin/bash

set -euxo pipefail

# Check all the crates
cargo check --features=std --manifest-path=./crates/ergot-base/Cargo.toml
cargo check --features=std --manifest-path=./crates/ergot/Cargo.toml
cargo check --features=std --manifest-path=./crates/cobs-acc/Cargo.toml

# Check all the demo workspaces
cargo check --all --manifest-path=./demos/std/Cargo.toml
cargo check --all --target=thumbv7em-none-eabi --manifest-path=./demos/nrf52840/Cargo.toml
cargo check --all --target=thumbv6m-none-eabi --manifest-path=./demos/rp2040/Cargo.toml
cargo check --all --target=riscv32imac-unknown-none-elf --manifest-path=./demos/esp32c6/Cargo.toml
cargo check --all --target=thumbv8m.main-none-eabihf --manifest-path=./demos/rp2350/Cargo.toml
