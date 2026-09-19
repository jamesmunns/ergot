# Phone number addressing

Status: WIP

This is a proposal to move from AppleTalk-style "fixed class" routing, to a technique we're referring to as "phone number addressing".

As the name suggests, this technique takes inspiration from the way that phone numbers in many locations operate:

* You can dial "local" numbers using a shorter number
* You can dial "further away" numbers using a longer number
* The shorter "local" numbers are a suffix of the longer "further away" numbers.

Phone number addressing is a form of [prefix coding](https://en.wikipedia.org/wiki/Prefix_code), and is somewhat inspired by [Classless Inter-Domain Routing (CIDR)](https://en.wikipedia.org/wiki/Classless_Inter-Domain_Routing) from TCP/IP, but diverges somewhat in usage.

As an example, within your *building*, you could dial any 4-digit number, e.g. 1234, to reach that room. Your building may have a prefix of 678, meaning that from other buildings (but still on your street), you could dial 678-1234 to reach that same room. Within your city, your street may have the prefix of 808, meaning that someone on the other side of the city could reach that same room by dialing 808-678-1234. Even if your street was reassigned a new prefix, changing 808 to 909, people on your street (and in all buildings) would be unaffected by the change when dialing inside of the "relocated" street.

The primary motivations of this scheme are:

* We can use shorter numbers when speaking "locally", reducing the amount of data sent in every packet
* Zones of entities that may "travel together" can assign and retain stable addresses autonomously, even if they are relocated and assigned a different prefix, or go from "disconnected" to "connected" with a larger network.

Currently, Phone Number Addressing is only intended to provide addressing for strictly hierarchical networks, e.g. those without loops or duplicate routes. This simplification/restriction makes it unsuitable for general "internet" style routing, but still usable for the simple networks targeted by ergot.

## Implementation Details

For the current implementation, an address space of 32 bits is planned, though this may be expanded in the future.

In *contrast* to CIDR notation, which uses the `/N` notation to state how many bits are in the PREFIX, e.g. `/24` means that there are 24 "network prefix" bits, and 8 "host identifier" bits, we will use the "PNA" (Phone Number Addressing) notation `^N` to denote the number of significant address bits. This means that an address notated `^8` would have eight address bits used, with an unused 24 bits as a prefix. This `^8` would contain up to 256 addresses (including any reserved addresses), and would take up the eight *least significant* bits of the 32-bit address space.

This shift from "left-notation" (in CIDR) to "right-notation" (in PNA) also allows for the removal of ambiguity in the future if 64-bit addresses are allowed.

Phone Number Addresses are typically written in hexidecimal format, with letters in all caps, and the PNA suffix denoting the "scope" written in decimal form. An address may be written as `3A0^10`, meaning the binary address of `0b11_1010_0000`, or in decimal form: 928. Leading zeros WITHIN the PNA scope should always be contained, e.g. `001^10`, NOT `1^10`.

A PNA notation of `^0` is invalid, as it describes an address range containing no addresses.

## Merging "Network Segments", "Node IDs", and "Ports"

In the previous system, we had:

* 16-bit network segments (NET_ID)
* 8-bit node addresses (NODE_ID)
* 8-bit ports for sockets (PORT)

In general, the `NET_ID.NODE_ID` was used to route to the destination *device*, and once a packet had reached the destination device, the `PORT` would be used to deliver to the relevant socket.

Under Phone Number Addressing, these concepts are all merged, meaning that a specific address now always refers to a socket.

## Address Allocators

In an ergot network, there may be multiple aspects of the system that act as "Address Allocators". These allocators need to keep track of two primary pieces of information:

* Their "Pool" of addresses that they can allocate FROM
* Their list of "Allocations", or address ranges within their Poll that have been exclusively assigned. These Allocations include:
    * Specific allocations, e.g. single-address assignments to sockets
    * Range allocations, e.g. for downstream devices

For example:

* A single device might allocate a range of `^7` to itself, to use as a range of socket addresses.
* A routing device may assign `^10` ranges to each of it's "downstream" connected devices

It is not currently specified whether the "Pool" of addresses are always contiguous, or a single `^N` allocation.

### Apex Entities

An entity without an "upstream" or "parent" entity above them is referred to as an "Apex" entity.

An Apex entity is able to use the entire 32-bit address space as it sees fit, as it has no upstream network that it needs to "fit within". However, Apex entities may still want to be conservative in how they use this space for two reasons:

1. The existence as an "Apex" may be temporary, if they later join a network as a "downstream" or "child" entity
2. The smaller addresses uses, the more compact they will be when transmitting.

Currently, it is suggested that Apex entity make a best guess at the maximum address space needed for all downstream entities, including its own local sockets.

### Downstream Entities

Devices that are connected to another device are referred to as "Downstream" entities. They may be directly connected to an Apex entity, or to another Downstream entity with or more "hops" to the Apex entity.

Like Apex entities, Downstream entities are expected to make a best guess at the size of address space necessary for themselves and any further downstream entitites.

### Initializing the Address Allocator

When an entity first boots, it must initialize its address allocator. The "best guess" address range is used to initialize the pool.

For example, if a device has guessed that it needs a `^10` address space, it will seed the allocator with that initial `^10` space, giving it the starting range of `000^10` to `3FF^10`, or 0 to 1023.

The pool is now ready for allocating. As a common first step, an entity will allocate a range for local sockets, usable for handing out individual addresses. It may decide that a `^7` range is suitable for this, allowing for 128 sockets (including reserved addresses), meaning addresses `00^7` to `7F^7` refer to local sockets. This ALSO means that `000^10` to `07F^10` will refer to sockets on THIS entity.

This would leave the device with addresses `080^10` to `3FF^10` eligible to allocate.

This space could be divided up in numerous different ways:

* A single `^9` could be assigned in EITHER of the ranges:
    * `100^10` to `2FF^10` (TODO: allow unaligned?)
    * `200^10` to `3FF^10`
* Three `^8`s could be assigned in the ranges:
    * `100^10` to `1FF^10`
    * `200^10` to `2FF^10`
    * `300^10` to `3FF^10`
* Seven `^7`s could be assigned in the ranges:
    * `080^10` to `0FF^10`
    * `100^10` to `17F^10`
    * `180^10` to `1FF^10`
    * `200^10` to `27F^10`
    * `280^10` to `2FF^10`
    * `300^10` to `37F^10`
    * `380^10` to `3FF^10`
* And so on

Allocators are NOT required to divide the space evenly, however all divisions must be a power of two, with a minimum size of `^1`. For example, we could end up with:

* `000^10` to `07F^10`: A `^7` for local sockets
* `080^10` to `0BF^10`: A `^6` for a downstream device
* `0C0^10` to `0FF^10`: A `^6` for a downstream device
* `100^10` to `1FF^10`: A `^8` for a downstream device
* `200^10` to `3FF^10`: A `^9` for a downstream device

## Any/All messages

In the previous system, port `0` was reserved as the "Any" port, which ergot would attempt to find a single port that matched the requested characteristics. Port `255` was reserved as the "All" port, which ergot would flood to all sockets and interfaces, except for the source interface, until the TTL was consumed.

In the new system, the 0th address in a range is reserved as the "Any/All" port. A bit in the header will be used to determine if a message is a "broadcast" message. This address is allowed to exist in the local "socket" range, as long as the 0th address is not used for a specific socket.

This "0th" address is also contextually sensitive to the "scope" of the address.

# Phone Number Addressing, by example

For example, if we had a network as follows:

* Entity A, the Apex entity, with an address scope of `^10`:
    * `000^10` to `3FF^10`
    * A's sockets assigned the scope `^7`:
        * `000^10` to `07F^10` OR `00^7` to `7F^7`
    * Entity B, a Downstream entity, with an address scope of `^8`:
        * `080^10` to `17F^10` OR `00^8` to `FF^8`
        * B's sockets assigned the full scope:
            * `00^8` to `FF^8`
    * Entity C, a Downstream entity, with an address scope of `^9`:
        * `180^10` to `37F^10` OR `000^9` to `1FF^9`
        * C's sockets assigned the scope `^7`:
            * `180^10` to `1FF^10` OR `000^9` to `07F^9` OR `00^7` to `7F^7`
        * Entity D, a Downstream entity, with an address scope of `^8`:
            * `200^10` to `2FF^10` OR `080^9` to `17F^9` OR `00^8` to `FF^8`
            * D's sockets assigned the scope `^7`:
                * `200^10` to `27F^10` OR `080^9` to `17F^9` OR `00^8` to `FF^8`
        * Entity E, a Downstream entity, with an address scope of `^6`:
            * `300^10` to `33F^10` OR `180^9` to `1BF^9` OR `00^6` to `3F^6`
    * Entity F, a Downstream entity, with an address scope of `^7`:
        * `380^10` to `3FF^10` OR `00^7` to `7F^7`

Visually:

```text
┌─────────────┬─────────────┐
│          000              │
│ Entity A    │ A's Sockets │
│ ^10           ^7          │
│ 000-3FF     │ 00-7F       │
│                           │
│          07F│             │
│             ┌─────────────┼─────────────┐
│          080│           00              │
│             │ Entity B    │ B's Sockets │
│             │ ^8            ^8          │
│             │ 00-FF       │ 00-FF       │
│             │                           │
│             │             │             │
│             │                           │
│             │             │             │
│             │                           │
│             │             │             │
│             │                           │
│             │             │             │
│          17F│           FF              │
│             ├─────────────┼─────────────┤
│          180│          000              │
│             │ Entity C    │ C's Sockets │
│             │ ^9            ^7          │
│             │ 000-1FF     │ 00-7F       │
│             │                           │
│          1FF│          07F│             │
│             │             ┌─────────────┼─────────────┐
│          200│          080│           00              │
│             │             │ Entity D    │ D's Sockets │
│             │             │ ^8            ^7          │
│             │             │ 00-FF       │ 00-7F       │
│             │             │                           │
│          27F│          0FF│           7F│             │
│             │             │             ┌─────────────┤
│          280│          100│           80│             │
│             │             │             │ Unused      │
│             │             │             │             │
│             │             │             │             │
│             │             │             │             │
│          2FF│          17F│           FF│             │
│             │             ├─────────────┼─────────────┤
│          300│          180│ Entity E  00  E's Sockets │
│             │             │ ^6          │ ^6          │
│          33F│          1BF│ 00-3F     3F  00-3F       │
│             │             ├─────────────┼─────────────┘
│          340│          1C0│             │
│             │             │ Unused      │
│          37F│          1FF│             │
│             ├─────────────┼─────────────┤
│          380│           00              │
│             │ Entity F    │ F's Sockets │
│             │ ^7            ^7          │
│             │ 00-FF       │ 00-7F       │
│             │                           │
│          3FF│           7F│             │
└─────────────┴───────────────────────────┘
```

Like:

## How does E find B, and do something with it?


1. E sends a broadcast discovery message to 00000000^32 asking "who has endpoint service $LED_OUTPUT?, with the source address of `03^6`.
2. E's Profile says "this needs to go to my parent. I am aware my base address in my parent's space is `180^9`. I will forward this message to my parent, C, and update the source address to `183^9`.
3. C delivers to it's own socket range, as well as to D, both with the source address `183^9`. Let's say neither respond.
4. C's profile knows it's base address is `180^10`. It forwards to its parent, A, and updates the source address to `303^10`.
5. A delivers to it's own socket range, as well as entities B and F. Let's say only B cares, and it is delivered to B's 09^8.
6. B receives the discovery request, and notes that it has an `$LED_OUTPUT` service on socket `0A^8`, which it does ??? to determine that `0A^8` maps to `08A^10`. It replies TO `303^10` with this info.
7. B sends it to A:303^10 is outside it's range of 080^10 to 17F^10.
8. A sends it to C, as 303^10 is in the range 180^10 to 37F^10.
9. C sends it to E, as 303^10 is in the range 300^10 to 33F^10
10. E gives it to it's own sockets, as 303^10 is in the range 300^10 to 33F^10

Now: E can send messages to B, using `303^10` as the source (it got this in the header of the reply), and `08A^10` as the destination (it got this in the body of the reply). B can reply to E using the same.

As descrived, this generally requires:

1. All devices know their own range (e.g. E's socket knows it is `03^6`)
2. The profile of a device knows it's parents range, and it's own offset in that range (e.g. E's profile knows it is 180^9, so its socket is `183^9`
3. A device has some way of looking up it's offset in a given range, e.g. B's socket `0A^8` can ask `000^10`, "hey what is my offset in the `^10` range?", and get back "you are `08A^10`".
4. Profiles are smart enough to route UP to addresses outside their range, and DOWN to addresses inside their range
5. A nice-to-have would be some way to determine the "smallest possible scope" between two addresses. e.g. What is the closest common parent between `08A^10` and `303^10`? In this case it would be `^10`, but for `303^10` and `202^10`, it's actually `^9` (doesn't need to leave C's address space)
