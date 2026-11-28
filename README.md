# vola-agent

An Agent written in Rust: it speaks the OpenAI-compatible `/v1/chat/completions` tools protocol (served locally by Ollama), actually runs commands on this machine, reads and writes files, maintains its own skills, and stores sessions and long-term memory in SQLite.

The workspace has four parts:

- `vola-core` — the engine (LLM clients, SQLite memory, skills, tools) plus a WebSocket API.
- `vola-tui` — the terminal front-end: argv parsing, subcommands, the interactive REPL, one-shot `ask`, `serve` and maintenance commands.
- `vola-agent` — the thin binary that launches `vola-tui`.
- `vola-gui` — a native desktop GUI: TSX rendered by [nsh](https://nsh.dev/), talking to `vola-core` over WebSocket.

## Quick start

```bash
cargo build --release
```

Or run the built binary directly:

```bash
./target/release/vola-agent            # start the REPL
./target/release/vola-agent ask "list the files in the current directory"
./target/release/vola-agent tools      # list all tools
./target/release/vola-agent serve      # WebSocket API for the GUI (ws://127.0.0.1:8765)
```

Then, in another terminal, run the GUI (requires `nsh` 0.7+):

```bash
nsh run vola-gui
```

Prerequisite: Ollama runs locally at `http://127.0.0.1:11434` with its OpenAI-compatible `/v1` endpoints (`ollama serve`; on this machine `G:\Ollama\start-ollama.bat` works). The default model is `heretic-9b-64k:latest`, overridable with `--model` or `VOLA_AGENT_MODEL`. Paths and LLM settings live in the project-root `config.json` (see "Configuration" below). `chat` and `ask` verify that the server is reachable and the model is installed before starting; a failure is reported clearly instead of leaving an empty session behind.

## Native GUI (vola-gui)

`vola-gui` is a separate nsh project: `App.tsx` and `src/` are the TSX interface, and
`ext/vola-gui-native` is a Rust crate the GUI calls to launch the core (`CoreServer.start`).
The GUI is not part of the Cargo workspace; build and check it with nsh:

```bash
nsh run vola-gui        # dev host with hot reload (Win32 on Windows)
nsh check vola-gui      # type-check the TSX and `cargo check` the Rust extension
nsh test vola-gui       # headless UI test
nsh package vola-gui --target windows
```

Start the core first, either from a terminal (`vola-agent serve`) or with the GUI's
**Start core** button (it runs the built `vola-agent` from `target/release` or
`target/debug`, or `VOLA_AGENT_BIN`). **Connect** opens `ws://127.0.0.1:8765`; the inspector's
**Settings → Stop core** stops a server the GUI started.

The GUI supports chat with live tool cards and token/context progress, cancel/stop,
sessions (new, resume, history), model switching and listing, model-provider errors,
compaction, the skill library, long-term memory, and the tool list.

### WebSocket protocol

One JSON object per text frame. Client requests carry `type` and fields; server replies
carry `type` and, when they have one, a `data` payload:

| Direction  | Messages                                                                                                                                                                                                                                               |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| GUI → core | `status`, `sessions`, `messages`, `new_session`, `resume`, `chat`, `cancel`, `set_model`, `models`, `skills`, `skill`, `notes`, `add_note`, `delete_note`, `compress`, `tools`, `stats`                                                                |
| core → GUI | `ready`, `status`, `sessions`, `messages`, `session`, `model`, `models`, `skills`, `skill`, `notes`, `note`, `deleted`, `compressed`, `tools`, `stats`, `turn_started`, `progress`, `tool_call`, `tool_result`, `notice`, `turn_done`, `error`, `pong` |

A turn streams `turn_started` → `progress` / `tool_call` / `tool_result` / `notice` →
`turn_done`. `cancel` sets the same cancellation flag the REPL uses, so the agent stops at
its next safe point and still pairs every `tool_calls` message with a result.

## Capabilities

| Capability         | Implementation                                                                                                                                                                                                                                                                                                                                                                                                |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| System prompt      | `agent/SYSTEM.md` (change with `--prompt`/`VOLA_AGENT_PROMPT`); at runtime it is combined with runtime facts, the skill and script indexes, pinned memories and a history summary                                                                                                                                                                                                                      |
| Compaction prompt  | `agent/MEMORY.md` (`--summarizer`/`VOLA_AGENT_SUMMARIZER`; read once at startup, falls back to the built-in default when missing or blank)                                                                                                                                                                                                                                                                    |
| Tool calling       | Sends JSON Schemas for 16 tools to the model; parses `message.tool_calls`, and also accepts the XML `<tool_call>` fallback written into the content; normalizes argument names (`command`/`cmdline` → `cmd`)                                                                                                                                                                                                  |
| Command execution  | `run_cmd`: timeout (60s by default, per-call `timeout_secs`), non-zero exit codes are not fatal, long output is truncated from the head and tail, dangerous commands are blocklisted (`VOLA_AGENT_ALLOW_DANGEROUS=1` bypasses it)                                                                                                                                                                             |
| File operations    | `read_file` / `write_file` / `list_dir`; paths resolve against the working directory                                                                                                                                                                                                                                                                                                                          |
| Skills             | `agent/skills/SKILL.md` is the library manager page (conventions for adding, updating, refactoring and retiring skills); the real skills live in `agent/skills/references/*.md` (YAML frontmatter) and are the source of truth, while the DB only indexes them and counts usage; `skill_list` / `skill_view` / `skill_save` / `skill_delete` let the model query, add, rewrite and retire its own skills (new skills are written into `references/` automatically; `SKILL.md` is protected) |
| Scripts            | Executable files in the scripts directory (by default `scripts/` next to `SYSTEM.md`). Metadata lives in a `# --- vola-script ---` comment block; `INDEX.md` is generated from it. `script_list` / `script_view` / `script_save` / `script_delete` manage the library, and `script_run` picks the interpreter from the extension (`.py`, `.sh`, `.ps1`, `.js`, ...) and runs it with the same timeout/output handling as `run_cmd` |
| Memory             | SQLite `agent/data/memory.db`: `messages` stores sessions, `notes` stores long-term memory (pinnable, pinned notes are injected into the system prompt on every turn), `summaries` stores compaction summaries, and `skills` stores the index                                                                                                                                                                 |
| Context compaction | When the context exceeds `budget_chars` (default `num_ctx × 2` characters), earlier messages are folded into a ≤700-word summary written back to the DB; the cut point never splits an assistant/tool pair; `/compress` triggers it manually                                                                                                                                                                  |
| Loop protection    | Round limit (default 8), consecutive tool-error limit (default 3), and the same call repeated twice is treated as a loop                                                                                                                                                                                                                                                                                      |

## Commands

```
vola-agent [chat]                       Interactive REPL (default)
vola-agent ask "<question>"             One-shot question; process to stderr, answer to stdout
vola-agent serve [--addr host:port]     WebSocket API for vola-gui (default 127.0.0.1:8765)
vola-agent skills [delete <name>] | scripts | memory list|search|add | models | sessions | tools
vola-agent init                         Create config.json, asset/script/skill dirs and prompt skeletons
```

`memory add <content> [--pin]`, `memory search <keyword>`, `memory list`.

REPL slash commands: `/help` `/clear` `/cls` `/exit` `/new` `/sessions` `/resume` `/skills` `/skill <name>` `/skill delete <name>` `/scripts` `/model` `/models` `/ctx` `/compress` `/history [n]` `/note <fact>` `/notes [keyword]` `/memory list [keyword]` `/memory del <id>` `/tools` `/db` `/!<command>`.

Ctrl+C: at the `you ›` prompt it quits; while the agent is working it cancels the current turn and returns to the prompt (a command the agent is running has its process tree killed). The cancelled model request is abandoned rather than written to the session.

While a turn runs, a left-margin spinner shows elapsed time, the live context size (`ctx` chars and % of the compaction budget) and token usage (`tok` = prompt tokens against `num_ctx`, `out` = completion tokens) as each round's provider response reports it; the first round shows an estimate (`tok est~`) until exact usage arrives.

## Configuration

The project-root `config.json` defines paths and the local LLM. It takes precedence over environment variables and built-in defaults (explicit command-line flags can override it for a single run). `VOLA_AGENT_CONFIG` can point at another file, and `--config` takes precedence. Relative paths in `config.json` resolve against the directory holding that file (relative paths from environment variables or the command line resolve against the current directory); omitted keys fall back to default file names, trying the uppercase name first and the lowercase name second (e.g. `agent/SYSTEM.md` before `agent/system.md`). Unknown keys are ignored.

```json
{
  "assets": "agent/assets",
  "skills": "agent/skills",
  "system": "agent/SYSTEM.md",
  "memory": "agent/MEMORY.md",
  "database": "agent/data/memory.db",
  "llm": {
    "default": "ollama",
    "providers": [
      {
        "tag": "ollama",
        "type": "ollama",
        "config": {
          "url": "http://127.0.0.1:11434",
          "model": "heretic-9b-64k:latest",
          "num_ctx": 16384,
          "temperature": 0.6,
          "keep_alive": "30m",
          "request_timeout_secs": 300
        }
      },
      {
        "tag": "openai",
        "type": "open_ai_compatible_api",
        "config": {
          "url": "http://127.0.0.1:11434",
          "model": "heretic-9b-64k:latest",
          "temperature": 0.6,
          "request_timeout_secs": 300
        }
      },
      {
        "tag": "deepseek",
        "type": "deepseek",
        "config": {
          "url": "https://api.deepseek.com/v1",
          "model": "deepseek-flash",
          "temperature": 0.6,
          "request_timeout_secs": 300
        }
      }
    ]
  }
}
```

`assets` / `scripts` / `skills` / `system` / `memory` / `database` accept relative or absolute paths (`system` / `memory` point at SYSTEM.md / MEMORY.md). When `scripts` is omitted, the library defaults to `scripts/` in the same directory as `system` (so `agent/scripts` for the default layout).

`llm.default` selects the provider tag to use, and `llm.providers` lists `{ "tag", "type", "config" }` entries. Supported types:

- `ollama`: native Ollama endpoints `POST /api/chat` and `GET /api/tags`; `num_ctx`, `temperature` and `keep_alive` apply.
- `open_ai_compatible_api`: `POST {url}/v1/chat/completions` and `GET {url}/v1/models` (a `url` already ending in `/v1` is accepted). Standard OpenAI fields only.
- `deepseek`: DeepSeek's OpenAI-compatible API, default `url` `https://api.deepseek.com/v1` and model `deepseek-flash`. The key comes from `api_key`, `VOLA_AGENT_API_KEY`, then `DEEPSEEK_API_KEY`. Its `/v1/models` endpoint is treated as optional; when unavailable, `models` shows the configured model.

Provider `config` accepts `url`, `model`, `num_ctx`, `temperature`, `keep_alive`, `request_timeout_secs` and an optional `api_key` (sent as `Authorization: Bearer`; `OPENAI_API_KEY` is the fallback for `open_ai_compatible_api`). A legacy `"llm": {"ollama": {...}}` block is still accepted as a provider tagged `ollama`.

Priority: command line > `config.json` > environment variables > built-in defaults.

Environment variables (effective only when `config.json` does not set the key): `VOLA_AGENT_CONFIG ASSETS SCRIPTS SKILLS PROMPT SUMMARIZER DB HOME URL MODEL NUM_CTX TEMPERATURE KEEP_ALIVE API_KEY BUDGET CMD_TIMEOUT OUTPUT_LIMIT MAX_ROUNDS MAX_TOOL_ERRORS REQUEST_TIMEOUT VERBOSE ALLOW_DANGEROUS SHELL BASH UTC_OFFSET`, plus `OLLAMA_HOST`, `OPENAI_API_KEY` and `DEEPSEEK_API_KEY`.

## Implementation notes (gotchas)

- **Never feed `thinking`/`reasoning` back**: reasoning stays on the provider side; only `content` and tool calls go into history, otherwise the next turn contaminates itself.
- **Tool-call ids**: assistant `tool_calls` and their tool results keep a matching `tool_call_id` (required by OpenAI-compatible APIs; ids are generated when a server omits them).
- **XML fallback**: small models occasionally write `<tool_call>{"name":...}</tool_call>` into the content instead of the `tool_calls` field; the parser accepts both paths.
- **Argument name normalization**: models often send `command` / `cmdline` / `shell_cmd`; all are mapped to `cmd`.
- **Empty-reply fallback**: when the model only calls tools and gives no conclusion, the client re-asks once with tools disabled; if that is still empty, it returns the raw tool results instead of showing "(the model produced no output)".
- **Compaction cut point**: cut only before a `user` message, so an assistant `tool_calls` message is never separated from the `tool` results that follow it (Ollama would answer with a 400).
- **Windows console encoding**: on startup the REPL switches the attached console to UTF-8 and enables ANSI/VT processing (for the in-place status line); both are restored on exit. Child-process and file text is decoded as UTF-8 with a fallback to the Windows ANSI code page, so PowerShell errors and Chinese output no longer turn into `�`. PowerShell is also told to emit UTF-8 (`[Console]::OutputEncoding`), and `cmd` runs under `chcp 65001`.

## Layout

```
Cargo.toml                Cargo workspace (`vola-agent` + `vola-core` + `vola-tui`)
config.json               paths and local LLM settings
src/main.rs               thin binary entry point; delegates to `vola_tui::main`
vola-tui/src/             the terminal front-end (library crate)
  lib.rs                  entry point and module wiring
  cli.rs                  argv parsing, subcommands, memory CLI, init
  repl.rs                 interactive loop, slash commands, Ctrl+C routing
  console.rs              Windows console UTF-8 setup
vola-core/src/            the engine (library crate)
  lib.rs                  public modules
  agent.rs                agent loop: budget, compaction, tools, guards, turn events
  config.rs               config.json + env + CLI precedence
  llm.rs                  native Ollama, OpenAI-compatible, DeepSeek clients
  memory.rs               SQLite sessions, messages, notes, summaries, skills
  prompt.rs               system prompt assembly and compaction prompt
  skills.rs               skill discovery and frontmatter parsing
  scripts.rs              script library: metadata parsing, CRUD, index rendering
  tools/                  shell.rs / fsops.rs / mod.rs
  protocol.rs             GUI wire types
  server.rs               WebSocket server: one Agent per connection
  ws.rs                   minimal blocking RFC 6455 server
  interrupt.rs            cancellation shared by the REPL and the server
  util.rs                 time, truncation, decoding helpers
vola-gui/                 native GUI (nsh project, not in the Cargo workspace)
  App.tsx                 mount point
  src/protocol.ts         wire types (mirrors protocol.rs)
  src/store.ts            signals, socket and actions
  src/ui.tsx              screens and components
  ext/vola-gui-native/    Rust extension: starts `vola-agent serve`
agent/SYSTEM.md           system prompt
agent/MEMORY.md           compaction prompt (formerly PROMPT_INIT.md)
agent/skills/SKILL.md     skill library manager page
agent/skills/references/  one Markdown file per skill
agent/scripts/            reusable scripts (default: next to SYSTEM.md)
agent/scripts/INDEX.md    generated script index (do not edit)
agent/data/memory.db      sessions, notes, summaries, skill index
```

Paths are configured by `config.json`; by default all runtime assets live under `agent/`:

```
config.json                paths and LLM settings
agent/SYSTEM.md            system prompt
agent/MEMORY.md            compaction prompt (formerly PROMPT_INIT.md)
agent/assets/              static assets
agent/scripts/             reusable scripts (default: next to SYSTEM.md)
agent/skills/SKILL.md      skill library manager page
agent/skills/references/    one Markdown file per skill
agent/data/memory.db       sessions, notes, summaries, skill index
```
