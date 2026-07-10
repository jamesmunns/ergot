//! # Delivery and Reliability
//!
//! Ergot makes a deliberate, narrow promise about delivery, and leaves
//! everything stronger to the application. Understanding exactly what a `send`
//! does — and does not — guarantee is the key to building something reliable on
//! top of it.
//!
//! ## Ergot is at-most-once
//!
//! A send in Ergot is **at-most-once**: the message is delivered immediately, or
//! it is dropped. There is no automatic retransmission, no acknowledgement, no
//! background worker holding messages for later, and no built-in priority or
//! quality-of-service. This follows directly from the "drop, don't block" choice
//! described in [Major Concepts](super::_02_major_concepts): buffers are bounded
//! and there is no hidden place to stash a deferred message, so when a
//! destination is full or absent the message is dropped rather than queued
//! indefinitely.
//!
//! This is a feature, not a limitation to apologize for: bounded buffers,
//! demand-driven sending, and no hidden retransmission state are what make Ergot
//! suitable for the smallest devices. It does mean that **reliability is the
//! application's responsibility**, built from the primitives below.
//!
//! ## What a `send` result tells you
//!
//! The `Result` returned by a send describes the *local* send attempt, never the
//! eventual fate of the message:
//!
//! * `Ok(())` means the message was accepted for immediate, best-effort delivery
//!   — handed to a matching local socket and/or enqueued on an outgoing
//!   interface. It does **not** mean the message was received or processed by the
//!   peer. An accepted message can still be dropped downstream: a full queue on a
//!   later hop, a stale frame shed under backpressure, or a peer that has gone
//!   away.
//! * `Err(..)` means the send could not even begin — for a unicast, there was no
//!   route to the requested destination and no matching local socket.
//!
//! Because `Ok` already carries no delivery guarantee, the meaningful question is
//! never "did anyone receive this?" (you cannot learn that from the return value)
//! but "was this message accepted for sending?".
//!
//! ### Broadcast is best-effort with no single destination
//!
//! A broadcast (topic message, destination port `255`) is addressed to
//! *everyone*, not to a specific node. It is delivered to every matching local
//! socket and flooded outward on every interface. Two consequences follow:
//!
//! * Reaching **at least one** recipient is success; a partial broadcast (some
//!   interfaces took it, others did not) still returns `Ok`.
//! * Reaching **no one** — no local subscriber and no interface to flood to — is
//!   a successful no-op, not an error. Under the at-most-once model, "nobody is
//!   listening right now" is the same expected, lossy outcome as a message
//!   dropped one hop later. A device that publishes telemetry whether or not a
//!   host is attached is the normal case, and it is not a failure.
//!
//! A genuine failure to an interface that *does* exist (for example, its outgoing
//! queue is full) is still reported as an error.
//!
//! ## The delivery-semantics ladder
//!
//! Stronger guarantees are built on top of at-most-once, rung by rung:
//!
//! * **At-most-once** — send and forget. Ergot's raw behaviour. The message may
//!   be lost.
//! * **At-least-once** — retry until acknowledged. This is just "wrap the request
//!   in a timeout and a bounded number of retries." Because a message may now be
//!   delivered more than once, the receiver must tolerate duplicates — that is,
//!   the operation must be **idempotent**.
//! * **Effectively-once** — at-least-once delivery plus de-duplication on the
//!   receiver (an idempotency key / request id the server remembers). True
//!   exactly-once is not achievable over an unreliable transport; this is the
//!   practical substitute.
//!
//! Note that the `seq_no` field in the header does **not** give you
//! de-duplication today: an application-level retry is a fresh send with a fresh
//! `seq_no`, so the receiver cannot use it to recognise a duplicate.
//! Effectively-once therefore needs an application-level request id carried in the
//! message body.
//!
//! ## Idempotency by design
//!
//! Because at-least-once retry is the workhorse of reliability, the cheapest way
//! to make a system robust is to make its operations idempotent from the start.
//! The most effective discipline is to prefer **declarative, level-triggered**
//! commands over **imperative, edge-triggered** ones:
//!
//! * Declarative / level-triggered: an absolute desired state — `set mode = Run`,
//!   `set current limit = 10 A`. Re-applying it changes nothing, so retrying is
//!   always safe, and absence of a fresh command fails toward a known state.
//! * Imperative / edge-triggered: a delta or one-shot action — `toggle`, `step
//!   +5`, `start calibration`. Re-applying it does something *again*, so a
//!   duplicate from a retry is a bug.
//!
//! This is the same distinction as HTTP `PUT` versus `POST`, or a reconcile loop
//! that drives toward a target rather than emitting edits. Declarative setpoints
//! are idempotent by construction and fail safe naturally. For the genuine
//! actions that remain, either make them no-ops when already done, or de-duplicate
//! them by request id.
//!
//! ## Liveness and interface state
//!
//! Ergot will not ping or retry for you, but it provides two optional hooks on a
//! transport's receive worker that the application-level reliability loop is built
//! from. Both default to `None`.
//!
//! * `LivenessConfig { timeout_ms }` is a **receive-side watchdog**. The worker
//!   arms a timer that is reset on every received frame; if nothing arrives within
//!   `timeout_ms` it declares the link dead and transitions the interface state.
//!   For a connectionless transport (UDP) the interface goes `Down` and the worker
//!   exits, so you re-register for the next session; for a COBS stream (TCP,
//!   serial, RTT) it goes `Inactive` and the workers keep running and recover when
//!   frames resume, while a real transport error goes `Down`. With no liveness
//!   configured, silence alone never changes the interface state.
//! * A `state_notify` wait-queue is woken on **every interface state change**:
//!   `Inactive → Active` on the first frame, `→ Inactive`/`Down` on a liveness
//!   timeout, and on (de)registration. You `wait()` on it and react.
//!
//! Each interface carries an `InterfaceState`:
//!
//! * `Active { net_id, node_id }` — up and addressable.
//! * `Inactive` — temporarily not delivering (a COBS-stream liveness timeout); the
//!   workers keep running and recover when frames resume.
//! * `Down` — gone (a UDP liveness timeout, or a transport error); for UDP the
//!   worker exits and you re-register.
//!
//! The canonical reliability loop is therefore: register the interface with
//! `liveness` and `state_notify`, `wait()` on the notify, and on each transition
//! react — on `Active` mark the link up (and run any handshake), on `Inactive`
//! wait a recovery window, on `Down` (or once the recovery window expires) tear
//! down and reconnect.
//!
//! ## Backpressure, lossiness, and the absence of QoS
//!
//! There is no priority within an interface: a high-rate telemetry stream and a
//! command response share one bounded outgoing queue. So the answer to "what
//! happens when the buffer fills and an important packet is lost?" is not a QoS
//! policy — it is to make the *high-volume, low-value* traffic the part that is
//! shed:
//!
//! * Make bulk streams (telemetry, logs) **lossy and rate-limited**: drop samples
//!   at the source when the queue is full, batch several samples per message, and
//!   make streaming opt-in (the consumer asks for a rate) so nothing is produced
//!   when no one is listening.
//! * Let the **low-volume, important** traffic (commands and their responses) ride
//!   the timeout-and-retry path from the ladder above.
//!
//! Combined with a transmit worker that drops stale frames and bounds the time
//! spent on any single frame, this keeps a slow or congested link from
//! head-of-line-blocking the command path: under pressure, telemetry is what is
//! lost, not commands.
//!
//! ## Safety-critical systems: fail safe by absence
//!
//! The corollary of at-most-once is sharp for anything that can hurt someone: a
//! "stop" command is **not** a safety guarantee, because it can be lost. Safety
//! must instead be **fail-safe by absence** — a deadman. The actuator runs only
//! while it receives continuous, positive affirmation, and the *loss* of that
//! affirmation drives it to a safe state. The `liveness` → `state_notify` chain is
//! exactly the "the controller went away" detector for this pattern.
//!
//! A subtlety worth internalizing: a link-loss gate that lives in the async
//! executor only fires while the executor is running. The strongest layer is a
//! command-staleness deadman that does not depend on the executor at all — for
//! example, checked in the control interrupt and backed by a hardware watchdog —
//! so that even a stalled executor still fails safe.
