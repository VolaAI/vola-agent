# AGENTS.md

Guidance for coding agents working in this repository.

## What this project is

`vola-agent` is An Agent written in Rust. It drives a local Ollama model through the
OpenAI-compatible `/v1/chat/completions` tools protocol, executes commands on the host, reads and writes files, curates its
own skill library, and keeps sessions plus long-term memory in SQLite.

- Binary crate, Rust 2021, single binary `vola-agent`.
- Dependencies: `anyhow`, `serde`/`serde_json`, `ureq` (blocking HTTP with rustls), `rusqlite`
  (bundled SQLite).
- No async runtime; all HTTP and tool work is synchronous.

## Repository layout

```
Cargo.toml / Cargo.lock   Cargo workspace: `vola-agent` (root package) + `vola-core`
config.json               paths, database and local LLM settings (source of truth)
README.md                 user-facing documentation
COMMIT_RULES.md           Conventional Commits 1.0.0; required for every commit
src/main.rs               argv parsing, subcommands (serve), memory CLI, project init
src/repl.rs               interactive REPL and slash commands, Ctrl+C routing
src/console.rs            Windows console UTF-8 setup (set/restore the code page)
vola-core/src/lib.rs      public modules of the engine crate
vola-core/src/agent.rs    agent loop: budget, compaction, dispatch, guards, TurnEvent
vola-core/src/config.rs  config.json + env + CLI precedence, path resolution, shell detection
vola-core/src/llm.rs      LLM client: native Ollama (/api/chat), OpenAI-compatible (/v1), DeepSeek
vola-core/src/memory.rs   SQLite schema and queries: sessions, messages, notes, summaries, skills
vola-core/src/prompt.rs   system prompt assembly and compaction-prompt loading/fallback
vola-core/src/skills.rs   skill discovery, YAML frontmatter parsing, save/load, index rendering
vola-core/src/tools/      mod.rs (schemas + dispatch), shell.rs, fsops.rs
vola-core/src/protocol.rs GUI wire types (JSON over WebSocket)
vola-core/src/server.rs   WebSocket API: one Agent per connection, turns on worker threads
vola-core/src/ws.rs       minimal blocking RFC 6455 server (handshake, frames, ping/pong)
vola-core/src/interrupt.rs cancellation state shared by the REPL and the server
vola-core/src/util.rs     time formatting, truncation, snippet, slugify helpers
vola-gui/                 native GUI: an nsh (Native Shell) project, not a Cargo workspace member
vola-gui/App.tsx          mount point (`mount` + `Window`)
vola-gui/src/protocol.ts  wire types mirroring vola-core/src/protocol.rs
vola-gui/src/store.ts     signals, WebSocket client, actions
vola-gui/src/ui.tsx       screens and components
vola-gui/ext/vola-gui-native/  Rust nshm crate: `CoreServer` launches `vola-agent serve`
agent/SYSTEM.md           default system prompt
agent/MEMORY.md           default compaction prompt (formerly PROMPT_INIT.md)
agent/skills/SKILL.md     skill library manager page
agent/skills/references/  one Markdown file per skill (source of truth)
agent/assets/             static assets directory
agent/scripts/            reusable scripts directory
agent/data/memory.db      SQLite database (gitignored, created on demand)
```

## Runtime prerequisites

- Local Ollama at `http://127.0.0.1:11434` with native `/api` and OpenAI-compatible `/v1` endpoints; on this machine start it with
  `G:\Ollama\start-ollama.bat` (sets `OLLAMA_HOME`, models and KV-cache env).
- Default model: `heretic-9b-64k:latest` (see `config.json`).
- `chat` and `ask` call `LlmClient::ensure_ready` before creating a session. Ollama and
  OpenAI-compatible providers must be reachable and list the configured model; DeepSeek only
  requires an API key (its `/v1/models` endpoint is optional).
- The GUI needs `nsh` 0.7+ (Native Shell). It is a separate project under `vola-gui/` and is
  not a member of the Cargo workspace.

## Build, test, run

```shell
cargo build --release
cargo test
```

On Windows, load the MSVC environment first (`vcvars64.bat`, or the recipe in
`agent/skills/references/rust-windows-build.md`); otherwise `/usr/bin/link.exe` may shadow the
MSVC linker.

The GUI is checked and run with nsh (the first `nsh check` compiles `nsh-core`, so it is slow):

```shell
nsh check vola-gui                 # TSX type check + cargo check of ext/vola-gui-native
nsh run vola-gui                   # dev host with hot reload
nsh test vola-gui                  # headless UI tests
```

