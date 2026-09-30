# CAN transport: link-layer encoding and fragmentation

Status: design, implementation in progress (same PR). Supersedes the
`canbus_experiments` branch (archived as `archive/canbus_experiments`) and
redirects #222 (CAN FD interface) and #223 (fragmentation sink). The
header-in-ID idea comes from those; this note owes them the shape.

## Goals

* One transport for classic CAN (8-byte frames) and CAN FD (64-byte frames).
  Only the maximum payload differs; the encoding, fragmentation and worker
  are shared.
* Fragmentation lives **inside the transport**, invisible to the netstack.
  `InterfaceSink::mtu()` already promises exactly this: "if the interface
  performs internal fragmentation/reassembly, this returns the max
  reassembled size". No peer service, no well-known endpoints, no acks.
* Broadcast topics work: every receiver reassembles independently (the
  J1939 BAM model).
* At-most-once is preserved: a lost frame loses the message, nothing is
  retransmitted — and nothing is delivered twice, although CAN itself
  sometimes delivers a frame twice (see *Repeated frames*).
* The CAN ID carries what every frame needs for delivery, filtering,
  arbitration and reassembly; the payload carries the ergot frame unchanged
  and nothing else, so routing, bridges and TTL behave exactly as on any
  other link, and classic CAN spends no payload byte on the link layer.

## Non-goals

* Acknowledgements, retransmission, flow control. That is a reliable socket
  kind, layered above the netstack (the reservation/ack machinery of #223
  is the seed for it).
* Reordering. Frames of one message, and messages of one class from one
  sender, are transmitted in order (see the TX contract); the receiver
  relies on it.
* Messages larger than the configured reassembled MTU (`PacketTooBig`).
* A new topology model. A CAN bus is a shared-medium segment: one arbiter
  router, node ids from the bus address claim, nothing CAN-specific.
* Header compression (see *Open questions*).

## CAN ID layout (29-bit extended)

```text
 28 27 26        19 18        11 10  9 8   6 5      0
┌─────┬────────────┬────────────┬─────┬─────┬────────┐
│prio2│ dst_node 8 │ src_node 8 │end 2│tid 3│ idx 6  │
└─────┴────────────┴────────────┴─────┴─────┴────────┘
```

* `prio` — the frame's `TrafficClass` bits: `Control → 0`, `Normal → 1`,
  `Bulk → 2`, `Background → 3`. Lower ID wins arbitration, so a Control frame
  from any node gets the bus ahead of anyone's Bulk traffic.
