---
name: ollama-tool-calling
description: Call tools through Ollama's OpenAI-compatible /v1/chat/completions endpoint, including tool_call_id pairing and the local model's quirks (reasoning, XML fallback, keep_alive)
tags: ollama, openai-compatible, tools, function-calling
---

# Tool calling through the OpenAI-compatible API

## Request

- `POST http://127.0.0.1:11434/v1/chat/completions` with `stream:false` and
  `tools:[{type:"function",function:{name,description,parameters}}]`.
- `config.json` selects the provider by tag and checks `type === "open_ai_compatible_api"`.
  Provider `config` carries `url`, `model`, `temperature`, `num_ctx`, `keep_alive`,
  `request_timeout_secs` and an optional `api_key` (Bearer token).
- Ollama accepts the Ollama-specific `num_ctx` and `keep_alive` fields on its OpenAI-compatible
  endpoint; other OpenAI-compatible servers normally ignore them.

## Response

- The assistant message is `choices[0].message`. Tool calls are
  `choices[0].message.tool_calls[]`; `function.arguments` is a **JSON-encoded string** here.
- `choices[0].message.reasoning` is the model's chain of thought (Ollama's `thinking`).
  **Never** paste it back into history; only `content` and `tool_calls` are resent.
- Preserve the tool call `id` on the assistant message and send the matching `tool_call_id` on
  every `role:"tool"` result. Generate a stable id if a server omits one.
- Usage is `usage.prompt_tokens` / `usage.completion_tokens`.

## Pitfalls (most visible with small local models)

- Some models stuff the call into `content` as `<tool_call>{"name":...,"arguments":{...}}</tool_call>`.
  Keep the XML fallback parser, otherwise tools never fire.
- Argument names get mangled: treat `command` / `cmdline` / `commandline` / `shell_cmd` all as `cmd`.
- Empty `content` with no `tool_calls` means the model stalled. `agent.rs` skips the empty
  assistant turn and re-asks once with tools disabled; if that also fails it returns the raw tool
  results instead of an empty reply.
- Shell diagnostics are in `run_cmd` results: quote URLs containing `&`, use `-f` on curl to
  surface HTTP errors, and avoid placeholder API keys.

## Verify

`curl -s http://127.0.0.1:11434/v1/models` listing the model ids means the provider is up.
