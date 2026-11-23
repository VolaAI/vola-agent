# AGENTS.md

Guidance for coding agents working in this repository.

## What this project is

`rs-agent` is An Agent written in Rust. It drives a local Ollama model through the
OpenAI-compatible `/v1/chat/completions` tools protocol, executes commands on the host, reads and writes files, curates its
own skill library, and keeps sessions plus long-term memory in SQLite.

- Binary crate, Rust 2021, single binary `rs-agent`.
- Dependencies: `anyhow`, `serde`/`serde_json`, `ureq` (blocking HTTP with rustls), `rusqlite`
  (bundled SQLite).
- No async runtime; all HTTP and tool work is synchronous.

## Repository layout

```
Cargo.toml / Cargo.lock   crate metadata and dependencies
config.json               paths, database and local LLM settings (source of truth)
README.md                 user-facing documentation
COMMIT_RULES.md           Conventional Commits 1.0.0; required for every commit
src/main.rs               argv parsing, subcommands, memory CLI, project init
src/repl.rs               interactive REPL and slash commands
src/agent.rs              agent loop: context budget, compaction, tool dispatch, loop guards
src/config.rs             config.json + env + CLI precedence, path resolution, shell detection
src/llm.rs                LLM client: native Ollama (/api/chat), OpenAI-compatible (/v1), DeepSeek
src/memory.rs             SQLite schema and queries: sessions, messages, notes, summaries, skills
src/prompt.rs             system prompt assembly and compaction-prompt loading/fallback
src/skills.rs             skill discovery, YAML frontmatter parsing, save/load, index rendering
src/tools/mod.rs          tool JSON schemas and dispatch (ToolEnv)
src/tools/shell.rs        run_cmd: timeout, process-tree kill, output truncation, danger blocklist
src/tools/fsops.rs        read_file / write_file / list_dir, resolved against cfg.home
src/util.rs               time formatting, truncation, snippet, slugify helpers
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

## Build, test, run

```shell
cargo build --release
cargo test
```

On Windows, load the MSVC environment first (`vcvars64.bat`, or the recipe in
`agent/skills/references/rust-windows-build.md`); otherwise `/usr/bin/link.exe` may shadow the
MSVC linker.

```bash
./target/release/rs-agent              # REPL
./target/release/rs-agent ask "..."    # one-shot
./target/release/rs-agent models       # Ollama models
./target/release/rs-agent skills       # skill library
./target/release/rs-agent sessions | tools | init
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

1. Discovery: `RS_AGENT_CONFIG`, project-root `config.json`, working-directory `config.json`,
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

When adding a config key, update all of: `FileConfig` and `Config` in `src/config.rs`, the
`load_from` mapping and defaults, `config.json`, `README.md`, and a parsing/resolution test.

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
- `run_cmd` blocks dangerous commands unless `RS_AGENT_ALLOW_DANGEROUS=1`; output is truncated
  head/tail and command timeout defaults to 60s.
- Tool-call and file relative paths resolve against `cfg.home`.
- The binary must remain runnable without a pre-existing DB: `MemoryStore::open` migrates the
  schema on first use.

## Extending the agent

- New tool: add its schema in `src/tools/mod.rs` (`schemas()`), add its name to `names()`,
  implement it in `dispatch()`, and use `arg_str`/`arg_usize`/`arg_bool` helpers with clear
  `[Tool error]` messages. Keep results compact; the model sees them verbatim.
- New skill: put a Markdown file in `agent/skills/references/` with YAML frontmatter
  (`name`, `description`, `tags`); `skill_save` does this automatically. `SKILL.md` is the
  conventions page, not a domain skill. Files are the source of truth; the DB index is rebuilt
  at startup.
- New memory or session field: update `src/memory.rs` migrations, row structs and queries, plus
  callers in `agent.rs`, `repl.rs`, `main.rs` or `tools/mod.rs`.
- Keep user-facing behavior documented; update `README.md` when commands, config keys, file
  names or defaults change.

## Invoking rs-agent from another agent

- One-shot: `rs-agent ask "<prompt>"`; the answer goes to stdout, tool/progress logs to stderr.
- Interactive: `rs-agent` or `rs-agent chat` reads requests from stdin and writes replies to stdout.
- Useful flags: `--model`, `--url`, `--num-ctx`, `--config <file>`, `--session <id>`, `--verbose`,
  `--allow-dangerous`.
- Exit code is non-zero on startup or runtime failure; errors are printed to stderr with an
  actionable message.
- `rs-agent sessions` lists saved sessions; `rs-agent memory list|search|add` manages long-term
  notes; `rs-agent skills` lists the skill library.

## Workspace

You should put all temporary files (tools scripts or other temp files) into folder temp, do not affecting the source code directory and repository.

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
