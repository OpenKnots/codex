# Codex Remote Mobile

`apps/mobile` is the foreground remote-control shell for the staged iOS rollout.

## What is here

- Tauri 2 app scaffold in `src-tauri/`
- React + Vite mobile UI in `src/`
- TanStack Query bootstrap layer and Zustand live-thread cache
- Relay-shaped gateway boundary with three runtime backends:
  - full mock data for pure UI work
  - local preview that reads `CODEX_HOME/remote` and the host UDS app-server socket
  - relay WebSocket transport when `VITE_CODEX_RELAY_URL` is configured

## Useful commands

- `pnpm --filter @openai/codex-mobile dev`
- `pnpm --filter @openai/codex-mobile test`
- `pnpm --filter @openai/codex-mobile ios:init`
- `pnpm --filter @openai/codex-mobile ios:dev`

## Relay config

- set `VITE_CODEX_RELAY_URL=wss://relay.example.test/mobile` to exercise the browser relay path instead of mock data when local preview is unavailable
- local preview still wins when `CODEX_HOME/remote` host state is present, because it is the most complete development path in this repo today

## Current scope

This package currently ships the five planned screens with a mixed development story:

- bootstrap state comes from either mock data or the local host runtime under `CODEX_HOME/remote/`
- local preview can browse real host threads and issue `turn/start`, `turn/steer`, and `turn/interrupt` over the UDS app-server socket
- local preview also maintains one active live thread stream, pushes thread runtime updates into the webview, and replays command, file-change, and permission approvals over the subscribed app-server connection
- relay/control-plane bootstrap is now subscribed separately, and local preview prefers a native Tauri bootstrap stream before falling back to polling, so host lists, device trust state, and session pairing metadata refresh in place without reloading the app shell
- the relay path now has a typed WebSocket client and gateway adapter that can hydrate bootstrap and live thread state against the same `RemoteGateway` contract used by the rest of the UI

The app still does not connect to the first-party relay or complete device pairing end to end. Local preview is a host-side developer mode, not the shipped remote transport.
