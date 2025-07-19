#!/bin/bash

set -euxo pipefail

cargo check --features=std --manifest-path=./crates/ergot-base/Cargo.toml
cargo check --features=std --manifest-path=./crates/ergot/Cargo.toml
cargo check --all --manifest-path=./demos/std/Cargo.toml
cargo check --all --target=thumbv7em-none-eabi --manifest-path=./demos/nrf52840/Cargo.toml
cargo check --all --target=thumbv6m-none-eabi --manifest-path=./demos/rp2040/Cargo.toml
