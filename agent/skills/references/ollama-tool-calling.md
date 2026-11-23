---
name: ollama-tool-calling
description: Call tools through the selected provider type (native Ollama, OpenAI-compatible, or DeepSeek), including tool_call_id pairing and small-model quirks
tags: ollama, openai-compatible, deepseek, tools, function-calling
---

# Tool calling through the selected provider

## Request

- `type: ollama` uses native `POST {url}/api/chat` with `stream:false`, `options.num_ctx`,
  `options.temperature` and `keep_alive`.
- `type: open_ai_compatible_api` uses `POST {url}/v1/chat/completions` with standard OpenAI
  fields; `type: deepseek` does the same against `https://api.deepseek.com/v1` by default.
- Send `tools:[{type:"function",function:{name,description,parameters}}]` in every case.
- Provider `config` carries `url`, `model`, `temperature`, `num_ctx`, `keep_alive`,
  `request_timeout_secs` and an optional `api_key` (Bearer token for OpenAI/DeepSeek).
- DeepSeek keys come from `api_key`, `RS_AGENT_API_KEY`, then `DEEPSEEK_API_KEY`.

## Response

- OpenAI/DeepSeek: the assistant message is `choices[0].message`; tool calls are
  `choices[0].message.tool_calls[]`; `function.arguments` is a **JSON-encoded string**.
- Native Ollama: the reply is `message`; `message.tool_calls[]` and `function.arguments` is an object.
- Reasoning content (`reasoning` / `reasoning_content` / `thinking`) is deliberately not captured;
  only `content` and `tool_calls` are resent.
- Preserve the tool call `id` on the assistant message and send the matching `tool_call_id` on
  every OpenAI/DeepSeek `role:"tool"` result. Native Ollama tool results use `tool_name` instead.
  Generate a stable id if a server omits one.
- Usage is `usage.prompt_tokens` / `usage.completion_tokens` for OpenAI/DeepSeek; native Ollama
  uses `prompt_eval_count` / `eval_count` / `eval_duration`.

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

- Native Ollama: `curl -s http://127.0.0.1:11434/api/tags` lists the models.
- OpenAI-compatible: `curl -s http://127.0.0.1:11434/v1/models` lists the model ids.
- DeepSeek: `POST https://api.deepseek.com/v1/chat/completions` with the key works; its
  `/v1/models` endpoint may be unavailable, so `models` falls back to the configured model.
