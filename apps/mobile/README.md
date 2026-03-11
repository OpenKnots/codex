# Codex Remote Mobile

`apps/mobile` is the foreground remote-control shell for the staged iOS rollout.

## What is here

- Tauri 2 app scaffold in `src-tauri/`
- React + Vite mobile UI in `src/`
- TanStack Query bootstrap layer and Zustand live-thread cache
- Relay-shaped gateway boundary with two development backends:
  - full mock data for pure UI work
  - local preview that reads `CODEX_HOME/remote` and the host UDS app-server socket

## Useful commands

- `pnpm --filter @openai/codex-mobile dev`
- `pnpm --filter @openai/codex-mobile test`
- `pnpm --filter @openai/codex-mobile ios:init`
- `pnpm --filter @openai/codex-mobile ios:dev`

## Current scope

This package currently ships the five planned screens with a mixed development story:

- bootstrap state comes from either mock data or the local host runtime under `CODEX_HOME/remote/`
- local preview can browse real host threads and issue `turn/start`, `turn/steer`, and `turn/interrupt` over the UDS app-server socket
- approval replay is still deferred because it depends on persistent server-request IDs from the future relay or a long-lived local bridge

The app still does not connect to the first-party relay or complete device pairing end to end. Local preview is a host-side developer mode, not the shipped remote transport.
