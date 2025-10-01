# Link layer

This expands and is based on the [Phone Number Addressing](./0145-phone-number-addressing.md) and
proposes a conceptual separation between the link layer and the network layer
for the purpose of allowing non-hierarchical networks that involve things like
loops and redundancies as well as multicast and broadcast addresses.

## Terms

### Link

A link is the "surface" between a device and a cable that the device is
connected to.

It has an [Address Allocator](#address-allocator) and/or a [Routing Table](#routing-table),
which might be shared with the other links of the device depending on the
setup.

### Address Allocator

Every device that has at least one child device, or which has the ability to
hotplug child devices is **required** have an address allocator!

As the [Phone Number Addressing](./0145-phone-number-addressing.md) proposal describes, an address allocator has a pool of
one or more address ranges it can allocate from.

An allocation consist of a contiguos range of one or more addresses with a
length of a power of 2.
Not all allocations that an address allocator gives out have to be the same size.

As an optional feature, the allocator can negotiate with the device for the 
the allocation to be "unaligned", which means the address range doesn't have to
be aligned to its size, but can be offset by a number where `offset < allocation length`.

TODO: Decide on the specifics of this. See [Open Questions: Unaligned Addresses](#unaligned-addresses-and-translation-into-local-addresses)

As an optional feature, the allocator can *dynamically* request for additional
address ranges for its allocation pool.
For example when a child device requests for an address range that the allocator
doesn't have space for, the allocator is allowed to request that range from
*its* parent instead of returning an allocation error.

### Routing Table

Every device that has more than one link is **required** to have a routing
table.

The routing table has the responsibility to decide through which link(s) an
outgoing packet should be sent.

Considering that `ergot`'s networking shape is *usually* tree shaped, this
decision can be thought of as deciding on which of the child links, or the
packet is not for one of its children, to send the packet to its parent device.

As an optional feature the routing table can allow for multi/broadcast
addresses, which are otherwise normal addresses, which can be configured to
correspond to multiple devices, which means that the packet might need to be
sent to multiple links.

#### Implementation Note

While Address Allocator and Routing Table are described as two different
concepts, they are likely best implemented as one piece.

If the address allocator allocates an address range, it is clear from context to which
link that address should be routed to.

## Link Layer Operations

Most Address configurations are message driven based on a well defined protocol
(described below), but depending on the address allocation scheme, a subnet
might define its own, private protocol, or even hardcode the local addresses.

For example when a group of devices "travel" together and can be
hotplugged with a larger network, the local addresses should
stay the same, even the global network prefix changes.

### Messages

Note that this protocol is work-in-progress and is mostly here for example
reasons for what the eventual implementation could look like.

#### Alloc Addresses

`Endpoint` from child to parent.

TODO: How should a child get the address of the parent to request its first
      address range to be able to request its first address?

```rust
struct Request {
    /// Array of allocations that are being requested.
    /// The allocations are created atomically, that means if one of the
    /// allocations fails, none of them are actually applied.
    allocs: Vec<Alloc>,
}

struct Alloc {
    len: u8,
    flags: Flags,
}

bitflags! {
    pub struct Flags: u8 {
        // whether the allocated address should be allowed to be subscribed to by
        // other devices.
        // For now multicast messages are assumed to be sent to all subscribed devices.
        const ALLOW_MULTICAST = 1 << 0;
        const ALLOW_UNALIGNED = 1 << 1;
    }
}

type Response = Result<Success,Error>;

struct Sucess {
    allocs: Vec<AllocInfo>
}

struct AllocInfo {
    /// first address of the successful allocation
    address: Varint,
    /// length of the allocation, can be bigger than what was requested at the
    /// discretion of the allocator.
    len: u8,
}

struct Error {
    // TODO
}
```

#### Subscribe Multicast

`Endpoint` from child to parent.

```rust
struct Request {
    /// Address to subscribe to.
    address: ErgotAddress,
}

type Response = Result<Success,Error>;

struct Success();

struct Error {
    // TODO
}
```

#### Publish new Prefix

`Topic` sent from parent to all children when plugging into a new network.

Mostly for cases where a group of devices travels together.

```rust
struct Topic {
  address: ErgotAddress,
}
```

## Example Scenarios/Patterns

### Sibling Routers

Also known as "Virtual Parent".

In the simplest version this pattern consist of three layers:

- The parent layer

  This represents the connection to the wider network
- The sibling routers

  1 to 4 devices that are interconnected with each other, are in the same subnet with hardcoded prefixes from `0^2` to `2^2`.
  They know which sibling is behind which link and can automatically route
  packets to the correct sibling based on the prefix.
- The child layer

  Each sibling router has its own separate subnet, which all have the same size.

When first connecting with the parent layer, one of the parents (chosen
deterministically) requests an address range that is `^2` + the size of its
subnet. This address range is then published to the other siblings.

When the device `1.7^2.4` wants to send a message to `3.4^2.4`, it first sends
the message to its local router `1^2`, which then forwards that message to the router `3^2`, which then can send it to the target device at `3.4^2.4`.

TODO: Document more pattern
- Fixed Device Group
- Leaf Allocator
- Apex Entity

In general the patterns come down to the fact that if you want to have loops
between devices, they have to be on the same "layer" of the network and need to
be either statically segment the network between each other, or dynamically
inform each other when a new address range was allocated by one of the devices.

## Open Questions

### Unaligned Addresses and Translation into local addresses

I see three ways unaligned addresses could be implemented:

- Calculate the global address from local address + offset: which would mean that the local address
  For example the local address `23^8` in the allocation `3a.22^16` would be
  translated into a global address `3a.45^16`.
  - Pro: `0^8` is **always** the first address of the allocation and the local
         addresses are ordered in the same order as the global addresses.
  - Con: The devices have to do a bit of work to translate between local and
         global addresses.

- "Wrap" the local address inside of the allocation.
  In that case the local address `20^8` in the allocation `3a.22^16` would
  correspond to the global address `3b.20^16`, while the local address `25^8`
  would translate to `3a.25^16`.
  - Pro: The translation between local and global addresses is **much** simpler
         and is easier to reason about when thinking in terms of the bit pattern.
  - Con: The order of the device addresses in the local and the global scope
         *differ*.

- Don't pre-define a mapping between the local and the global address and let
  the user define their own mapping method. 
  This problem is *per definition* local to the subnet we are in, so even when
  mixing methods inside of different parts of the network that should be fine.
  - Pro \
    The user can choose their own optimal strategy.
  - Con \
    Having to potentially understand multiple different solutions to this
    is a slight increase in terms of complexity, but as long as the `ergot`
    implementation chooses a sane default, that shouldn't really have a big
    impact.