```bash
./target/release/vola-agent              # REPL
./target/release/vola-agent ask "..."    # one-shot
./target/release/vola-agent serve        # WebSocket API for vola-gui (127.0.0.1:8765)
./target/release/vola-agent models       # Ollama models
./target/release/vola-agent skills       # skill library
./target/release/vola-agent sessions | tools | init
```

Keep the tree warning-free: `cargo check --all-targets` and `cargo test` must report no
warnings or errors. Do not silence warnings with blanket `#[allow(dead_code)]`; either use the
value or delete the dead code.

## Configuration model

`config.json` at the project root is the primary configuration. Keys:

| Key               | Meaning                                                                                                                                                                                                 |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `assets`          | static-asset directory                                                                                                                                                                                  |
| `scripts`         | reusable-scripts directory                                                                                                                                                                              |
| `skills`          | skill library directory                                                                                                                                                                                 |
| `system`          | system prompt file (`agent/SYSTEM.md`)                                                                                                                                                                  |
| `memory`          | compaction prompt file (`agent/MEMORY.md`)                                                                                                                                                              |
| `database`        | SQLite file (`agent/data/memory.db`)                                                                                                                                                                    |
| `llm.default`     | provider tag to use (matches `providers[].tag`)                                                                                                                                                         |
| `llm.providers[]` | `{ tag, type, config }` entries. Types: `ollama` (native), `open_ai_compatible_api`, `deepseek`. `config` has `url`, `model`, `num_ctx`, `temperature`, `keep_alive`, `request_timeout_secs`, `api_key` |

Resolution rules:

1. Discovery: `VOLA_AGENT_CONFIG`, project-root `config.json`, working-directory `config.json`,
   executable directory, then two levels above the executable (`target/release` layout).
2. Priority: command-line flags > `config.json` > environment variables > built-in defaults.
3. Paths in `config.json` are relative to the config file's directory; relative env/CLI paths
   are relative to the current directory.
4. Default file-name lookup tries uppercase first, then lowercase (`agent/SYSTEM.md` before
   `agent/system.md`). Legacy names (`PROMPT_INIT.md`, `agent/PROMPT_INIT.md`, `AGENT.md`,
   root `PROMPT.md`) remain supported as fallbacks.
5. `llm.default` must match a provider tag; a provider `type` other than
   `open_ai_compatible_api`, `ollama` or `deepseek` is a startup error. A legacy `llm.ollama`
   block is treated as a provider tagged `ollama`.
6. Unknown keys are ignored; malformed JSON or a malformed value is a hard startup error.

When adding a config key, update all of: `FileConfig` and `Config` in `vola-core/src/config.rs`,
the `load_from` mapping and defaults, `config.json`, `README.md`, and a parsing/resolution test.

## Behavior invariants

- Never feed model `thinking`/`reasoning` back into history. `ChatMessage` intentionally does not
  capture it; only `content` and `tool_calls` are stored and sent.
- Keep assistant `tool_calls` ids paired with the matching `tool` message's `tool_call_id`
  (required by OpenAI-compatible APIs); ids are generated when a server omits them.
- Accept both `message.tool_calls` and the XML `<tool_call>{...}</tool_call>` fallback in
  `content`; normalize aliased argument names (`command`/`cmdline`/`shell_cmd` → `cmd`).
- Compaction cuts only before a `user` message so an assistant `tool_calls` message is never
  separated from its following `tool` results (Ollama would return HTTP 400).
- Pinned notes are injected into the system prompt every turn; all other notes are fetched
  with `memory_search`.
- Loop guards: round limit (`max_rounds`, default 8), consecutive tool-error limit (default 3),
  and the same tool call repeated twice is rejected as a loop.
- `run_cmd` blocks dangerous commands unless `VOLA_AGENT_ALLOW_DANGEROUS=1`; output is truncated
  head/tail and command timeout defaults to 60s.
- Ctrl+C at the REPL prompt exits; while a turn runs it sets a cancellation flag that the agent
  checks between rounds and tool calls (model requests are run on a helper thread so the REPL
  never blocks), and `run_cmd` kills its process tree. Tool calls whose results were already
  requested get `[cancelled]` replies so a `tool_calls` message never loses its pairing.
- `Agent::respond_events` is the loop's only output path: it emits `TurnEvent`s (notices, tool
  calls, tool results, progress), and `respond_with` maps them back to the REPL's text lines. The
  WebSocket server forwards the same events, so CLI and GUI never drift. `completion_tokens` comes
  from the provider usage (OpenAI `usage.completion_tokens`, Ollama `eval_count`), summed per turn.
