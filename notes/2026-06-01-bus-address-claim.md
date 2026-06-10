# Bus Address Claim

Status: Implemented (`Router` profile, `C > 0`)

This note describes how `node_id`s are assigned on **shared-medium** network
segments — buses where many devices share one physical link and one `net_id`
(ESP-NOW, CAN FD, RS-485, simple radios). It complements
[`2025-09-04-seed-router.md`](./2025-09-04-seed-router.md), which covers the
sibling problem of assigning `net_id`s to bridged segments.

## Problem

On a point-to-point link the two roles are fixed: the controller side is
`CENTRAL_NODE_ID` (1) and the target side is `EDGE_NODE_ID` (2). A shared bus
breaks that assumption — there can be up to 254 devices on a single segment,
and each needs a unique `node_id` before it can be addressed. There is no
pre-assignment, so the `node_id` has to be negotiated at runtime.

The original addressing note ([`interfaces.md`](./interfaces.md)) sketched an
AppleTalk-style scheme: a node picks a candidate, broadcasts "is anyone using
`X`?", and adopts it if nobody objects. This note describes the scheme we
actually shipped, which differs deliberately.

## Why a central arbiter (and not AppleTalk-style probing)

Address claim is **arbitrated by the segment's router**, not resolved
peer-to-peer. A device proposes a candidate `node_id`; the router grants it,
or rejects it if it is taken. This is closer to DHCP than to AppleTalk AARP.

The deciding factor is that ergot networks are **strictly hierarchical trees**,
and every segment therefore has exactly one "upstream" device — the natural
arbiter. Given that a router is always present on the segment, central
arbitration is strictly better than distributed probing:

* **Robustness on lossy media.** AARP relies on every node reliably hearing
  every probe; a dropped probe on a radio leads to two nodes silently sharing
  an address. The router holds a single authoritative table, so there is no
  missed-probe ambiguity.
* **Fast join.** A claim is one request/response; AARP must wait out a probe
  window before it can use an address.
* **Leases for free.** The router reuses the seed-router lease machinery
  (lease + refresh + tombstone + GC), so a `node_id` is automatically
  reclaimed when a device disappears. AARP has no intrinsic lease.
* **Trivial conflict handling.** Concurrent requests are serialized by the
  router's mutex; the second one simply sees the first and returns a conflict.

**Load-bearing assumption: exactly one router-arbiter per segment.** A strict
tree guarantees this (two arbiters on one segment would be a loop, which the
model forbids). If segment-level router redundancy is ever wanted, the two
claim tables would need to be reconciled — see Open questions.

## Protocol

Two well-known endpoints, served by `Services::address_claim_handler` on a
`Router` with `C > 0`:

* `ergot/.well-known/address/claim` — `AddressClaimRequest { candidate_node_id, nonce }`
  → `Result<AddressClaimGranted, AddressClaimError>`
* `ergot/.well-known/address/refresh` — `AddressRefreshRequest { node_id, refresh_token }`
  → `Result<NodeClaimAssignment, AddressRefreshError>`

The client side is `services::bus_claim` / `bus_claim_refresh`, mirroring
`bridge_seed_assign` / `bridge_seed_refresh`.

### Claiming

1. A device boots link-local: `InterfaceState::Active { net_id: 0, node_id: candidate }`,
   where `candidate` is device-chosen (random / from a UUID / configured).
2. It sends `AddressClaimRequest` to the router at the link-local address
   `(0, CENTRAL_NODE_ID, port 0)`. Port 0 is the wildcard port, resolved to the
   claim endpoint by key.
3. The router (`request_node_claim(source_net, candidate, nonce)`):
   * rejects reserved candidates (`0`, `CENTRAL`, `EDGE`, `255`) →
     `AddressClaimError::InvalidNodeId`;
   * rejects an unknown `source_net` → `UnknownSource`;
   * if `(candidate, source_net)` already has an **active** claim:
     * same `nonce` → idempotently returns the existing assignment (handles a
       retransmitted request on a lossy bus),
     * different `nonce` → `Conflict`;
   * if it is **tombstoned** → `Conflict` (reserved during the grace period);
   * if the claim table is full → `Exhausted`;
   * otherwise grants it: stores a lease and returns
     `NodeClaimAssignment { node_id, net_id, expires_seconds, refresh_token, … }`.
4. On grant, the device sets its interface to
   `Active { net_id, node_id }` and communicates normally.

The router never substitutes a different `node_id` — it only confirms or
rejects the candidate. On `Conflict` the device must pick a new candidate and
retry (the retry policy is left to the device).

### Refreshing

