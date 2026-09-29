# moshi-lite

A thin, self-owned local gateway that serves the Moshi mobile app's
**workspace sidebar** (workspaces → tabs → panes → agents) from a running
[Herdr](https://herdr.dev) server.

It replaces the cloud-facing, closed-source `moshi-hook` daemon for this one
use case: no Moshi cloud API, no pairing, no `hostSecret`, no push, no
approvals, no usage collection, no Chat View, and no agent hooks. The phone
reaches it over your own SSH connection (Tailscale works); the gateway itself
is loopback-only.

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

## Installation

moshi-lite is a single binary, but the Moshi app looks for it under the name
`moshi-hook`: its SSH probes run `moshi-hook probe --json` and
`moshi-hook doctor --json`. Install it at `~/.local/bin/moshi-hook`.

### Prebuilt binaries

Every release publishes tarballs for Linux (x86_64, aarch64) and macOS
(arm64, x86_64) named `moshi-lite-<target>.tar.gz`, for example
`moshi-lite-x86_64-unknown-linux-gnu.tar.gz`. The repository is private, so
download with an authenticated `gh` CLI:

```bash
gh release download --repo hkalexling/moshi-lite \
  --pattern 'moshi-lite-x86_64-unknown-linux-gnu.tar.gz'
tar -xzf moshi-lite-x86_64-unknown-linux-gnu.tar.gz
install -m755 moshi-lite ~/.local/bin/moshi-hook
```

### From source

```bash
git clone https://github.com/hkalexling/moshi-lite.git
cd moshi-lite
cargo build --release
install -m755 target/release/moshi-lite ~/.local/bin/moshi-hook
```

### Run the gateway

```bash
moshi-hook serve --listen 127.0.0.1:24543
```

To keep it running, use a systemd user service:

```ini
# ~/.config/systemd/user/moshi-lite.service
[Unit]
Description=moshi-lite gateway for the Moshi app

[Service]
ExecStart=%h/.local/bin/moshi-hook serve --listen 127.0.0.1:24543
Restart=on-failure

[Install]
WantedBy=default.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now moshi-lite
```

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

## Running from a checkout

```bash
cargo run -- serve                 # 127.0.0.1:24543
cargo run -- serve --listen 127.0.0.1:24599 --poll-interval-ms 500
cargo run -- serve --herdr-socket ~/.config/herdr/herdr.sock
```

The listen address must be loopback (enforced). `$HERDR_SOCKET_PATH` overrides
the Herdr socket path. `RUST_LOG=moshi_lite=debug` enables per-request and
WebSocket-frame logging.

## Development

```bash
cargo test --all-targets           # unit + integration (mock Herdr, live WS)
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

- `probe --json` → `{"installed":true,"running":true,"gateway":true,
  "version":"<gateway version>"}` when the local gateway answers `/v1/version`.
- `doctor --json` → feature verdicts with `workspaces` **ok** (other Moshi
  features are reported as not supported by moshi-lite).

`serve` (the gateway) and `version` are implemented too. Any other command is
forwarded to `~/.local/bin/moshi-hook.official` if you keep an official binary
there; moshi-lite does not install or ship one, so without it unsupported
commands exit with an error. `install`, `pair`, and `host` are always refused
so Moshi-owned hooks and cloud pairing cannot be re-enabled accidentally.
Every invocation is logged to `~/.local/state/moshi-lite/invocations.log`.

## Releasing

- **CI** (`.github/workflows/ci.yml`) runs `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings`, and `cargo test` on pushes to
  `main` and on pull requests.
- **Release** (`.github/workflows/release.yml`) runs when `Cargo.toml` changes
  on `main`. If the version has no matching `vX.Y.Z` tag yet, it creates the
  tag, opens a draft release with generated notes, builds Linux (x86_64,
  aarch64) and macOS (arm64, x86_64) tarballs, uploads them, and publishes the
  release.

To cut a release, bump `version` in `Cargo.toml` and merge to `main`.

## Roadmap

Done:
- CLI compatibility (`probe`, `doctor`, `version`) and the `/events` handshake.
- Workspace tree, pane refresh, focus, and the diff/integrations probes.
- Installed as `~/.local/bin/moshi-hook`; the Moshi app opens the sidebar and
  jumps to workspaces.

Next:
1. `/v1/pty` so the sidebar's terminal action can attach.
2. Session-lookup resolution (`ssh-connection` / `mosh-port`) to mark the
   caller's focused branch, and multi-session Herdr support.
3. Optional trimmed agent hook extension for exact `blocked` timing and
   conversation titles (Herdr already reports agent status without it).
4. Port `context` / `cwd-list` from the official CLI if the app needs them.
