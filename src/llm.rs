//! LLM client for the supported provider types:
//! - `ollama`: native `/api/chat` and `/api/tags`
//! - `open_ai_compatible_api`: `POST /v1/chat/completions`, `GET /v1/models`
//! - `deepseek`: DeepSeek's OpenAI-compatible `/v1` API (models endpoint is optional)

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::util::snippet;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ToolCallFunction {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ToolCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub function: ToolCallFunction,
}

#[derive(Clone, Debug, Default)]
pub struct Message {
    pub role: String,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Message {
        Message {
            role: "system".into(),
            content: content.into(),
            ..Default::default()
        }
    }
    pub fn user(content: impl Into<String>) -> Message {
        Message {
            role: "user".into(),
            content: content.into(),
            ..Default::default()
        }
    }

    /// Rough character count, used for the context budget.
    pub fn weight(&self) -> usize {
        let calls = self
            .tool_calls
            .as_ref()
            .map(|c| serde_json::to_string(c).map(|s| s.len()).unwrap_or(0))
            .unwrap_or(0);
        crate::util::char_len(&self.content) + calls
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderKind {
    Ollama,
    OpenAiCompatible,
    DeepSeek,
}

impl ProviderKind {
    fn from_config(type_name: &str) -> ProviderKind {
        match type_name {
            "ollama" => ProviderKind::Ollama,
            "deepseek" => ProviderKind::DeepSeek,
            _ => ProviderKind::OpenAiCompatible,
        }
    }
}

#[derive(Debug, Default)]
pub struct ChatResponse {
    content: String,
    calls: Vec<ToolCall>,
    prompt_tokens: Option<u64>,
    tokens_per_sec: Option<f64>,
}

impl ChatResponse {
    pub fn content(&self) -> String {
        self.content.clone()
    }

    pub fn calls(&self) -> Vec<ToolCall> {
        self.calls.clone()
    }

    pub fn prompt_tokens(&self) -> Option<u64> {
        self.prompt_tokens
    }

    pub fn tokens_per_sec(&self) -> Option<f64> {
        self.tokens_per_sec
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub owned_by: String,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Deserialize, Default)]
struct OpenAiResponse {
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: OpenAiUsage,
}

#[derive(Deserialize, Default)]
struct OpenAiChoice {
    #[serde(default)]
    message: OpenAiMessage,
}

#[derive(Clone, Deserialize, Default)]
struct OpenAiMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Deserialize, Default)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct OpenAiModels {
    #[serde(default)]
    data: Vec<ModelInfo>,
}

#[derive(Deserialize, Default)]
struct OllamaResponse {
    #[serde(default)]
    message: OllamaMessage,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
    #[serde(default)]
    eval_duration: Option<u64>,
}

#[derive(Deserialize, Default)]
struct OllamaMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Deserialize)]
struct OllamaTags {
    #[serde(default)]
    models: Vec<OllamaTag>,
}

#[derive(Deserialize, Default)]
struct OllamaTag {
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: u64,
}

pub struct LlmClient {
    agent: ureq::Agent,
    base: String,
    api_key: Option<String>,
    tag: String,
    kind: ProviderKind,
    pub model: String,
    num_ctx: u32,
    temperature: f32,
    keep_alive: String,
}

