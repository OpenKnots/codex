# codex-app-server-client

Shared app-server client used by conversational CLI surfaces:

- `codex-exec`
- `codex-tui`

## Purpose

This crate centralizes startup and lifecycle management for app-server
connections, so CLI clients do not need to duplicate:

- app-server bootstrap and initialize handshake
- request/event transport wiring
- lifecycle orchestration around caller-provided startup identity
- graceful shutdown behavior

## Startup identity

In-process callers pass both the app-server `SessionSource` and the
initialize `client_info.name` explicitly when starting the facade.

That keeps thread metadata (for example in `thread/list` and `thread/read`)
aligned with the originating runtime without baking TUI/exec-specific policy
into the shared client layer.

## Transport model

The crate currently supports two transport modes:

- in-process runtime embedding
- unix domain socket attachment to a background `codex app-server`

The in-process path uses typed channels:

- client -> server: `ClientRequest` / `ClientNotification`
- server -> client: `InProcessServerEvent`
  - `ServerRequest`
  - `ServerNotification`
  - `LegacyNotification`

The unix domain socket path speaks line-delimited JSON-RPC and performs
the standard `initialize` handshake against the remote host runtime
before surfacing the same typed request and event facade to callers.

Typed requests still receive app-server responses through the JSON-RPC
result envelope in both modes. That is intentional: the client facade is
meant to preserve app-server semantics across transports, not introduce a
second response contract.

## Bootstrap behavior

The client facade either starts an in-process runtime or attaches to an
already-running app-server over UDS, but thread bootstrap still follows
normal app-server flow:

- caller sends `thread/start` or `thread/resume`
- app-server returns the immediate typed response
- richer session metadata may arrive later as a `SessionConfigured`
  legacy event

Surfaces such as TUI and exec may therefore need a short bootstrap
phase where they reconcile startup response data with later events.

## Backpressure and shutdown

- Queues are bounded and use `DEFAULT_IN_PROCESS_CHANNEL_CAPACITY` by default.
- Full queues return explicit overload behavior instead of unbounded growth.
- `shutdown()` performs a bounded graceful shutdown and then aborts if timeout
  is exceeded.

If the client falls behind on event consumption, the worker emits
`InProcessServerEvent::Lagged` and may reject pending server requests so
approval flows do not hang indefinitely behind a saturated queue.
