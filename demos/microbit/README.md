# Ergot demos for the micro:bit v2

These demos show how to use ergot on the micro:bit v2, which uses the nrf52833 chip. 
The demos are similar to the nrf52840 ones, but adopting to the different hardware
and specific to this board, which you might already have laying around.

No board support crate is used, and we drive the hardware, for simplicity, directly with `embassy_nrf`.

This demos are board specific to provide the simplest possible starting experience
utilizing a low-cost and widely available board. 
In that spirit, code comments are sprinkled throughout more extensively than usual.

To run these demos, you need a micro:bit v2 board, a USB cable, and [probe-rs](https://probe.rs/).
A good summary of how to get started with Rust and the micro:bit v2 is available in the 
[Embedded Rust Discovery Book](https://docs.rust-embedded.org/discovery-mb2/index.html).

## Demos currently implemented:

- `microbit-null`: A minimal demo that does not communicate with the outside world. 
  Message passing between tasks is implemented with ergot. After loading, press the two buttons and/or 
  the touch logo to see some LEDs light up.

## Further information:

- [micro:bit Circuit Schematics, assembly and test point map](https://tech.microbit.org/hardware/schematic/)
- [micro:bit Hardware schematic](https://github.com/microbit-foundation/microbit-v2-hardware)