impl LlmClient {
    pub fn new(cfg: &Config) -> LlmClient {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(cfg.request_timeout_secs))
            .build();
        LlmClient {
            agent,
            base: cfg.base_url.clone(),
            api_key: cfg.api_key.clone(),
            tag: cfg.provider_tag.clone(),
            kind: ProviderKind::from_config(&cfg.provider_type),
            model: cfg.model.clone(),
            num_ctx: cfg.num_ctx,
            temperature: cfg.temperature,
            keep_alive: cfg.keep_alive.clone(),
        }
    }

    fn openai_endpoint(&self, path: &str) -> String {
        let base = self.base.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{}{}", base, path)
        } else {
            format!("{}/v1{}", base, path)
        }
    }

    fn ollama_endpoint(&self, path: &str) -> String {
        format!("{}{}", self.base.trim_end_matches('/'), path)
    }

    fn authorize(&self, req: ureq::Request) -> ureq::Request {
        match &self.api_key {
            Some(key) => req.set("Authorization", &format!("Bearer {}", key)),
            None => req,
        }
    }

    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    /// One completion; tools are disabled when the list is empty.
    pub fn chat(&self, messages: &[Message], tools: Option<&[Value]>) -> Result<ChatResponse> {
        match self.kind {
            ProviderKind::Ollama => self.chat_ollama(messages, tools),
            ProviderKind::OpenAiCompatible | ProviderKind::DeepSeek => {
                self.chat_openai(messages, tools)
            }
        }
    }

    fn chat_openai(&self, messages: &[Message], tools: Option<&[Value]>) -> Result<ChatResponse> {
        let mut body = json!({
            "model": self.model,
            "messages": messages.iter().map(openai_message).collect::<Vec<_>>(),
            "stream": false,
            "temperature": self.temperature,
        });
        if let Some(t) = tools {
            if !t.is_empty() {
                body["tools"] = Value::Array(t.to_vec());
            }
        }

        let url = self.openai_endpoint("/chat/completions");
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..2 {
            let started = Instant::now();
            match self
                .authorize(self.agent.post(&url))
                .send_json(body.clone())
            {
                Ok(resp) => {
                    let text = resp
                        .into_string()
                        .context("failed to read the completion response body")?;
                    let parsed: OpenAiResponse =
                        serde_json::from_str(&text).with_context(|| {
                            format!(
                                "failed to parse the /v1/chat/completions response: {}",
                                snippet(&text, 400)
                            )
                        })?;
                    let message = parsed
                        .choices
                        .first()
                        .map(|c| &c.message)
                        .cloned()
                        .unwrap_or_default();
                    let elapsed = started.elapsed().as_secs_f64();
                    let speed = match parsed.usage.completion_tokens {
                        Some(count) if elapsed > 0.0 => Some(count as f64 / elapsed),
                        _ => None,
                    };
                    return Ok(ChatResponse {
                        content: message.content.unwrap_or_default(),
                        calls: message.tool_calls.unwrap_or_default(),
                        prompt_tokens: parsed.usage.prompt_tokens,
                        tokens_per_sec: speed,
                    });
                }
                Err(ureq::Error::Status(code, resp)) => {
                    let detail = resp.into_string().unwrap_or_default();
                    return Err(anyhow!(
                        "provider `{}` returned HTTP {}: {}",
                        self.tag,
                        code,
                        snippet(&detail, 300)
                    ));
                }
                Err(e) => {
                    last_err = Some(anyhow!(
                        "failed to connect to provider `{}` (attempt {}, {}): {}. Make sure the server is running at {}",
                        self.tag,
                        attempt + 1,
                        self.base,
                        e,
                        self.base
                    ));
                    std::thread::sleep(Duration::from_millis(600));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("unknown error")))
    }

    fn chat_ollama(&self, messages: &[Message], tools: Option<&[Value]>) -> Result<ChatResponse> {
        let mut body = json!({
            "model": self.model,
            "messages": messages.iter().map(ollama_message).collect::<Vec<_>>(),
            "stream": false,
            "options": {"num_ctx": self.num_ctx, "temperature": self.temperature},
            "keep_alive": self.keep_alive,
        });
        if let Some(t) = tools {
            if !t.is_empty() {
                body["tools"] = Value::Array(t.to_vec());
            }
        }

        let url = self.ollama_endpoint("/api/chat");
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..2 {
            match self.agent.post(&url).send_json(body.clone()) {
                Ok(resp) => {
                    let text = resp
                        .into_string()
                        .context("failed to read the /api/chat response body")?;
                    let parsed: OllamaResponse =
                        serde_json::from_str(&text).with_context(|| {
                            format!(
                                "failed to parse the /api/chat response: {}",
                                snippet(&text, 400)
                            )
                        })?;
                    let speed = match (parsed.eval_count, parsed.eval_duration) {
                        (Some(count), Some(nanos)) if nanos > 0 => {
                            Some(count as f64 / (nanos as f64 / 1e9))
                        }
                        _ => None,
                    };
                    return Ok(ChatResponse {
                        content: parsed.message.content.unwrap_or_default(),
                        calls: parsed.message.tool_calls.unwrap_or_default(),
                        prompt_tokens: parsed.prompt_eval_count,
                        tokens_per_sec: speed,
                    });
                }
                Err(ureq::Error::Status(code, resp)) => {
                    let detail = resp.into_string().unwrap_or_default();
                    return Err(anyhow!(
                        "provider `{}` returned HTTP {}: {}",
                        self.tag,
                        code,
                        snippet(&detail, 300)
                    ));
                }
                Err(e) => {
                    last_err = Some(anyhow!(
                        "failed to connect to provider `{}` (attempt {}, {}): {}. Make sure the server is running at {}",
                        self.tag,
                        attempt + 1,
                        self.base,
                        e,
                        self.base
                    ));
                    std::thread::sleep(Duration::from_millis(600));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("unknown error")))
    }

    pub fn list_models(&self) -> Result<Vec<ModelInfo>> {
        match self.kind {
            ProviderKind::Ollama => self.list_ollama_models(),
            ProviderKind::OpenAiCompatible => self.list_openai_models(),
            ProviderKind::DeepSeek => self.list_openai_models().or_else(|_| {
                Ok(vec![ModelInfo {
                    id: self.model.clone(),
                    owned_by: "deepseek".to_string(),
                    size: None,
                }])
            }),
        }
    }

    fn list_openai_models(&self) -> Result<Vec<ModelInfo>> {
        let url = self.openai_endpoint("/models");
        let resp = self
            .authorize(self.agent.get(&url))
            .call()
            .map_err(|e| anyhow!("failed to fetch models from {}: {}", url, e))?;
        let models: OpenAiModels = resp
            .into_json()
            .context("failed to parse the /v1/models response")?;
        Ok(models.data)
    }

    fn list_ollama_models(&self) -> Result<Vec<ModelInfo>> {
        let url = self.ollama_endpoint("/api/tags");
        let resp = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| anyhow!("failed to fetch models from {}: {}", url, e))?;
        let tags: OllamaTags = resp
            .into_json()
            .context("failed to parse the /api/tags response")?;
        Ok(tags
            .models
            .into_iter()
            .map(|m| ModelInfo {
                id: m.name,
                owned_by: "ollama".to_string(),
                size: Some(m.size),
            })
            .collect())
    }

    /// Check the provider and model before a turn starts, so a missing server or model fails
    /// fast with an actionable message instead of a raw connection error mid-conversation.
    pub fn ensure_ready(&self) -> Result<()> {
        match self.kind {
            ProviderKind::DeepSeek => {
                if self.api_key.is_none() {
                    return Err(anyhow!(
                        "provider `{}` needs an API key: set `api_key` in config.json, or RS_AGENT_API_KEY / DEEPSEEK_API_KEY",
                        self.tag
                    ));
                }
                Ok(())
            }
            ProviderKind::Ollama => {
                let models = self.list_ollama_models().with_context(|| {
                    format!(
                        "LLM provider `{}` is not reachable at {}; start it first (for Ollama: G:\\Ollama\\start-ollama.bat, or `ollama serve`)",
                        self.tag, self.base
                    )
                })?;
                self.require_model(
                    &models,
                    &models.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
                )
            }
            ProviderKind::OpenAiCompatible => {
                let models = self.list_openai_models().with_context(|| {
                    format!(
                        "LLM provider `{}` is not reachable at {}; start the OpenAI-compatible server",
                        self.tag, self.base
                    )
                })?;
                self.require_model(
                    &models,
                    &models.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
                )
            }
        }
    }

    fn require_model(&self, models: &[ModelInfo], ids: &[String]) -> Result<()> {
        if models.iter().any(|m| model_matches(&m.id, &self.model)) {
            return Ok(());
        }
        Err(anyhow!(
            "model `{}` is not available from provider `{}` at {}. Available: {}. Install/pull it, or change `llm.default` or the provider config in config.json.",
            self.model,
            self.tag,
            self.base,
            if ids.is_empty() {
                "none".to_string()
            } else {
                ids.join(", ")
            }
        ))
    }
}

