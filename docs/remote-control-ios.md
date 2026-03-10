# Remote Control Architecture for Tauri iOS

This document describes the host-side architecture for a minimalist iOS remote console for Codex and the protocol guarantees the mobile client should rely on.

## Scope

The target product is a remote controller, not a mobile runner. The iPhone app pairs with a host machine that is already running Codex, lists live threads, resumes or steers turns, interrupts work, and resolves approval requests.

Current repository work in this area focuses on the host foundation:

- `codex app-server` supports a local Unix domain socket transport via `--listen uds:///absolute/path.sock`.
- app-server v2 exposes stable `item/commandExecution/requestApproval.availableDecisions` so mobile clients can render exact approval actions without heuristics.
- Integration coverage verifies independent connection state over the UDS transport.
- `codex remote` manages a local background host runtime with `start`, `status`, `pair`, `devices list`, `devices revoke`, and `stop`.
- `codex-app-server-client` can now attach to that host runtime over UDS, and `codex-exec` can opt into the shared daemon when `[features].remote_control = true`.

## Host Architecture

The host runtime should be layered as follows:

1. A long-running `codex app-server` process listens on a Unix domain socket and owns remote-capable threads.
2. Local stdio-only clients can bridge into that socket with `codex stdio-to-uds /absolute/path.sock`.
3. Local CLI surfaces that opt into remote control attach to that same socket instead of embedding a private in-process runtime.
4. A future remote daemon layer will maintain the outbound relay connection, device enrollment state, and push-notification wakeups without exposing an inbound public port.
5. The iOS app talks to the relay, not directly to the raw socket listener.

The Unix domain socket transport exists to give the host a supported multi-client local transport before adding the relay and mobile layers.

The current host-management commands own state under `CODEX_HOME/remote/`:

- `app-server.sock` for the local UDS listener
- `remote.pid` for the supervisor process
- `host.json` for host identity metadata
- `devices.json` for paired-device state
- `pairing.json` for active pairing sessions

`codex remote pair` currently issues the deep-link payload and pairing code from the host side. Native QR scanning and device-side completion are part of the mobile app work, not the host runtime.

To route `codex-exec` through the shared host runtime, enable the under-development feature flag:

```toml
[features]
remote_control = true
```

When that flag is enabled, `codex-exec` requires `codex remote start` to be running and will fail fast with an actionable error if `CODEX_HOME/remote/app-server.sock` is unavailable.

## Mobile Contract

The iOS client should build on top of the existing app-server v2 surface:

- `thread/list`, `thread/read`, `thread/resume`
- `turn/start`, `turn/steer`, `turn/interrupt`
- thread and item notifications
- approval request and response flows

Client rules:

- Use `Thread.name` as the thread title.
- Render command approval actions from `availableDecisions` when present.
- Treat permission responses as grant-subset payloads. Omitted permissions are denied.
- Use `scope = "session"` only when the user explicitly wants a persistent grant; otherwise default to turn-scoped approvals.

## Tauri iOS App Shape

The recommended Tauri 2 app remains intentionally small:

- sign-in and pairing
- hosts list
- thread list
- live thread view
- settings and devices

The web layer should own the event timeline, composer, and approval UI. Native plugins should be reserved for iOS-only capabilities such as Keychain storage, QR scanning, push notifications, and file or photo import.

## Non-Goals for v1

- no mobile-side repo browsing
- no local patch editing
- no mobile execution of Codex
- no transcript persistence in the relay
- no direct iPhone-to-raw-websocket deployment