* `dst_node` — the **next hop** on this segment, `0xFF`
  (`BROADCAST_NODE_ID`) for all nodes. The transport does not decide it: the
  port hands every send a `LinkMeta` (#229), whose rules are a broadcast
  (`dst.port_id == 255`) to all nodes, a destination on this segment
  (`dst.network_id` equal to the segment's net, or 0 for link-local) directly,
  and anything off-segment to the segment router (`CENTRAL_NODE_ID`, 1) from
  an edge. The router itself, forwarding off-segment traffic onto the bus
  towards a downstream bridge, addresses the bridge's node: a seed route
  remembers which node on the segment requested (and refreshes) the net's
  lease (#231). Only a router that does not know the routing node falls back
  to `0xFF` and lets that node pick the frame up. Hardware filters accept
  `{own node, 0xFF}`.
* `src_node` — the **transmitting** segment node (`LinkMeta::src_node`), not
  the ergot source (which may sit behind a bridge in another net). With
  `prio` it is the reassembly key.
* `end` — `0` more frames follow, `1` last frame, `2` last frame padded (CAN
  FD only, below), `3` reserved (dropped as malformed).
* `tid` — the message number, counted per sender **per class**, wrapping at
  8. It is the same in every frame of a message.
* `idx` — the frame's position in its message, from 0. It never wraps, so a
  message spans at most 64 frames: 512 bytes of encoded ergot frame on
  classic CAN, 4096 on CAN FD. The reassembled MTU is configured below that
  (default 256 B); the sink reports `min(MTU, 64 × max_payload)`.

The ergot `kind`, class and TTL are **not** taken out of the payload: they
stay in the frame's meta byte, so a bridge forwarding a raw frame onto CAN
changes nothing about it. `prio` duplicates the class because arbitration
and reassembly need it before the payload is known.

## Payload

Every frame of a message carries the next bytes of the complete encoded
ergot frame (header, appendix, body), nothing else. All frames but the last
are full (`max_payload` bytes); the last is as long as what is left.

A frame's data length is its DLC. That is exact on classic CAN, and on CAN
FD for any length a DLC can express (0–8, 12, 16, 20, 24, 32, 48, 64).
Otherwise the last frame has to be padded to the next DLC, and marked
`end = 2`: its **last byte** is the number of data bytes in front of the
padding. Padding is at least one byte, so the length always fits without
growing the frame, and it is < 64, so one byte holds it. Classic CAN never
pads and never carries a length.

A single-frame message is simply a message whose first frame is also its
last (`idx 0`, `end ≠ 0`); nothing distinguishes it on the wire.

History: the first design put a role in the ID and a `len` (single),
`xfer_id + total_len` (first) or `xfer_id` (continuation) byte in the
payload. On classic CAN that was 1–3 of 8 bytes per frame, a single-frame
message had 7 bytes left for a ~7-byte ergot header plus body, and a length
was stored where the DLC already gave it.

## Reassembly

* A sender never interleaves two messages of one class (the worker sends a
  class's messages one after another), so the receiver assembles **at most
  one message per (src_node, prio)**. Messages of different classes from one
  sender do interleave — Control cuts into Bulk — and are kept apart by
  `prio`.
* `K` slots, each `MTU` bytes. A first frame (`idx 0`) retires anything
  still assembling for its (sender, class) — the sender has moved on — and,
  unless it is also the last, takes a slot: a free one, or else the oldest
  slot of the lowest-priority class assembling, if that class is lower than
  its own (a higher `prio` value). Otherwise it is dropped.
* A continuation (`idx > 0`) is taken only by the slot of its (sender,
  class), and only with that slot's `tid` and the next `idx`. A different
  `tid` means the slot's message lost its tail and the sender's next one lost
  its start: both are dropped (this is the splice case — without `tid` the
  new tail would complete the old message with every index looking right).
  An index that skips ahead means frames were lost: the slot is dropped.
* A slot with no progress for longer than its class timeout is dropped. The
  check runs on every frame's arrival as well as periodically, so a frame
  that arrives after the timeout is dropped rather than reviving the slot —
  the worker may have been parked in `recv()` for longer than the timeout.
  Defaults: Control 20 ms, Normal 100 ms, Bulk/Background 500 ms; all
  configurable.
* When the last frame arrives the frame is handed to the `FrameProcessor`
  exactly like a frame from any other link. A single-frame message is handed
  over straight from the CAN frame, without a slot.

### Repeated frames

A CAN receiver accepts a frame at the next-to-last bit of its end-of-frame,
a transmitter only after the last one. If that last bit is disturbed for the
transmitter, it signals an error and **retransmits a frame the receivers
already accepted**. The repeat comes right after the original from that
sender (nothing of its own can be sent in between, see the TX contract), so
it is recognised by comparing with the last frame taken:

* a continuation whose `idx` is the one just taken is dropped;
* a first frame with the same `tid` as the last first frame of its (sender,
  class), within `duplicate_ms` (20 ms), is dropped. The receiver remembers
  the last start of the 8 most recently heard (sender, class) pairs for this.

Consecutive messages of one class always differ in `tid`, so a new message
is never mistaken for a repeat; 3 bits are plenty for that. The window
bounds the other case: a sender that reboots and reuses the `tid` of its
last message.

### Why `tid` is 3 bits and there is no CRC

`tid` has three jobs: telling a repeat from a new message (any two
consecutive values differ), keeping two messages of one class from being
spliced (a false match needs 8 consecutive starts of one (sender, class) lost
within one slot's lifetime), and keeping a rebooted sender's first message
from completing its previous life's stale slot. For the last one the
counter is **seeded per boot** (`CanConfig::new(tid_seed)`, from an RNG or
a boot counter); the residual risk — equal seed, a stalled slot and a lost
start, all inside one timeout — is accepted.

CAN checks every frame with its own CRC, and `tid`, `idx` and the per-class
ordering close the ways frames of different messages could be combined. A
CRC over the reassembled message would only guard against bugs in the
reassembler itself, and is left out.

## Transmit

* The sink serializes each ergot frame, as **one queue entry**
  (`[prio, dst_node, src_node]` + frame), into a class queue (two levels:
  `Control` and everything else). A send therefore queues the whole message
  or nothing: a full queue never leaves the head of a message on the bus.
  An entry needs `MTU + 5` contiguous bytes of its queue.
* The worker fragments an entry as it transmits and assigns its `tid` then.
  Control goes first, and a Control entry queued while a Bulk message is on
  the wire goes out **between that message's frames**; within a queue,
  messages do not interleave (the reassembly rule above depends on it).
* Receiving and transmitting are two independent loops over separate
  `CanRx` / `CanTx` halves. A message waiting for the controller never stops
  the receive side, whose hardware FIFO is often a few frames deep. A
  transient receive error is logged and followed by a 1 ms back-off, so an
  adapter that reports it over and over cannot starve the transmit loop.
* **TX timeout.** A frame the controller does not accept within
  `CanConfig::tx_timeout_ms` (default 100 ms) abandons its message. Without
  it, a node alone on the bus — no ACKs, the controller retries forever —
  would never free its queues.
* **TX contract.** The next frame of a class is handed to the driver only
  after the previous one has been *accepted for transmission in order*: the
  controller must send frames in the order it got them (bxCAN: TXFP = 1, see
  below). Arbitration orders only frames that are simultaneously pending in
  hardware mailboxes by ID, and by ID the next message's first frame (`end
  0`) beats the previous message's last frame (`end 1`). Adapters for
  controllers that preempt lower-priority mailbox contents (bxCAN returns the
  evicted frame from `transmit`) must requeue the evicted frame, never drop
  it. Aborting a message means stopping; receivers time the slot out.
* Until the bus address claim completes the node transmits with its
  link-local candidate id (`bus_claim` sends from the candidate). The claim
  requests go to port 0, which the arbiter accepts from unclaimed nodes by
  design. On classic CAN they are fragmented like anything else: the
  wildcard port carries the 13-byte any/all appendix, so a request is about
  27 bytes. A candidate collision is resolved by the claim nonce, not by the
  transport.
* The receive filter needs this node's id. The RX loop follows it with
  `NetStack::wait_profile`: one wait, kept alive across frames, resolves
  when the interface's node changes, whether by a granted claim or a denied
  one that restores the previous node, with or without a transmission. No
  profile lock per frame. On every change the RX loop reprograms the
  hardware filter (`CanRx::set_node_filter`, synchronous) before it
  receives on; since `select` polls it before the TX loop, the new node's
  filter is in place before the first frame from that node goes out, and
  replies to it are not filtered away. Only first frames are filtered in
  software; a continuation can only extend a message whose first frame
  passed.

## Adapters

The transport is written against two small traits, `CanRx` (receive a
frame) and `CanTx` (send a frame, report `max_payload`: 8, or a CAN FD
length such as 64), and takes its time from `ergot::time`, whose backend
the features pick (`embassy-time` on a microcontroller, `tokio-std` on a
host, whose paused time the tests run on); the worker exists only with a
backend. Adapter errors implement `CanError::kind`, mapping the
controller's errors onto `CanErrorKind`: RX overrun, bus errors
(error-passive, bus-off with automatic recovery) and a failed frame are
recoverable, so the worker logs the kind and carries on; `Stopped` and
`Other` end `run()`. The kind is ergot's own type, so it logs under defmt
as well as `log` whatever the adapter's error type implements. `embedded-can` 0.4 covers classic only, so FD
adapters are driver-specific (esp-hal TWAI-FD, embassy-stm32 FDCAN); a
classic adapter over `embedded-can::Frame` and an FD adapter share nothing
but the traits. An in-memory bus adapter drives the tests.

### Classic CAN on bxCAN (STM32F4)

The first hardware target is an STM32F405: bxCAN, classic CAN only. Its
limits and the settings that make the transport dependable on it:

* **Receive into a software queue from the interrupt.** bxCAN has two
  receive FIFOs of 3 frames. At 1 Mbit/s an 8-byte extended frame takes
  ~130 µs, so a FIFO overruns if nothing drains it for ~0.4 ms — easily
  exceeded by a task-level `recv()` behind a busy executor or a long
  control-loop interrupt. Every overrun frame loses its whole message. The
  adapter should move frames out of the FIFOs in the RX interrupt into a
  queue of a few dozen frames (a buffered driver mode does exactly this),
  and splitting the filters across both FIFOs doubles the hardware depth.
* **Transmit in request order (TXFP = 1).** With the default ID-priority
  mode the three mailboxes can reorder frames of one class (see the TX
  contract). Request order costs at most the frames already in mailboxes
  (≤ 3, ~0.4 ms) of priority inversion for a Control frame queued behind
  Bulk ones; the worker's two-level queue keeps it from waiting longer.
* **Hardware filters `{own node, 0xFF}`** on `dst_node` (ID bits 26..19),
  extended IDs only, so the CPU and the receive queue see only this node's
  traffic: two `Mask32` banks from `CanId::dst_filter`, set in
  `CanRx::set_node_filter` (bxCAN changes filters while running, and the
  worker calls it again whenever the address claim moves the node). With
  `None`, accept all destinations and let the software filter sort them.
* **Automatic bus-off recovery (ABOM = 1)**, reported as a non-fatal error,
  so a burst of bus errors takes the node off the bus briefly instead of
  ending the worker.
* **Automatic retransmission stays on (NART = 0).** A frame lost to an error
  is repeated by the controller; the repeat side effect is handled in
  *Repeated frames*.

With those, loss on a healthy bus comes down to bus-off periods and full
reassembly slots, and the CAN CRC keeps corrupted frames out. The receive
FIFO depth is what makes a naive adapter lossy, not the bus.

## Load and latency (to be measured, not assumed)

Rough figures for a 2WD control plane on classic CAN at 1 Mbit/s:
`DriveIntent` (~24 B) is 3 frames × 50 Hz, `DriveStatus` (~52 B) 7 frames
× 50 Hz × 2 nodes, `HardwareInfo` (~120 B) 15 frames once — roughly 11 %
of the bus before bit stuffing. Average load says nothing about a Control
frame's latency during a Bulk burst; that is what the ID priority plus the
two-level TX queue are for, and it is the first thing to measure on hardware.

## Open questions

* **Header compression.** The ~7-byte ergot header partly repeats the CAN
  ID; deriving addresses on this segment from it saves 2 bytes each. It only
  turns two classic frames into one for small unicast to a known port —
  topics and wildcard requests carry the 9–13 byte any/all appendix — so it
  is not done. The variants considered and when to revisit are in the
  `TODO(header compression)` comment in `interface_manager/utils/can.rs`.
* A core API for fixed, known ports, which is what would make compression
  worth it.

## Review findings folded in

An independent review of the first implementation (2026-09-28) found four
reproducible defects, all fixed and pinned by tests: CAN FD padding on the
last fragment was rejected as overflow; off-segment unicast was addressed to
the final node instead of the segment router and filtered out by the
gateway; the transfer-id counter started at zero on every boot; and a
fragment arriving after the timeout revived the slot because expiry only ran
before `recv()`.

A second review, after the core gained `LinkMeta` (#228–#230), reworked the
sink and the worker, again pinned by tests:

* The sink learned its segment through an out-of-band
  `InterfaceSink::set_local_segment` hook that `multi_interface!` did not
  forward, so a CAN sink inside a gateway router sent from node 0 and
  addressed its own node 1. Addressing now arrives with every send as
  `LinkMeta`, and the next-hop rules live in the core.
* RX and TX shared one `select` over a single link, and the TX branch awaited
  the controller: while its mailboxes were full, nothing drained the RX FIFO.
  They are now separate halves in two loops, and a TX timeout stops a stuck
  frame from wedging the interface.
* Any adapter error ended the worker; transient ones are now logged.
* The sink queued CAN fragments one by one, so a full queue left the head of
  a transfer queued; entries are now whole ergot frames, fragmented at
  transmit time (which also lets Control cut into a Bulk transfer).

A third review (2026-09-29) led to the current frame format and:

* CAN's own retransmission of an already-accepted frame delivered a
  single-frame message twice, and a repeated continuation dropped its
  multi-frame message as an index gap. Repeats are now recognised by
  `(tid, idx)`.
* The link-layer bytes in the payload are gone (above), which also removed
  the stored lengths the DLC already gives.
* An adapter reporting a transient error in a tight loop starved the
  transmit loop; the receive loop now backs off.
* The cached own-node id dropped frames to a new address until the next
  transmission; a mismatch now asks the profile.
* Dropping a worker whose `run()` was cancelled set the interface Down
  without notifying the state observers.
* The full-queue test did not exercise a queue that was actually full.

## Test plan

Unit tests on the encoder and reassembler, and the real sink and worker over
a scripted link or an in-memory bus:

1. Every message length round-trips, classic and FD, including every
   exact-DLC boundary and FD padding; classic frames carry no link bytes; a
   malformed padding length byte is dropped; the 64-frame limit.
2. Repeats of first, middle, last and single frames deliver each message
   once, through the reassembler and through the worker; a repeat outside
   the window is a new message.
3. **Lost tail of A + lost start of B → nothing delivered** (the splice
   case); a new start retires a stale slot.
4. A Control message interleaved into a Bulk message from the same node;
   both reassemble.
5. Index gap → slot dropped; slot timeout; a late frame after the timeout;
   slot exhaustion evicts Bulk before Control; oversize messages.
6. TX queue: Control frames leave before queued Bulk frames, and cut into a
   Bulk message already on the wire; `tid` counts per class from the seed.
7. Link addressing: off-segment unicast to the segment router, on-segment
   direct, broadcast; a CAN sink inside a `multi_interface!` gateway router.
8. Worker: a full queue refuses a message whole; receiving continues while
   the transmitter is stuck; the TX timeout abandons a stuck message; a
   transient RX error does not stop the worker, and a stream of them does
   not starve transmit; the receive filter follows an address change without
   a transmission; the hardware filter is set at start and for a new node
   before any frame from it goes out; dropping a cancelled worker notifies
   state observers; FD messages through the worker.
9. End to end over an in-memory bus with several nodes.
