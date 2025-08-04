# Demos

These are various demos that are currently used to showcase specific features, and in many cases, act as basic smoke tests that things are still working.

The demos are split into separate cargo workspaces, one per target platform.

Currently, there are demos for:

* `std` - meant to run on typical desktop platforms (win/mac/linux)
* `esp32c6` - Applications for the ESP32-C6 platform, typically using the `ESP32-C6-DevKitC-1`
* `rp2040` - Applications for the RP2040 platform, typically using a `Pico` development board, with or without a debugger.
* `nrf52840` - Applications for the `nRF52840` platform, typically using an `nRF52840-DK`
