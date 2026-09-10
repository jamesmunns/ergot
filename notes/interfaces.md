# Interfaces

Now that we have sockets, I should probably figure out interfaces, and generally how to figure out "on the wire" packets.

## A reminder on Addresses

Our Address type is very much inspired by AppleTalk's address format. It is made up of three parts:

* network id (16b)
* node id (8b)
* port/socket addr (8b)

### networks

"network ids" are generally defined as a "network segment". A network segment is any medium where multiple nodes can "hear" each other.

network IDs are expected to be automatically negotiated, though "how" isn't defined yet.

For example: an RS-485 bus would be a single "network", for example with up to 32 devices. A point to point ethernet link would be a single "network", with up to two devices.

The network IDs `0` and `65535` are reserved. All other IDs are considered valid.

The network ID `0` is defined as "the current network", typically used before a network ID is negotiated, or when sending messages within a single device. Messages with a destination network ID 0 should never be routed to other networks.

### nodes

Nodes are individual computers/entities on a network.

Node IDs are USUALLY expected to be automatically negotiated, using a technique similar to what appletalk does, basically each node will guess an ID, then shout out "is anyone using node ID x?", and see if anyone complains. If nobody complains after a couple attempts, then that node id will start being used.

Node IDs are designed to be amenable to re-use as an ID used on some local communication medium. For example: I2C addresses, RS-485/modbus addresses, SPI bus CS pin indexes, etc.

In cases where the local medium demands it (like i2c), node IDs may be statically defined, instead of automatically negotiated. These busses will not generally allow for dynamically adding new devices.

node ids `0` and `255` are reserved. All other IDs are considered valid.

The node ID `0` is defined as "the current node", typically used for messages within a single device. Messages with a destination node ID 0 should never be routed to other devices.

The node id `255` is defined as a "broadcast" message, to be processed by all nodes on a given network.

### (network, node) tuples

In general, every (network, node) tuple will be unique on an entire internet.

A single device may have multiple (network, node) tuples that "belong" to it, one per interface.

### ports

a "port id" or "socket id" identifies a single socket interface that can receive messages.

port ids are assigned by the device itself. port ids are the same across all interfaces of a device.

Port IDs `0` and `255` are reserved. All other IDs are considered valid.

The port id `0` is defined as "any port". This is explained more later.

There is currently no way for a single device to have more than 254 active ports. If you have a device that can or needs to do that, it's probably worth just using ipv4/ipv6.

todo: should we reserve any ports for special things? 1..=31 or something?

## On the wire

We definitely need some information in the header of packets.

* src address (32 bits)
* dst address (32 bits)
* sequence number (?? bits)
    * *2026-09 (wire v1): removed. It was never compared anywhere; correlation is by source port.*

We might want some additional information in packets:

* a header and/or payload checksum, separate from a wire checksum (16-64 bits)
* some kind of version identifier for packets (4-8 bits)
    * *2026-09: not per packet — `WIRE_VERSION` is reported in the well-known `DeviceInfo` (a per-packet version cannot protect the header that carries it).*
* some kind of message kind/"protocol" (e.g. endpoint req/resp, topic message) identifier, (4-8 bits)
    * todo: use for things like sessionful connections?
    * *shipped as a 2-bit frame kind (wire v1: shares a byte with class and TTL).*
* TTL counter (4-8 bits)
    * *shipped: 4 bits (wire v1).*
* QoS number (4-8 bits)
    * *2026-09 (wire v1): shipped as a 2-bit traffic class **hint** — no scheduling in the netstack, interfaces map or ignore it.*
* Something for fragmentation?
    * *still an interface concern; see the CAN work.*

### Address compression?

todo: we could use a varint to encode the entire 32-bit (network, node, port) triplet. This would allow us to send addresses (0, 0, 1..=127) in one byte, and (0, 0..=63, 1..=254) in two bytes. The worst case (65534, 254, 254) is 5 bytes.

This means in many cases of point to point and local networks, we could support up to 63 nodes with only 2 byte addresses.

### `postcard-rpc` keys?

todo: we want to use postcard-rpc `Key`s in a load-bearing way. but they are 8 bytes, and that's a lot.

Maybe we only use that in cases where we want to use port id = 0, to auto-detect the correct port id? The requestor would then see the actual port id in the response.

## The interface interface

todo: what should the code interface of interfaces look like?

Interfaces need to:

* Do some kind of self-driven operation, like negotiating node ids or discovering network ids
* incoming packet path
    * In a pipelined manner:
        * Receive incoming packets, shove them in a queue (maybe in an interrupt)
        * Move packets from the queue into the netstack (probably in non-interrupt context)
    * In a non-pipelined manner:
        * recv frame, shove it into the netstack
* outgoing packet path
    * In a pipelined manner:
        * Have a handle (given to the netstack) that allows for shoving frames into an outgoing queue
        * Move packets from the queue to the wire (maybe in interrupt context)
    * In a non-pipelined manner:
        * I dunno how to do this with the blocking mutex

### A new day

We probably need some way of plugging in different "interface + routing" impls. Off the top of my head:

* a "null route" impl
    * has zero interfaces
    * has no routing table
    * rejects all requests
    * never determines a network/node id
    * only useful as a placeholder, or for local-only comms
* a "single lite" impl
    * has exactly one interface
    * has no routing table
    * routes all outgoing packets (it has no idea if that is good or not)
    * accepts any received network id as its own
    * maybe accepts any received node id as its own?
        * if not, does whatever the absolute minimum to autonegotiate a node id
        * or, could maybe only support hardcoded node id
    * useful in point to point links, esp w/ low resource nodes
* a "single plus" interface?
    * maybe very similar to single lite, but with some kind of limited routing table, to avoid wasting send time?
    * or some more extensive net/node negotiation?
    * might not be a good differentiator here. might just be "static node addr" vs "dynamic node addr".
* a "multi lite" impl
    * has 2+ interfaces
    * has some routing table, maybe a configurable max depth?
    * will only route packets to known networks
    * needs to participate in network id selection
    * needs to participate in node id selection
* a "cadillac" impl
    * has arbitrary number of interfaces
    * has a "full world" view of the routing table
    * tries to lead net/node id selection

fun trick, some kind of "mushbrain" routing? A smarter router can say something like `(towards net,node)(rest of messsage)`. can be stacked multiple headers deep, so that a host with knowledge can bypass all nodes w/o routing info to get to destination

How do we approach network id negotiation? do we require some kind of "seed router"? Maybe networks only get autonegotiated when at least one neighbor has a defined network id?

* if a node has a single interface:
    * it does nothing, it waits to be told what the netid is
* if a node has multiple interfaces:
    * if NONE of the interfaces have a netid:
        * the node waits for one of the interfaces to be assigned a netid
    * if ONE of the interfaces have a netid:
        * periodically tell all netid-less interfaces that we have a hookup
        * do some kind of negotiation of who becomes "in charge" of assigning a netid (some time windowed assignment?)
        * pick a random network id for the new network
        * do some kind of broadcast on all interfaces like "do you know of network 1234? tell me at NET,NODE,PORT"

# Interface Manager

What about vtable?

* "is this address us?" or "what is your address?"
    * opt: "has our address changed since last ask?"
* "can you route this addr", or just "send"
    * Avoid a toctou, "don't ask to ask"?

The interface manager is independent from the interface.
