# ESP32C3 example

This example uses the USB Serial/JTAG peripheral built into ESP32 microcontrollers, to demonstrate
compatibility with `embedded-io-async` trait implementations.

The example was built for an ESP32-C3-DevKit-RUST-1, which has:
- An addressable RGB LED on GPIO2
- A single color LED on GPIO7
- An ICM-42670-P IMU attached via I2C at address 0x68
- An SHTC3 Temperature and Humidity sensor attached via I2C at address 0x70