# Demos

These are various demos that are currently used to showcase specific features, and in many cases, act as basic smoke tests that things are still working.

The demos are split into separate cargo workspaces, one per target platform.

Currently, there are demos for:

* `std` - meant to run on typical desktop platforms (win/mac/linux)
* `esp32c6` - Applications for the ESP32-C6 platform, typically using the `ESP32-C6-DevKitC-1`
* `rp2040` - Applications for the RP2040 platform, typically using a `Pico` development board, with or without a debugger.
* `nrf52840` - Applications for the `nRF52840` platform, typically using an `nRF52840-DK`

## Working Demo Combinations

### USB

These are demos that just ping and post over usb, a simple demo for getting
started as it requires no components besides the microcontroller itself. the
workspaces for these are

- `std/ergot-nusb-router`
- `rp2040/rp2040-eusb` / `rp2350/rp2350-eusb` / `nrf52840/nrf52840-eusb`

### Tilt

The tilt project is a demo to measure some IMU data and send it over the
network, the workspaces for this is

- `std/tilt-app`
- `rp2350/rp2350-tilt`
