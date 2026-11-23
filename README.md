# rs-agent

An Agent written in Rust: it speaks the OpenAI-compatible `/v1/chat/completions` tools protocol (served locally by Ollama), actually runs commands on this machine, reads and writes files, maintains its own skills, and stores sessions and long-term memory in SQLite.

## Quick start

```bash
cargo build --release
```

Or run the built binary directly:

```bash
./target/release/rs-agent            # start the REPL
./target/release/rs-agent ask "list the files in the current directory"
./target/release/rs-agent tools      # list all tools
```

Prerequisite: Ollama runs locally at `http://127.0.0.1:11434` with its OpenAI-compatible `/v1` endpoints (`ollama serve`; on this machine `G:\Ollama\start-ollama.bat` works). The default model is `heretic-9b-64k:latest`, overridable with `--model` or `RS_AGENT_MODEL`. Paths and LLM settings live in the project-root `config.json` (see "Configuration" below). `chat` and `ask` verify that the server is reachable and the model is installed before starting; a failure is reported clearly instead of leaving an empty session behind.

## Capabilities

| Capability         | Implementation                                                                                                                                                                                                                                                                                                                                                                                                |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| System prompt      | `agent/SYSTEM.md` (change with `--prompt`/`RS_AGENT_PROMPT`); at runtime it is combined with environment facts, the skill index, pinned memories and a history summary                                                                                                                                                                                                                                        |
| Compaction prompt  | `agent/MEMORY.md` (`--summarizer`/`RS_AGENT_SUMMARIZER`; read once at startup, falls back to the built-in default when missing or blank)                                                                                                                                                                                                                                                                      |
| Tool calling       | Sends JSON Schemas for 10 tools to the model; parses `message.tool_calls`, and also accepts the XML `<tool_call>` fallback written into the content; normalizes argument names (`command`/`cmdline` → `cmd`)                                                                                                                                                                                                  |
| Command execution  | `run_cmd`: timeout (60s by default, per-call `timeout_secs`), non-zero exit codes are not fatal, long output is truncated from the head and tail, dangerous commands are blocklisted (`RS_AGENT_ALLOW_DANGEROUS=1` bypasses it)                                                                                                                                                                               |
| File operations    | `read_file` / `write_file` / `list_dir`; paths resolve against the working directory                                                                                                                                                                                                                                                                                                                          |
| Skills             | `agent/skills/SKILL.md` is the library manager page (conventions for adding, updating and retiring skills); the real skills live in `agent/skills/references/*.md` (YAML frontmatter) and are the source of truth, while the DB only indexes them and counts usage; `skill_list` / `skill_view` / `skill_save` let the model curate its own library (new skills are written into `references/` automatically) |
| Memory             | SQLite `agent/data/memory.db`: `messages` stores sessions, `notes` stores long-term memory (pinnable, pinned notes are injected into the system prompt on every turn), `summaries` stores compaction summaries, and `skills` stores the index                                                                                                                                                                 |
| Context compaction | When the context exceeds `budget_chars` (default `num_ctx × 2` characters), earlier messages are folded into a ≤700-word summary written back to the DB; the cut point never splits an assistant/tool pair; `/compress` triggers it manually                                                                                                                                                                  |
| Loop protection    | Round limit (default 8), consecutive tool-error limit (default 3), and the same call repeated twice is treated as a loop                                                                                                                                                                                                                                                                                      |

## Commands

```
rs-agent [chat]                       Interactive REPL (default)
rs-agent ask "<question>"             One-shot question; process to stderr, answer to stdout
rs-agent skills | memory list|search|add | models | sessions | tools
rs-agent init                         Create config.json, asset/script/skill dirs and prompt skeletons
```

`memory add <content> [--pin]`, `memory search <keyword>`, `memory list`.

REPL slash commands: `/help` `/clear` `/cls` `/exit` `/new` `/sessions` `/resume` `/skills` `/skill <name>` `/model` `/models` `/ctx` `/compress` `/history [n]` `/note <fact>` `/notes [keyword]` `/tools` `/db` `/!<command>`.

## Configuration

The project-root `config.json` defines paths and the local LLM. It takes precedence over environment variables and built-in defaults (explicit command-line flags can override it for a single run). `RS_AGENT_CONFIG` can point at another file, and `--config` takes precedence. Relative paths in `config.json` resolve against the directory holding that file (relative paths from environment variables or the command line resolve against the current directory); omitted keys fall back to default file names, trying the uppercase name first and the lowercase name second (e.g. `agent/SYSTEM.md` before `agent/system.md`). Unknown keys are ignored.

