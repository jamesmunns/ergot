//! # Major Concepts
//!
//! There are some concepts/vocabulary that are important to understand, both as a contributor and as a user. I **want** other people to use and contribute to Ergot: if you aren't able to do this because you don't know how, that's a problem, and I intend to fix it! Please [open an issue] or [start a discussion]!
//!
//! [open an issue]: https://github.com/jamesmunns/ergot/issues/new
//! [start a discussion]: https://github.com/jamesmunns/ergot/discussions/new/choose
//!
//! ## The Netstack
//!
//! The **Netstack** is the core component of messaging in Ergot. When you create sockets, you do it through the Netstack. When you send messages, you send it to the Netstack. If you have external interfaces, you manage them through the Netstack. For all intents and purposes, the Netstack is your post office for communication.
//!
//! The Netstack itself is not an "active" component, there is currently no background task that makes it tick. Instead, it's more of a data structure: it holds state, and is the meeting point for other components that are active. The Netstack only does things when driven externally: when you send a packet, when a packet is received externally, etc.
//!
//! The Netstack manages concurrency internally, using a mutex to serialize all access. For embedded systems, this is generally the right choice: we have a single CPU with a single "thread" of execution. This means that all actions done to the Netstack must be **immediate**. Sending a packet to a local destination means that the message is given to a socket (or is rejected if there is no matching socket, or the socket is full). Sending a packet to a remote destination means the message is enqueued to the external interface (or is rejected if there is no matching destination, or the outgoing interface queue is full).
//!
//! This seems a little totalitarian, you might say, "what if I don't want my message to be rejected?!", but at the end of the day, our space is limited, and we are targeting systems that don't necessarily have a heap. That means that messages must live somewhere if their delivery is to be deferred, and if our destinations are full, then there are only two options: drop the message, or halt execution until there is space. For now, we've chosen the former, and it is the responsibility of the user to "right size" their buffers to ensure messages are not dropped, and if they are, that the system can handle this appropriately.
//!
//! ## Sockets
//!
//! After the Netstack itself, the next major component you will interact with are **Sockets**. Unlike other Sockets you may have interacted with in TCP or UDP, sockets in Ergot are a little different.
//!
//! First, Sockets in Ergot generally only *receive* data. As a user, you open a socket, because you would like to receive some kind of data.
//!
//! Second, Sockets in Ergot are always **typed**: rather than receiving a stream of bytes, like a TCP socket, or a stream of frames, like a UDP socket, they receive a specific data type. This means that they act more like **Channels** in Rust, where you receive a stream of data structures of a particular kind. Serialization and Deserialization is handled by the sockets themselves.
//!
//! Third, Sockets in Ergot can be customized to suit different kinds of behavior. You may want a socket that listens to a stream of broadcast messages, or you may want a socket that is intended to accept specific requests. Currently, making a new "kind" of socket means opening a PR to Ergot.
//!
//! Sockets achieve this "specialization" through two major routes:
//!
//! Sockets have *metadata* that describes what kind of messages they accept, and this affects how or if the Netstack will route messages to that socket. This metadata is set when creating the socket, and is constant for the lifetime of the socket.
//!
//! Sockets also have a *vtable*, or a list of methods that the netstack can call when it decides that it DOES want to route a message to a socket. These methods define how a socket decodes/deserializes a message (when necessary), and allow the socket to perform any other actions it may want to at the time of reception.
//!
//! Most **users** will never have to think about these details, only people writing or debugging a particular kind of socket.
//!
//! If sockets are receive only, you may wonder, "how do I send messages", or "what about sockets kinds that are inherently bidirectional?". Sending a message is done through the netstack, so if your socket is providing a request/response or RPC (remote procedure call) type interface, the "server side" of the socket may provide helper methods that accept an incoming message, and then send any generated replies back to the source of the message.
//!
//! Similarly, the Netstack may have helper methods for sending certain types of messages to a socket, either in a "fire and forget" manner (for broadcast type communication), or a method that sends a request, and awaits a response, by opening a one-shot response socket that can receive the expected response.
//!
//! As of today (2025-08-11), there are no socket kinds that automatically handle things like retries or timeouts automatically. This means that you will typically need to handle this as appropriate in your application. In the future, there may be socket kinds that do handle this, storing messages for resending, or automatically detecting timeouts. If you are interested in this functionality, please [start a discussion] on how we can make this happen!
//!
//! ## Addresses
//!
//! In order to route messages, Ergot uses **Addresses** to describe the source and destination of a message. For a more detailed overview, see the [Address Docs].
//!
//! [Address Docs]: https://docs.rs/ergot-base/latest/ergot_base/address/index.html
//!
//! Addresses are made up of three parts:
//!
//! * The Network ID - a 16-bit quantity
//! * The Node ID - an 8-bit quantity
//! * The Port ID - an 8-bit quantity
//!
//! The Network ID describes the *network segment* of an address. A network segment is an area where devices may talk to each other. For a point to point link (e.g. USB, serial ports), a network segment will always have two nodes. For a bus-style link (e.g. simple radios, RS-485 networks), a network segment may have up to 254 nodes.
//!
//! The Node ID describes a single device on a network segment. All devices on a Network ID will have a unique Node ID.
//!
//! When put together, the Network ID + Node ID pair uniquely identify what device to deliver a message to. These may change over time, if the device joins a network segment (e.g. a USB cable is plugged in). Users should not assume that this pair is ever hardcoded or constant.
//!
//! A single device may have multiple Network ID + Node ID pairs, one for each external interface. This may occur if a device is connected to both a USB port and an RS-485 bus, and is responsible for bridging the two network segments together.
//!
//! Finally, each socket is assigned a Port ID. The Netstack is responsible for assigning port IDs when a socket is opened. Generally, these port IDs will be unique, with the exception of broadcast ports, which may all share the port ID of 255. A single device may have up to 254 unique port IDs, and an unlimited number of broadcast ports.
//!
//! A socket has the same Port ID on all interfaces, meaning that if a device has a USB port with the `Net ID:Node ID` of `12.2`, and an RS-485 bus with the `Net ID:Node ID` of `13.1`, a socket with the port ID of `4` will accept packets with the destination of `12.2:4` and `13.1:4`.
//!
//! Currently there is no way to expose a socket to ONLY a single interface. All sockets will accept responses from all external interfaces.
//!
//! ## Frames
//!
//! Ergot is oriented around **frames**. Frames are variable length quantities of bytes, and are expected to contain a full message. A frame consists of the following parts:
//!
//! * A [common frame header](https://docs.rs/ergot-base/latest/ergot_base/wire_frames/struct.CommonHeader.html)
//! * An optional [Any/All frame appendix](https://docs.rs/ergot-base/latest/ergot_base/struct.AnyAllAppendix.html)
//! * An optional Socket Header
//! * The body of a request
//!
//! The Common Frame Header contains the following information:
//!
//! * A Source Address - Where the frame came from
//! * A Destination Address - Where the frame is going
//! * A sequence number - a rotating 16-bit number that can be used for correlating requests and responses
//! * A Frame Kind - an 8-bit number that describes the kind of frame or the kind of socket this frame is for
//! * A TTL - an 8-bit number that is decremented at each hop as a message is routed
//!
//! The optional Any/All Frame Appendix contains the following information:
//!
//! * A "key", an 8 byte/64-bit number that is used to describe the type of the message
//! * An optional "name hash", which is used to differentiate between multiple instances of the same kind of destination socket.
//!
//! The Any/All Frame Appendix is ALWAYS included when the destination port is EITHER:
//!
//! * `0`: The "Any" port, at which point the appendix will be used to select EXACTLY one destination socket that matches the given key/name hash data
//! * `255`: The "All" or broadcast port, at which point the appendix will be used to find ALL destination sockets that match the given key/name hash data
//!
//! For all other destination ports, `1..=254`, the frame MUST NOT include an Any/All Frame Appendix.
//!
//! A socket may include additional information in the optional Socket Header. This information is not considered by the Netstack, but the socket may use it to include additional data, or discriminate between a "data" frame and a "control" frame, e.g. for acknowledging data, or synchronizing flow control data.
//!
//! The body of a request is not considered by the Netstack, and is typically deserialized by the Socket.
//!
//! Currently, Ergot does NOT mandate a minimum or maximum "transmission unit", e.g. the min/max size of a frame.
//!
//! Ergot also currently does not handle fragmentation, or splitting a frame to fit within a maximum transmission unit of an interface.
//!
//! Ergot also currently does not mandate any kind of message integrity check, e.g. a CRC or other checksum.
//!
//! All of the preceding items are considered the concern of a given interface.
//!
//! It is also worth noting that a frame may not ALWAYS be created: If the source and destination of a message exist on the same device, a message may be delivered "in memory", providing the header and body as a data structure, skipping serialization and deserialization. Sockets may choose to handle this case to avoid wasted work.
//!
//! ## Profiles
//!
//! Ergot is expected to be used in a variety of different devices, with drastically different resources and capabilities. The Netstack abstracts over this by being generic over a **Profile**. A Profile represents two main concepts:
//!
//! 1. The external **interfaces** of a device, e.g. connections to other devices via Network Segments
//! 2. The routing capabilities of a device, e.g. how "smart" it is when it comes to routing messages.
//!
//! This allows us to "tune" the netstack to certain behaviors. For example, a "Null" profile describes a device that has NO external interfaces. It does not need to understand how to route messages outside of the current device, because the device can't do that!
//!
//! As a next step up, a device with a single interface may need to understand how to send and receive messages externally, but the routing process is very simple: If a message is for THIS device, deliver it locally. If a message is NOT for this device, send it to the one interface we have. This means that a device does not need to maintain any kind of routing table to determine where a message should be sent.
//!
//! Finally, devices may have multiple interfaces, in which case they will require a larger routing table, to determine WHICH interface a message should be sent to, if any.
//!
//! This flexibility lets simple devices stay simple, and allow for more complex devices with a greater level of resources to perform at their lowest level of requirement.
//!
//! ## Interfaces
//!
//! **Interfaces** describe how a device communicates with a given network segment. Profiles are responsible for managing interfaces, and these concepts are separate to allow different kinds of profiles to reuse the same code for sending and receiving messages, separately from how they handle routing.
//!
//! For example, we may have an interface that uses `embassy-usb` to send and receive frames over USB Bulk endpoints. This interface could be used with a `DirectEdge` profile when the device only has a single interface. The same interface can be re-used with a `Bridging` profile, for a device that connects via USB on one side, and RS-485 on the other side.
//!
//! Interfaces will typically define a "sink", that the Netstack and Profile can use for enqueuing outgoing messages, and will typically have two worker tasks:
//!
//! * An "RX" worker task, that decodes incoming frames, and feeds them to the Netstack
//! * A "TX" worker task, that is responsible for draining the "sink", and encoding frames and sending them out of the interface.
//!
//! One or both of these worker tasks will also typically be responsible for tracking the "link state" of the interface, for example reporting to the Profile if a USB cable is disconnected, so frames will no longer be routable to that interface.
