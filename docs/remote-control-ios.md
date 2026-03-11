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
- `apps/mobile` now contains the first Tauri 2 + React app shell, a relay-aligned gateway boundary, and focused UI tests for the five-screen mobile flow.

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

The current host state is now versioned and typed rather than ad-hoc JSON:

- `host.json` stores a persistent host ID, host name, platform, relay stub state, and a static X25519 host identity for future encrypted relay sessions.
- `devices.json` stores paired-device records plus revocation timestamps and leaves room for per-device platform and public-key metadata.
- `pairing.json` stores short-lived pairing sessions and prunes expired, used, or revoked sessions before issuing a new one.

`codex remote status` now reports host identity, socket health, relay status, paired-device count, and the number of active pairing sessions. `codex remote pair` prints the deep link, session ID, pairing code, and expiry timestamp from the host side. Native QR scanning and device-side completion are still part of the mobile app work, not the host runtime.

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

The current repository implementation reflects that split:

- `apps/mobile/src/` contains the React UI, query hooks, Zustand live-thread cache, and a relay-shaped gateway boundary that can swap between mock data and local-preview host data.
- `apps/mobile/src-tauri/` contains the Tauri shell plus the native-command boundary for capability probing, attachment-import hooks, and local-preview thread commands.
- `apps/mobile/README.md` documents local development commands for web preview, tests, and future iOS initialization.
- `VITE_CODEX_RELAY_URL` now selects a typed WebSocket relay gateway in web preview builds when local preview host state is not available.

The mobile control-plane boundary is now split from the thread stream on purpose:

- the relay connector owns session, host, pairing, and device bootstrap state
- the thread gateway owns thread list/read, live thread streaming, prompt send, interrupt, and approval replay
- the app shell subscribes to connector bootstrap updates and refreshes session/host/device query state without a full reload

The mobile shell now has a local-preview mode for development on the host machine:

- session, host, device, and pairing state come from `CODEX_HOME/remote/{host,devices,pairing}.json`
- thread list and thread read use `thread/list` and `thread/read` over `CODEX_HOME/remote/app-server.sock`
- composer sends `turn/start` for new turns and `turn/steer` when the thread has an in-progress turn
- interrupt uses `turn/interrupt` against the active turn when one exists
- opening a live thread starts one long-lived subscribed app-server connection that resumes the thread, streams thread/item status changes, captures approval server requests, and emits full thread-record snapshots into the Tauri webview
- command, file-change, and permission approvals now replay over that same subscribed connection so app-server request IDs remain valid
- the local-preview relay connector now prefers a native Tauri bootstrap stream that emits `remote-connector-snapshot` events from the host-state files and falls back to polling `read_remote_connector_snapshot` only when that stream is unavailable
- optimistic client-side revocations are still preserved over both transport paths until the host-side control plane exists

The current local-preview bridge is intentionally scoped to one active live-thread stream in the mobile shell. That keeps the native boundary small and stable while the first-party relay transport is still under development.

Alongside that local-preview path, the mobile app now has a typed relay socket boundary:

- `apps/mobile/src/remote/relayProtocol.ts` is now the single source of truth for relay request methods, params, results, and server notifications, so the socket client, gateway, and tests share one contract
- the WebSocket client uses request/response envelopes for `bootstrap/get`, `thread/read`, `thread/list`, `turn/prompt`, `turn/interrupt`, `approval/resolve`, and future relay control-plane methods
- bootstrap and live-thread notifications reuse the same `RemoteGateway` contract as mock data and local preview, so the UI does not care which transport is active
- the relay gateway injects device-native capabilities into bootstrap state so iPhone UX decisions still come from the client runtime rather than the relay
- reconnecting socket state is surfaced immediately by the gateway as a temporary `Relay reconnecting` host status, then restored to the last stable bootstrap payload when the socket reconnects
- live relay thread subscriptions now issue explicit `thread/subscribe` and `thread/unsubscribe` requests, and the gateway automatically reattaches and refreshes active threads after reconnect
- relay-delivered approval updates now drive the same approval sheet path as local preview, which keeps the UI transport-agnostic as the real first-party relay handshake is filled in

For local end-to-end iteration today:

1. Run `codex remote start` on the host.
2. Run `codex remote pair` to mint a fresh pairing session and capture the deep-link payload.
3. Launch the Tauri shell from `apps/mobile` and let it read the local-preview bridge.
4. Use `codex remote status` to verify host identity, relay stub state, and pairing-session count while the first-party relay connector is still under development.

## Non-Goals for v1

- no mobile-side repo browsing
- no local patch editing
- no mobile execution of Codex
- no transcript persistence in the relay
- no direct iPhone-to-raw-websocket deployment
