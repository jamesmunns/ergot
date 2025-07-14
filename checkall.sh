#!/bin/bash

set -euxo pipefail

cargo check --features=std --manifest-path=./crates/ergot-base/Cargo.toml
cargo check --features=std --manifest-path=./crates/ergot/Cargo.toml
cargo check --manifest-path=./demos/ergot-client/Cargo.toml
cargo check --manifest-path=./demos/ergot-nusb-router/Cargo.toml
cargo check --manifest-path=./demos/ergot-router/Cargo.toml
cargo check --target=thumbv7em-none-eabi --manifest-path=./demos/nrf52840-eusb/Cargo.toml
cargo check --target=thumbv7em-none-eabi --manifest-path=./demos/nrf52840-null/Cargo.toml
cargo check --target=thumbv6m-none-eabi --manifest-path=./demos/rp2040-null/Cargo.toml
