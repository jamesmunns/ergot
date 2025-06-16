# `ergot`

*The latest in unsafe network stacks.*

## Purpose

Ergot is a network stack that can run on a variety of differently sized devices, from large desktop/server PCs down to very small single core microcontrollers.

Ergot allows developers to enjoy a coherent network of devices, regardless of the size of devices, or transport mediums used to connect them.

It includes type-safe sockets, addressing, and routing. In minimal MCU-sized configurations, it requires no allocator, and is `no_std` friendly. In larger PC-sized configurations, allocations may be used for performance and convenience.

Ergot has grown out of the lessons of the `postcard` and `postcard-rpc` projects, and aims to (eventually) supercede `postcard-rpc` in functionality. From a networking perspective, it is heavily inspired by [AppleTalk](https://en.wikipedia.org/wiki/AppleTalk), an OSI-model networking stack used on Mac computers in the late 80s and early 90s.

Ergot is still very early in development. Bugs are expected. Help is welcome.

## Name

The name "ergot" (pronounced "ur-get",  or more specifically /ˈɜːrɡət/, UR-gət) comes from the [Ergot fungus](https://en.wikipedia.org/wiki/Ergot), a parasitic fungus that grows on grains such as rye, produces Lysergic Acid, the precursor of LSD.

This name was chosen in line with the naming theme of the [mycelium](https://github.com/hawkw/mycelium/) project.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