```json
{
  "assets": "agent/assets",
  "scripts": "agent/scripts",
  "skills": "agent/skills",
  "system": "agent/SYSTEM.md",
  "memory": "agent/MEMORY.md",
  "database": "agent/data/memory.db",
  "llm": {
    "default": "ollama",
    "providers": [
      {
        "tag": "ollama",
        "type": "open_ai_compatible_api",
        "config": {
          "url": "http://127.0.0.1:11434",
          "model": "heretic-9b-64k:latest",
          "num_ctx": 16384,
          "temperature": 0.6,
          "keep_alive": "30m",
          "request_timeout_secs": 300
        }
      }
    ]
  }
}
```

`assets` / `scripts` / `skills` / `system` / `memory` / `database` accept relative or absolute paths (`system` / `memory` point at SYSTEM.md / MEMORY.md).

`llm.default` selects the provider tag to use, and `llm.providers` lists `{ "tag", "type", "config" }` entries. Only `open_ai_compatible_api` is supported: the client calls `POST {url}/v1/chat/completions` and `GET {url}/v1/models` (a `url` already ending in `/v1` is accepted). Provider `config` accepts `url`, `model`, `num_ctx`, `temperature`, `keep_alive`, `request_timeout_secs` and an optional `api_key` (sent as `Authorization: Bearer`; `RS_AGENT_API_KEY` / `OPENAI_API_KEY` are fallbacks). `num_ctx` and `keep_alive` are Ollama-specific extras passed through to Ollama's OpenAI endpoint and ignored by most other servers. A legacy `"llm": {"ollama": {...}}` block is still accepted as a provider tagged `ollama`.

Priority: command line > `config.json` > environment variables > built-in defaults.

Environment variables (effective only when `config.json` does not set the key): `RS_AGENT_CONFIG ASSETS SCRIPTS SKILLS PROMPT SUMMARIZER DB HOME URL MODEL NUM_CTX TEMPERATURE KEEP_ALIVE API_KEY BUDGET CMD_TIMEOUT OUTPUT_LIMIT MAX_ROUNDS MAX_TOOL_ERRORS REQUEST_TIMEOUT VERBOSE ALLOW_DANGEROUS SHELL BASH UTC_OFFSET`, plus `OLLAMA_HOST` and `OPENAI_API_KEY`.

## Implementation notes (gotchas)

- **Never feed `thinking`/`reasoning` back**: reasoning stays on the provider side; only `content` and tool calls go into history, otherwise the next turn contaminates itself.
- **Tool-call ids**: assistant `tool_calls` and their tool results keep a matching `tool_call_id` (required by OpenAI-compatible APIs; ids are generated when a server omits them).
- **XML fallback**: small models occasionally write `<tool_call>{"name":...}</tool_call>` into the content instead of the `tool_calls` field; the parser accepts both paths.
- **Argument name normalization**: models often send `command` / `cmdline` / `shell_cmd`; all are mapped to `cmd`.
- **Empty-reply fallback**: when the model only calls tools and gives no conclusion, the client re-asks once with tools disabled; if that is still empty, it returns the raw tool results instead of showing "(the model produced no output)".
- **Compaction cut point**: cut only before a `user` message, so an assistant `tool_calls` message is never separated from the `tool` results that follow it (Ollama would answer with a 400).

## Layout

```
src/main.rs     argv parsing and subcommands
src/repl.rs     interactive loop and slash commands
src/agent.rs    main loop: compaction, tool dispatch, error counting
src/llm.rs      OpenAI-compatible HTTP (/v1/chat/completions, /v1/models)
src/memory.rs   SQLite: messages, notes, summaries, skill index
src/skills.rs   skill file discovery and frontmatter parsing
src/prompt.rs   system prompt assembly
src/tools/      shell.rs / fsops.rs / mod.rs (schemas and dispatch)
```

Paths are configured by `config.json`; by default all runtime assets live under `agent/`:

```
config.json                paths and LLM settings
agent/SYSTEM.md            system prompt
agent/MEMORY.md            compaction prompt (formerly PROMPT_INIT.md)
agent/assets/              static assets
agent/scripts/             reusable scripts
agent/skills/SKILL.md      skill library manager page
agent/skills/references/    one Markdown file per skill
agent/data/memory.db       sessions, notes, summaries, skill index
```
