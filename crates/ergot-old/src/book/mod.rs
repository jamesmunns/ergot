//! # `ergot` - An Introduction
//!
//! `ergot` is a messaging library written in Rust. It is currently experimental.
//!
//! ## What do you mean, "Messaging Library"?
//!
//! `ergot` is an evolution in the `postcard` family.
//!
//! `postcard` is a **binary wire format**, that describes how to convert Rust data
//! types to and from a binary form that can be sent over a network, or stored to
//! disk. However: it has no prescription of HOW to communicate between devices, at
//! a higher level. It is just **data**.
//!
//! `postcard-rpc` is a **protocol** using the `postcard` wire format. It prescribes
//! *behaviors* for when and how to send messages to perform certain operations,
//! such as request/response "endpoint" pairs, or broadcast-style "topic" messages.
//! However: it has no prescription of HOW to connect two devices, or how a server
//! could manage connections with more than one client at a time.
//!
//! `ergot` is a **messaging library**. It handles concerns such as routing, addressing,
//! and socket-oriented connections between many devices. It uses `postcard` for
//! the encoding of user data, and currently offers protocol functionality inspired
//! by `postcard-rpc`. It allows devices of various sizes to become part of a
//! network, and for all nodes to communicate with each other.
//!
//! As a messaging library: It should help you to communicate *across* systems, whether
//! that is between tasks within the same program, between devices that are directly
//! connected to each other, or between devices separated by multiple network links.
//!
//! ## Why `ergot`?
//!
//! The decision to build a custom messaging library rather than a layer on top of
//! existing tech was made to support four main goals:
//!
//! ### Communication for devices of ANY size
//!
//! First, **the ability to support devices with different levels of capabilities**,
//! ranging from the smallest Cortex-M0+ devices, all the way up to desktop devices.
//!
//! `ergot` achieves this by making many capabilities of the messaging library optional
//! and swappable. Smaller devices can be part of a network, without having to
//! manage things like routing tables or large network buffers. Instead, they can
//! rely on larger devices they are connected to, to manage heavier networking
//! tasks, allowing them to still be communicated with.
//!
//! ### Communication across ANY data link
//!
//! Second, **the ability to support a wide range of communication links**, within
//! the same coherent network. For example, a PC could be connected over USB to
//! a microcontroller, which then is connected to four smaller microcontrollers over
//! SPI, UART, RS-485, or I2C.
//!
//! `ergot` achieves this by allowing for devices to define custom "interfaces": if
//! you can define how to get a chunk of data from one device to another, you can
//! probably implement an `ergot` interface.
//!
//! ### Type Safe Communication, not chunks/streams of bytes
//!
//! Rather than only providing transport of frame (e.g. UDP) or streams (e.g. TCP)
//! of bytes, `ergot` instead focuses on type safe communication. Sockets have a
//! type associated with them, always. This means that using `ergot` sockets doesn't
//! feel like talking over a network, it feels like using channels: data goes in one
//! side, data comes out the other.
//!
//! When sending data locally (e.g. to a socket on the same machine), this allows us
//! to *completely skip* serialization, placing data in the destination the same as
//! we would for a channel. When sending data remotely (from or to a remote
//! machine), senders or receivers don't need to worry about doing the serialization
//! themselves: all data is encoded as `postcard` data automatically.
//!
//! ### Just because I can
//!
//! I've been working on communication between devices within the `postcard`
//! cinematic universe since 2019. This has always been a cycle of exploring how far
//! can I get with my current tools, finding the limitations of the tools I have,
//! then building the next level up to address the shortcomings, and repeating that
//! cycle.
//!
//! `ergot` is the next step after `postcard-rpc`: I've reached the limitations of
//! what can be achieved with JUST a point to point communication protocol, when you
//! want to connect to more than one device, or want devices to talk with each
//! other, rather than just a central "host" computer.
//!
//! It's possible this all could have been done on top of TCP/IP, writing my own
//! TCP/IP stack, or extending the wonderful `smoltcp` crate. I didn't *want* to do
//! that. We'll see if it pays off.
//!
//! ## Inspiration
//!
//! `ergot` is strongly influenced by [AppleTalk], a networking stack used by
//! computers in the late 80's and early 90's. It is an interesting place to pull
//! design choices from: the computers of that time are generally comparable to
//! today's microcontrollers, and if it worked for them then, why shouldn't the
//! same design choices work today?
//!
//! [AppleTalk]: https://en.wikipedia.org/wiki/AppleTalk

pub mod _01_what_can_you_do;
pub mod _02_major_concepts;
pub mod _03_feature_overview;