fn openai_message(message: &Message) -> Value {
    let mut out = json!({"role": message.role, "content": message.content});
    if let Some(calls) = &message.tool_calls {
        if !calls.is_empty() {
            out["tool_calls"] = Value::Array(calls.iter().map(openai_tool_call).collect());
        }
    }
    if let Some(id) = &message.tool_call_id {
        out["tool_call_id"] = json!(id);
    }
    out
}

fn openai_tool_call(call: &ToolCall) -> Value {
    let arguments = match &call.function.arguments {
        Value::String(raw) => raw.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string()),
    };
    json!({
        "id": call.id.clone().unwrap_or_default(),
        "type": "function",
        "function": {"name": call.function.name, "arguments": arguments},
    })
}

fn ollama_message(message: &Message) -> Value {
    let mut out = json!({"role": message.role, "content": message.content});
    if let Some(calls) = &message.tool_calls {
        if !calls.is_empty() {
            out["tool_calls"] = Value::Array(calls.iter().map(ollama_tool_call).collect());
        }
    }
    if let Some(name) = &message.tool_name {
        out["tool_name"] = json!(name);
    }
    out
}

fn ollama_tool_call(call: &ToolCall) -> Value {
    let arguments = match &call.function.arguments {
        Value::String(raw) => {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.clone()))
        }
        other => other.clone(),
    };
    json!({
        "function": {"name": call.function.name, "arguments": arguments},
    })
}

/// A model name from `/v1/models` or `/api/tags` matches the configured one with or without an
/// explicit `:latest` tag.
fn model_matches(available: &str, wanted: &str) -> bool {
    fn normalize(name: &str) -> &str {
        name.strip_suffix(":latest").unwrap_or(name)
    }
    normalize(available) == normalize(wanted)
}

#[cfg(test)]
mod tests {
    use super::model_matches;

    #[test]
    fn model_name_matching() {
        assert!(model_matches("heretic-9b-64k", "heretic-9b-64k:latest"));
        assert!(model_matches("heretic-9b-64k:latest", "heretic-9b-64k"));
        assert!(model_matches("qwen3-vl:2b", "qwen3-vl:2b"));
        assert!(!model_matches("qwen3-vl:2b", "heretic-9b-64k:latest"));
    }
}
