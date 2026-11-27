# Vola GUI

The native desktop front end for `vola-core`, written in TSX and rendered by
[Native Shell (nsh)](https://nsh.dev/) — Win32 on Windows, AppKit on macOS, GTK 4 on Linux,
and the other nsh targets.

It talks to the agent engine over WebSocket. Start the core, then connect:

```sh
vola-agent serve                      # ws://127.0.0.1:8765
nsh run vola-gui                      # this machine's native host
```

The **Start core** button on the connection screen launches the built `vola-agent serve`
through the app's Rust extension (it looks for `target/release`, then `target/debug`, then
`VOLA_AGENT_BIN`, then `vola-agent` on `PATH`). That default is for development inside this
repository; for a packaged app, set `VOLA_AGENT_BIN` to the installed binary, or run
`vola-agent serve` yourself and only press **Connect**. The inspector's **Settings → Stop core**
stops a server this app started.

## Layout

```text
App.tsx                      the entry (`mount` + `Window`)
src/protocol.ts              wire types, mirroring vola-core/src/protocol.rs
src/store.ts                 signals, the WebSocket client and actions
src/ui.tsx                   screens: connect, chat, composer, inspector
ext/vola-gui-native/         the app's Rust: `CoreServer` starts/stops `vola-agent serve`
nsh.json                     app metadata, theme, the path dependency on ext/
```

## Features

- Chat with streamed turn progress: rounds, context size, prompt/output tokens.
- Live tool cards (`tool_call` / `tool_result`), notices and stop/cancel.
- Sessions: new, resume, message history and compaction summaries.
- Model switching and model listing, with provider errors reported inline.
- Inspector: skill library (with the full skill text), long-term memory (add/pin/delete),
  tools, and settings (paths, provider, stats).

## Commands

```sh
nsh check vola-gui                  # TSX type check + cargo check of ext/vola-gui-native
nsh run vola-gui                    # development host with hot reload
nsh test vola-gui                   # headless UI tests
nsh package vola-gui --target windows
```

The first `nsh check` compiles the nsh crates, so it is slow; later runs reuse the cache.

## Protocol

One JSON object per WebSocket text frame. Requests carry `type` and their fields; replies
carry `type` and, when they have a payload, `data`.

```jsonc
// -> core
{ "type": "chat", "text": "list the files" }
{ "type": "cancel" }

// core ->
{ "type": "turn_started", "data": { "session_id": 3 } }
{ "type": "tool_call", "data": { "name": "list_dir", "args": "{\"path\":\".\"}" } }
{ "type": "tool_result", "data": { "name": "list_dir", "result": "...", "error": false } }
{ "type": "progress", "data": { "round": 1, "ctx_chars": 1200, "budget_chars": 32768 } }
{ "type": "turn_done", "data": { "text": "...", "rounds": 2, "tool_calls": 1 } }
```

Keep `src/protocol.ts` in sync with `vola-core/src/protocol.rs`; `PROTOCOL_VERSION` marks
incompatible changes.