A claim is a lease (initial 30 s, extendable to 120 s on refresh, minimum
remaining-time-before-refresh 62 s). Before expiry the device sends
`AddressRefreshRequest { node_id, refresh_token }`. The router verifies the
token, extends the lease, and **rotates the token** (replay protection). A
refresh that is too early returns `TooSoon`; a wrong token returns
`BadRequest`; an expired/unknown claim returns `AlreadyExpired` / `UnknownNodeId`.

### Validation

On every received frame, the router checks
`is_node_claimed(net_id, src.node_id)` and drops frames from unclaimed
node_ids — **except** frames to port 0, which may be claim requests from a
device that does not have an address yet. Validation is **scoped to the
`net_id` the frame arrived on**: a claim only validates frames on its own bus
segment, so the same `node_id` claimed on two different buses does not cross
over. `CENTRAL_NODE_ID` and `EDGE_NODE_ID` are always valid (point-to-point
compatibility). An expired-but-not-yet-GC'd claim stops validating
immediately, so a quiet bus cannot keep a stale `node_id` alive.

### Expiry, tombstones, and reuse

When a lease expires (or the via-interface is deregistered), the claim becomes
a **tombstone** that reserves the `node_id` for a grace period
(`TOMBSTONE_DURATION_SECS`, anchored to the expiration). During the grace a
returning/zombie peer with the stale `node_id` cannot collide with a freshly
granted one. After the grace, GC removes the tombstone and the `node_id`
becomes claimable again.

## net_id reuse

A closely related fix lives in the same area. The router previously allocated
`net_id`s from a strictly monotonic counter that was never reused, so a
long-running router with device churn (e.g. an ESP-NOW bridge repeatedly
disconnecting and reconnecting) would climb until the `u16` space was
"exhausted", and the only recovery was resetting the router.

`alloc_net_id` now allocates the **lowest free** `net_id`, skipping those in
use by direct slots, seed routes (including tombstones), and the upstream
interface. A `net_id` returns to the pool once its slot is removed or its
seed-route tombstone clears. Combined with tombstoning expired seed routes
(the gc fix proposed in jamesmunns/ergot#204 by @tommasoclini, included and
generalized here), this means the router can run indefinitely under device
churn without exhausting either table slots or `net_id` values, with no manual
intervention. #204 frees table slots but keeps the monotonic counter, so it
does not by itself reclaim `net_id` values; this work supersedes it.

## Shared lease lifecycle (`LeaseTable`)

Seed routes and node claims have the same lease lifecycle (active/tombstone
state, expiry→tombstone GC, token-checked refresh with extend+rotate, grace
anchored to expiration). That lifecycle is factored into a
representation-agnostic `LeaseKind` + `LeaseTable<K, X>`:

* seed routes: `LeaseTable<u16, u8>` — key = assigned `net_id`, scope =
  requesting `source_net`, extra = `via_ident`;
* node claims: `LeaseTable<u8, u64>` — key = `node_id`, scope = bus `net_id`,
  extra = `nonce`.

Each caller keeps its own lookup (a `net_id` is globally unique → looked up by
key; a `node_id` is unique only per segment → looked up by `(key, scope)`),
its own grant logic (seed allocates a fresh `net_id`; a claim takes a
device-chosen candidate with nonce arbitration), and its own error mapping.

## Threat model

Address claim is a defense against **accidental collisions**, not against
malicious peers. There is no authentication: any device can claim a free
`node_id`, and the port-0 exemption lets an unclaimed device send wildcard
frames. On a trusted bus (the intended use) this is fine; it should not be
relied on as a security boundary.

## Relationship to phone-number addressing (#145)

Phone-number addressing (jamesmunns/ergot#145) keeps the tree and replaces
`(net_id, node_id, port)` with hierarchical address *ranges*, allocated
parent→child. That parent→child allocation is the **same lease lifecycle**
this note describes — only the unit handed out changes (a single `node_id`
becomes an address range). The `K` in `LeaseTable<K, X>` is exactly that seam:
`node_id` today, an address range under PNA. The protocol shape (link-local
bootstrap → request → lease → refresh → reclaim) carries over; only `K` and
the wire format change.

## Open questions

* **Router redundancy.** The single-arbiter-per-segment assumption rules out
  two routers sharing a bus. Supporting that would need claim-table
  reconciliation or a leader election.
* **Candidate selection / retry.** How a device picks its first candidate and
  how it backs off on `Conflict` is currently left to the device; a helper
  (e.g. claim-with-retry over a range or RNG) could live in `bus_claim`.
* **Routerless meshes.** For peer meshes with no natural coordinator, an
  AARP-style distributed mode could be offered as an alternative to the
  central arbiter.
