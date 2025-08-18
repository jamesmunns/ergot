#!/bin/bash

# We disable isolation because we use tokio sleep
# We test on linux because currently just pulling in the net stuff for
#   tokio_tcp sockets causes the runtime to call kqueue, which also doesn't
#   work in miri
#
# TODO: Can we eliminate some of these limitations for testing?
MIRIFLAGS=-Zmiri-disable-isolation \
    cargo +nightly miri test \
    --target x86_64-unknown-linux-gnu \
    --features=tokio-std
