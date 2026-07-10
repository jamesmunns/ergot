# Broadcast Delivery — No Audience Is Success

Status: Implemented (`net_stack::inner::broadcast`)

This note records a deliberate change to what a **broadcast** send returns when
it reaches no one, and the reasoning behind it. The narrative model lives in the
book ([`book/_04_delivery_and_reliability.rs`](../crates/ergot/src/book/_04_delivery_and_reliability.rs));
the normative rules live in the conformance spec
([`conformance/net_stack.rs`](../crates/ergot/src/conformance/net_stack.rs), the
`Broadcast Messages` section). This note is the "why".

## The change

A topic broadcast (destination port `255`) to a stack with no local subscriber
and no interface used to return `Err(NetStackSendError::NoRoute)` and log two
errors. It now returns `Ok(())`.

Concretely: in `inner::broadcast`, the external leg now treats a profile
`NoRouteToDest` the same as `RoutingLoop` — a benign "no external recipient" —
instead of an error. A genuine delivery failure to an interface that *exists*
(e.g. `InterfaceFull`) is unchanged: it still errors and logs.

## Why

A broadcast is addressed to *everyone*, not to a single node, so the
unicast-shaped error `NoRouteToDest` ("no route to *the* destination") does not
apply to it — there is no specific destination to fail to route to. The only
meaningful outcomes are "delivered to ≥1 recipient" and "nobody was listening".

Three things make "nobody was listening = success" the right call:

1. **Consistency.** `RoutingLoop` (the only interface is the source) was already
   treated as a benign success on the same leg. "No interface at all" is the same
   situation — no external recipient — and was inconsistently treated as an error.
2. **At-most-once.** `Ok` from a send never meant "delivered" anyway — an accepted
   broadcast can be dropped one hop later, and a *partial* broadcast (some
   interfaces took it, others did not) already returned `Ok`. Singling out
   "zero recipients" as an error draws the line at "zero vs non-zero recipients",
   which is not the same as "delivered vs not" (which cannot be expressed).
3. **It is the normal case.** A device that publishes telemetry whether or not a
   host is attached broadcasts into the void constantly; that is by design, not a
   fault worth an error and two log lines on every frame.

## Impact on existing callers

Audited every broadcast call-site (ergot + demos):

- `net_stack::discovery` was the only place that *used* the error: an
  `if broadcast().is_err() { return vec![] }` fast-fail. With the change that
  short-circuit is dead, so it is removed; discovery now always listens for the
  timeout and returns an empty result when no one answers. Same result, slightly
  slower only in the degenerate "no interface at all" case.
- A few demos `.unwrap()` the broadcast and would panic when broadcasting before
  a peer connected; they now no-op silently (an improvement).
- Every other caller (defmt logging, telemetry streams, etc.) already ignored the
  result.

No external user can reliably depend on the old behaviour, since a partial
broadcast never reported "nobody got it" in the first place.

## Tests / docs touched

- `conformance/net_stack.rs`: added broadcast send cases (the table was
  unicast-only) and the normative `Broadcast Messages` prose.
- `book/_04_delivery_and_reliability.rs`: new chapter documenting the
  at-most-once delivery model, the reliability ladder, idempotency-by-design,
  liveness/`state_notify`, lossy streams, and fail-safe-by-absence.
- Doc comments on `NetStackSendError::NoRoute` and `Topics::broadcast`.
