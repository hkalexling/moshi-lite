# moshi-lite

A thin, self-owned local gateway that serves the Moshi mobile app's
**workspace sidebar** (workspaces → tabs → panes → agents) from a running
[Herdr](https://github.com/herdrdev/herdr) server.

It replaces the cloud-facing, closed-source `moshi-hook` daemon for this one
use case: no Moshi API, no pairing, no `hostSecret`, no agent hooks, no push,
no approvals. The phone reaches it over your own SSH connection (Tailscale
works), where the loopback gateway is the only transport.

## Scope

Implemented endpoints (Moshi host-gateway contract `protocolVersion: 1`):

| Endpoint | Purpose |
| --- | --- |
| `GET /v1/version` | Capability handshake (`events.watch.workspaces`, `events.doctor`, `workspaces.live-session`) |
| `GET /v1/muxes` | Herdr mux list for the app's session picker |
| `GET /v1/workspaces` | Normalized workspace tree (Herdr snapshot) |
| `GET /v1/workspaces/panes` | Inline pane refresh for one tab |
| `POST /v1/workspaces/focus` | Focus workspace/tab/pane/agent in Herdr |
| `GET /v1/diff/start` | Diff-support probe: answers 405 like the official daemon |
| `GET /v1/integrations` | Empty hook list (moshi-lite installs no agent hooks) |
| `GET /events` | WebSocket: gateway hello, watch, doctor, workspaces/context frames |

Non-goals for now: cloud API/WS, pairing, notifications, usage, approvals,
transcripts/Chat View, diff viewer, web client, hook installation.

## Architecture

```
Moshi app ──SSH/Tailscale──▶ 127.0.0.1:24543 (moshi-lite)
                                   │  axum: HTTP + /events WS
                                   │  poller (default 1s)
                                   ▼
                            ~/.config/herdr/herdr.sock
                            {id, method, params} JSON lines
                            session.snapshot / *.focus
```

- `src/herdr/` — Herdr adapter. `SocketBackend` speaks the JSON-line socket
  protocol directly (one connection per request). The `HerdrBackend` trait is
  what tests mock.
- `src/mapping.rs` — Herdr snapshot → Moshi tree mapping, plus observed
  `statusChangedAt` tracking (`approx` marks a first sighting).
- `src/state.rs` — shared state + background poller; only changes are pushed.
- `src/gateway.rs` — the HTTP/WS surface.

## How the app connects

The app SSHes in and runs the CLI probes first (`probe --json`, `doctor
--json`), then talks to the gateway over the SSH-forwarded port:

1. `GET /v1/version` — capabilities. `events.watch.workspaces`,
   `workspaces.live-session` and `events.doctor` are what unlock the sidebar
   sockets.
2. `GET /v1/diff/start` — a probe that expects `405` (the endpoint is a POST);
   answering `404` makes the app abandon the rest of the session setup.
3. `GET /events?doctor=refresh` — the gateway pushes `{"doctor": …}`
   immediately after the hello frame.
4. `GET /events?session=ssh&sshConnection=…` — the session-scoped sidebar
   socket. The app sends `{"watch":{"workspaces":true}}`; the gateway answers
   with `{"watching":{…},"doctor":…}` followed by `{"workspaces":…}` frames
   and, when context is watched, `{"context":…}` frames. Session-lookup
   parameters are accepted and ignored: the tree is the loopback Herdr
   resolution (the `default` session).

Taps return as `POST /v1/workspaces/focus` and are applied through the Herdr
socket API (`pane.focus`, with an `agent.focus` fallback).

## Running

```bash
cargo run -- serve                 # 127.0.0.1:24543
cargo run -- serve --listen 127.0.0.1:24599 --poll-interval-ms 500
cargo run -- serve --herdr-socket ~/.config/herdr/herdr.sock
```

The listen address must be loopback (enforced). `$HERDR_SOCKET_PATH` overrides
the Herdr socket path. `RUST_LOG=moshi_lite=debug` enables per-request and
WebSocket-frame logging.

## Testing

```bash
cargo test                         # unit + integration (mock Herdr, live WS)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Tests never touch a real Herdr server: `MockBackend` serves a fixture captured
from Herdr 0.9.1 (`tests/fixtures/herdr-snapshot.json`), and socket tests spin
up a throwaway Unix listener.

## CLI compatibility

The Moshi app SSHes in and runs `moshi-hook probe --json` and
`moshi-hook doctor --json` to decide whether the host is usable. moshi-lite
implements both:

- `probe --json` -> `{"installed":true,"running":true,"gateway":true,
  "version":"0.1.0"}` when the local gateway answers `/v1/version`.
- `doctor --json` -> feature verdicts with `workspaces` **ok** (other features
  are reported as not supported by moshi-lite).

Anything else (for example `cwd-list`, `context`, `servers`) is forwarded to
the official binary kept at `~/.local/bin/moshi-hook.official`, except
`install` / `pair` / `host`, which are refused so Moshi-owned hooks and cloud
pairing cannot be re-enabled accidentally. Every invocation is logged to
`~/.local/state/moshi-lite/invocations.log` for debugging.

Install the CLI as the app expects:

```bash
cargo build --release
cp target/release/moshi-lite ~/.local/bin/moshi-hook
```

## Roadmap

Done:
- CLI compatibility (`probe`, `doctor`) and the `/events` handshake above.
- Installed as `~/.local/bin/moshi-hook`; the official binary stays beside it
  (`moshi-hook.official`) as the fallback for `context` / `cwd-list` /
  `servers`.

Next:
1. `/v1/pty` so the sidebar's terminal action can attach.
2. Session-lookup resolution (`ssh-connection` / `mosh-port`) to mark the
   caller's focused branch, and multi-session mux support.
3. Optional agent hook extension for exact `blocked` timing and conversation
   titles (Herdr already reports agent status without it).
4. Port `context` / `cwd-list` and drop the official-binary fallback.
