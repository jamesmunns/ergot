#!/bin/bash

set -euxo pipefail

cargo fmt --manifest-path=./crates/ergot-base/Cargo.toml
cargo fmt --manifest-path=./crates/ergot/Cargo.toml
cargo fmt --all --manifest-path=./demos/std/Cargo.toml
cargo fmt --all --manifest-path=./demos/nrf52840/Cargo.toml
cargo fmt --all --manifest-path=./demos/rp2040/Cargo.toml
