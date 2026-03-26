//! # Feature Overview
//!
//! Let's look at what Ergot can, or will, support, and what kind of setups could be made easier with Ergot.
//!
//! ## Connectivity - Now
//!
//! First, we can look at a "topology" view of how your system might be set up. The hope is that you can "see" your system in one of these diagrams!
//!
//! ### Direct connection, PC to 1x MCU
//!
//! ```text
//! ┌ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─
//!   ┌────────────┐            ┌────────────┐ │
//! │ │            │            │   Micro    │
//!   │     PC     │◀──USB or──▶│ controller │ │
//! │ │            │    UART    │            │
//!   └────────────┘            └────────────┘ │
//! │  ┌─────────────────────┐
//!  ─ ┤  Direct Connection  ├ ─ ─ ─ ─ ─ ─ ─ ─ ┘
//!    └─────────────────────┘
//! ```
//!
//! The first setup is a direct connection between a larger host system, often your desktop, server, or embedded linux application processor, and some kind of microcontroller acting as a "coprocessor" or "sidekick".
//!
//! For cases where you have a desktop-style PC, this allows you to interact with the physical world, reading sensors, driving motors, or handling user input in the form of buttons or dials. Since desktops/laptops don't typically have a any kind of I/O module that is easy to use, this allows you to easily build your own!
//!
//! For cases where you have an embedded-linux style application processor, it can still be useful to offload certain tasks to an embedded coprocessor in order to meet real-time needs, have access to more pins or peripherals than your embedded linux system supports, or even just avoid writing kernel modules for your external hardware.
//!
//! Ergot can help by providing a socket-like interface, meaning your application on the PC can still handle any networking, storage, or bulk processing, while letting the embedded processor handle the details.
//!
//! This is also a great way to decouple your development process: it's easier to revise EITHER the "main processor" or the "sidekick", and to do application development on your laptop, using standard USB or UART interfaces.
//!
//! **For examples, see:**
//!
//! * Using USB:
//!   * [`nrf52840-eusb`] or [`rp2040-eusb`] for the microcontroller projects, using embassy-usb
//!   * [`ergot-nusb-serial`] for the PC side application that manages connection to the MCU(s)
//! * Using Serial:
//!   * [`esp32c6-serial`] for a microcontroler using the built-in USB-serial of the ESP32C6
//!   * [`log-router-serial`] for the PC side application that manages connection to the MCU
//!
//! [`nrf52840-eusb`]: https://github.com/jamesmunns/ergot/tree/main/demos/nrf52840/nrf52840-eusb
//! [`rp2040-eusb`]: https://github.com/jamesmunns/ergot/tree/main/demos/rp2040/rp2040-eusb
//! [`esp32c6-serial`]: https://github.com/jamesmunns/ergot/tree/main/demos/esp32c6/esp32c6-serial
//! [`ergot-nusb-serial`]: https://github.com/jamesmunns/ergot/tree/main/demos/std/ergot-nusb-router
//! [`log-router-serial`]: https://github.com/jamesmunns/ergot/tree/main/demos/std/log-router-serial
//!
//! ### Direct connection, 1:N
//!
//! ```text
//! ┌ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─
//!   ┌────────────┐            ┌────────────┐ │
//! │ │            │◀──────────▶│   Micro    │
//!   │     PC     │◀────────┐  │ controller │ │
//! │ │            │◀─────┐  │  │            │
//!   └────────────┘      │  │  └────────────┘ │
//! │              USB or │  │  ┌────────────┐
//!                 UARTs │  │  │   Micro    │ │
//! │                     │  └─▶│ controller │
//!                       │     │            │ │
//! │                     │     └────────────┘
//!                       │     ┌────────────┐ │
//! │                     │     │   Micro    │
//!                       └────▶│ controller │ │
//! │                           │            │
//!   ┌─────────────────────┐   └────────────┘ │
//! └ ┤   1:N Connections   ├ ─ ─ ─ ─ ─ ─ ─ ─ ─
//!   └─────────────────────┘
//! ```
//!
//! There are also cases where you may require a LOT of I/O, or have a setup where it is desirable to be modular.
//!
//! Rather than requiring a re-spin of your entire hardware to select a processor or microcontroller with more pins or peripherals, you can instead decide to "scale horizontally", adding additional external devices. With USB, this could be as simple as using a USB hub, which can easily scale up to dozens of external devices.
//!
//! This is also useful in cases where it's easier to build a few different "specialized" sidekicks: instead of having ONE microcontroller that needs to handle all of the inputs and outputs, you can take a more "microservice" approach, allowing the firmware of each device to be much simpler, and require less finesse to meet resource requirements.
//!
//! This can allow you to put your PC or USB hub on some kind of backplane, and expose dedicated "cards" or "modules", reducing the number of bespoke configurations required.
//!
//! **For examples, see:**
//!
//! * Using USB:
//!   * [`nrf52840-eusb`] or [`rp2040-eusb`] for the microcontroller projects, using embassy-usb
//!   * [`ergot-nusb-serial`] for the PC side application that manages connection to the MCU(s)
//! * Using Serial:
//!   * [`esp32c6-serial`] for a microcontroler using the built-in USB-serial of the ESP32C6
//!   * [`log-router-serial`] for the PC side application that manages connection to the MCU
//!
//! [`nrf52840-eusb`]: https://github.com/jamesmunns/ergot/tree/main/demos/nrf52840/nrf52840-eusb
//! [`rp2040-eusb`]: https://github.com/jamesmunns/ergot/tree/main/demos/rp2040/rp2040-eusb
//! [`esp32c6-serial`]: https://github.com/jamesmunns/ergot/tree/main/demos/esp32c6/esp32c6-serial
//! [`ergot-nusb-serial`]: https://github.com/jamesmunns/ergot/tree/main/demos/std/ergot-nusb-router
//! [`log-router-serial`]: https://github.com/jamesmunns/ergot/tree/main/demos/std/log-router-serial
//!
//! ```text
//! ┌ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─
//!   ┌────────────┐TCP Sockets ┌────────────┐ │
//! │ │   Server   │◀──────────▶│            │
//!   │  process   │◀────────┐  │  Process   │ │
//! │ │            │◀─────┐  │  │            │
//!   └────────────┘      │  │  └────────────┘ │
//! │                     │  │  ┌────────────┐
//!                       │  │  │            │ │
//! │                     │  └─▶│  Process   │
//!                       │     │            │ │
//! │                     │     └────────────┘
//!                       │     ┌────────────┐ │
//! │                     │     │            │
//!                       └────▶│  Process   │ │
//! │                           │            │
//!   ┌─────────────────────┐   └────────────┘ │
//! └ ┤   1:N Connections   ├ ─ ─ ─ ─ ─ ─ ─ ─ ─
//!   └─────────────────────┘
//! ```
//!
//! It's worth noting that Ergot also supports TCP sockets as a wire interface, which can be useful for testing or simulation outside of physical hardware.
//!
//! This allows you to decouple your application from the actual hardware it interacts with, and do rapid prototyping and testing during development, even if you don't have hardware handy!
//!
//! **For examples, see:**
//!
//! * [`ergot-router`] for the "Server" application shown above
//! * [`ergot-client-tcp`] for the "Process" application shown above
//!
//! [`ergot-router`]: https://github.com/jamesmunns/ergot/tree/main/demos/std/ergot-router
//! [`ergot-client-tcp`]: https://github.com/jamesmunns/ergot/tree/main/demos/std/ergot-client-tcp
//!
//! ### Direct connection, MCU to MCU
//!
//! ```text
//! ┌ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─
//!   ┌────────────┐            ┌────────────┐ │
//! │ │   Micro    │            │   Micro    │
//!   │ Controller │◀───UART───▶│ controller │ │
//! │ │            │            │            │
//!   └────────────┘            └────────────┘ │
//! │  ┌─────────────────────┐
//!  ─ ┤  Direct Connection  ├ ─ ─ ─ ─ ─ ─ ─ ─ ┘
//!    └─────────────────────┘
//! ```
//!
//! Some folks don't necessarily need connectivity to a host PC, but may have more than one MCU on the same board, allowing them to split responsibilities.
//!
//! **For examples, see:**
//!
//! * [`rp2040-serial-pair`] for two RP2040 devices connected by UART
//!
//! [`rp2040-serial-pair`]: https://github.com/jamesmunns/ergot/tree/main/demos/rp2040/rp2040-serial-pair
//!
//! ### Bridged connection, PC to Nx MCU to 1x MCU
//!
//! ```text
//! ┌ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─
//!   ┌────────────┐            ┌────────────┐            ┌────────────┐           │
//! │ │            │            │   Micro    │            │   Micro    │
//!   │     PC     │◀──USB or──▶│ controller │◀──────────▶│ controller │─ ─ ─ ─ ─ ▶│
//! │ │            │    UART    │  (bridge)  │ UART, SPI, │            │(any depth)
//!   └────────────┘            └────────────┘   or I2C   └────────────┘           │
//! │  ┌─────────────────────┐
//!  ─ ┤ Bridged Connections ├ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ┘
//!    └─────────────────────┘
//! ```
//!
//! A bridge device sits between a root router and one or more downstream devices.
//! The bridge uses the unified `Router` profile in **bridge mode**: it has an
//! upstream interface connecting to the root router, and one or more downstream
//! interfaces connecting to edge devices.
//!
//! Traffic that the bridge can route locally (between its own downstreams) stays
//! local. Traffic for unknown destinations is forwarded upstream to the root
//! router. Broadcasts are flooded in both directions (excluding the source).
//!
//! For downstream devices to be reachable from anywhere in the network, the
//! bridge uses **seed routing**: it requests a globally-routable `net_id` from
//! the root router's seed router service, then assigns that `net_id` to the
//! downstream interface. The root router installs a route for that `net_id`
//! through the bridge's upstream link, so traffic from any node in the network
//! can reach the bridge's downstream devices.
//!
//! See `Router::new_bridge()` and `bridge_seed_assign()` for the API.
//!
//! ## Connectivity - Soon
//!
//! * "Broadcast bus"-style interfaces, e.g. RS-485, Radio
//!     * Needs: physical addressing, collision handling, bridged connection support
//! * "Time slice"-style interfaces, e.g. SPI, I2C
//!     * Needs: master polling logic (slave can't initiate), similar to above
//!
//! ## Socket Features - Now
//!
//! Things you can do:
//!
//! ### Broadcast
//!
//! ```text
//! ┌ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ┐
//!                                           ┌─────────┐
//! │                                   ┌────▶│STM32G030│ │
//!                                     │ ┌ ─▶└─────────┘
//! │                                   │                 │
//!   ┌────────┐         ┌────────┐     │ │   ┌─────────┐
//! │ │   PC   │◀───────▶│ RP2040 │◀────┼────▶│STM32G030│ │
//!   └────────┘◀ ─ ─ ─ ─└────────┘ ◀ ─ │ │┌ ▶└─────────┘
//! │                                  ││                 │
//!                                     │ │└ ─┌─────────┐
//! │                                  ││  ─ ─│STM32G030│ │
//!                                     └────▶└─────────┘
//! │ ┌─────────────────────┐          └ ─ ─ ─            │
//!  ─│Broadcast w/ Flooding│─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─
//!   └─────────────────────┘
//! ```
//!
//! ### Endpoint
//!
//! ```text
//! ┌ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─
//!   ┌────────────┐───Request─▶┌────────────┐ │
//! │ │            │◀─Response──│   Micro    │
//!   │     PC     │            │ controller │ │
//! │ │            │◀─Request───│            │
//!   └────────────┘──Response─▶└────────────┘ │
//! │  ┌─────────────────────┐
//!  ─ ┤    Endpoint/RPC     ├ ─ ─ ─ ─ ─ ─ ─ ─ ┘
//!    └─────────────────────┘
//! ```
//!
//! ## Socket Features - Future
//!
//! ### Sessionful Sockets
//!
//! ```text
//! ┌ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ┐
//!   ┌────────────┐                       ┌────────────┐
//! │ │            │  ┌ ─ ─ ─ ─ ─ ─ ─ ─ ┐  │            │ │
//!   │            │◀──────────────────────│            │
//! │ │            │◀─┼─────────────────┼──│   Micro    │ │
//!   │     PC     │──────────────────────▶│ controller │
//! │ │            │◀─┼─────────────────┼──│            │ │
//!   │            │──────────────────────▶│            │
//! │ │            │  └ ─ ─ ─ ─ ─ ─ ─ ─ ┘  │            │ │
//!   └────────────┘                       └────────────┘
//! │ ┌──────────────────────┐                            │
//!  ─│Sessionful Connections├ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─
//!   └──────────────────────┘
//! ```
//!
//! * Allows for:
//!     * Exclusive access to items
//!     * Keepalives, notice when device "drops"
//!     * Reliable deliver, retries, higher QoS