- The GUI protocol lives in `vola-core/src/protocol.rs`; keep `vola-gui/src/protocol.ts` in sync
  and bump `PROTOCOL_VERSION` on incompatible changes. One JSON object per WebSocket text frame;
  server messages carry `type` and an optional `data`.
- `server.rs` runs one `Agent` per connection. Turns go through `TURN_LOCK` because the
  cancellation flag in `interrupt` is process-wide; `cancel` only touches it while
  `TURN_ACTIVE` is set. The reader thread keeps answering `cancel` while a turn worker runs.
  A cancelled turn still gets `[cancelled]` tool replies, so `tool_calls` stays paired.
- The spinner/ANSI status line is used only when `stdout.is_terminal()`; piped input and `ask`
  stay plain text. The first round labels token counts `est~` until the provider reports usage.
- Child-process and file text is decoded as UTF-8 first, then the Windows ANSI code page (GBK), so
  PowerShell messages and Chinese output never reach the model as `�`.
- Tool-call and file relative paths resolve against `cfg.home`.
- The binary must remain runnable without a pre-existing DB: `MemoryStore::open` migrates the
  schema on first use.

## Extending the agent

- New tool: add its schema in `vola-core/src/tools/mod.rs` (`schemas()`), add its name to
  `names()`, implement it in `dispatch()`, and use `arg_str`/`arg_usize`/`arg_bool` helpers with
  clear `[Tool error]` messages. Keep results compact; the model sees them verbatim.
- New skill: put a Markdown file in `agent/skills/references/` with YAML frontmatter
  (`name`, `description`, `tags`); `skill_save` does this automatically. `SKILL.md` is the
  conventions page, not a domain skill. Files are the source of truth; the DB index is rebuilt
  at startup.
- New memory or session field: update `vola-core/src/memory.rs` migrations, row structs and
  queries, plus callers in `agent.rs`, `repl.rs`, `main.rs`, `server.rs` or `tools/mod.rs`.
- New GUI request: add the variant to `ClientMessage` in `vola-core/src/protocol.rs`, handle it in
  `server.rs`, add a `ServerMessage` reply if needed, mirror both in `vola-gui/src/protocol.ts`,
  handle the reply in `vola-gui/src/store.ts`, and surface it in `vola-gui/src/ui.tsx`.
- Keep user-facing behavior documented; update `README.md` when commands, config keys, file
  names or defaults change.

## Invoking vola-agent from another agent

- One-shot: `vola-agent ask "<prompt>"`; the answer goes to stdout, tool/progress logs to stderr.
- Interactive: `vola-agent` or `vola-agent chat` reads requests from stdin and writes replies to stdout.
- Useful flags: `--model`, `--url`, `--num-ctx`, `--config <file>`, `--session <id>`, `--verbose`,
  `--allow-dangerous`.
- Exit code is non-zero on startup or runtime failure; errors are printed to stderr with an
  actionable message.
- `vola-agent sessions` lists saved sessions; `vola-agent memory list|search|add` manages long-term
  notes; `vola-agent skills` lists the skill library.
- `vola-agent serve [--addr host:port]` exposes the GUI's WebSocket API (default
  `127.0.0.1:8765`, or `VOLA_AGENT_SERVE_ADDR`); `--port <n>` is shorthand for
  `127.0.0.1:<n>`.

## Workspace

You should put all temporary files (tools scripts or other temp files) into folder temp, do not affecting the source code directory and repository.

`vola-gui/` is a separate nsh project with its own toolchain and `target/`; do not add it to the
Cargo workspace. Its generated `.nsh/` directory is gitignored.

## Commit rules

All commits must follow `COMMIT_RULES.md` (Conventional Commits 1.0.0):

```
<type>[(scope)][!]: <description>

[optional body]

[optional footer(s)]
```

- Use `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`,
  `revert`, or another conventional type.
- Add `!` after the type/scope or a `BREAKING CHANGE:` footer for breaking changes.
- `BREAKING CHANGE` must be uppercase.
- Keep the description short and imperative; put details and rationale in the body.
- Do not commit unless the user explicitly asks, and do not rewrite history or force-push
  without permission.

Note: this working copy currently has no `.git` directory. Initialize or restore the repository
before committing, and re-check `git status` because file names in `agent/` are uppercase and
some platforms are case-insensitive.
