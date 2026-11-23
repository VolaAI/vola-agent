//! OpenAI-compatible chat client: POST /v1/chat/completions and GET /v1/models.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::util::snippet;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ToolCallFunction {
    pub name: String,
    /// OpenAI sends a JSON-encoded string; some native servers send an object. Keep `Value` and
    /// normalize when sending or dispatching.
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

/// One assistant message from a completion. `reasoning` (Ollama's thinking field) is deliberately
/// not captured: it stays on the provider side and is never fed back into history.
#[derive(Debug, Deserialize, Default)]
pub struct ChatMessage {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Deserialize, Default)]
struct Choice {
    #[serde(default)]
    message: ChatMessage,
}

#[derive(Debug, Deserialize, Default)]
struct Usage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Usage,
    #[serde(skip)]
    elapsed_secs: f64,
}

impl ChatResponse {
    pub fn content(&self) -> String {
        self.choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default()
    }

    pub fn calls(&self) -> Vec<ToolCall> {
        self.choices
            .first()
            .and_then(|c| c.message.tool_calls.clone())
            .unwrap_or_default()
    }

    pub fn prompt_tokens(&self) -> Option<u64> {
        self.usage.prompt_tokens
    }

    /// Tokens generated per second (completion tokens over the measured request time).
    pub fn tokens_per_sec(&self) -> Option<f64> {
        match self.usage.completion_tokens {
            Some(count) if self.elapsed_secs > 0.0 => Some(count as f64 / self.elapsed_secs),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub owned_by: String,
}

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelInfo>,
}

pub struct LlmClient {
    agent: ureq::Agent,
    base: String,
    api_key: Option<String>,
    tag: String,
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
            model: cfg.model.clone(),
            num_ctx: cfg.num_ctx,
            temperature: cfg.temperature,
            keep_alive: cfg.keep_alive.clone(),
        }
    }

    fn endpoint(&self, path: &str) -> String {
        let base = self.base.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{}{}", base, path)
        } else {
            format!("{}/v1{}", base, path)
        }
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
        let mut body = json!({
            "model": self.model,
            "messages": messages.iter().map(openai_message).collect::<Vec<_>>(),
            "stream": false,
            "temperature": self.temperature,
            // Ollama extensions accepted by its OpenAI-compatible endpoint; other local
            // OpenAI-compatible servers normally ignore unknown fields.
            "num_ctx": self.num_ctx,
            "keep_alive": self.keep_alive,
        });
        if let Some(t) = tools {
            if !t.is_empty() {
                body["tools"] = Value::Array(t.to_vec());
            }
        }

        let url = self.endpoint("/chat/completions");
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
                    let parsed: ChatResponse = serde_json::from_str(&text).with_context(|| {
                        format!(
                            "failed to parse the /v1/chat/completions response: {}",
                            snippet(&text, 400)
                        )
                    })?;
                    let mut parsed = parsed;
                    parsed.elapsed_secs = started.elapsed().as_secs_f64();
                    return Ok(parsed);
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
        let url = self.endpoint("/models");
        let resp = self
            .authorize(self.agent.get(&url))
            .call()
            .map_err(|e| anyhow!("failed to fetch models from {}: {}", url, e))?;
        let models: ModelsResponse = resp
            .into_json()
            .context("failed to parse the /v1/models response")?;
        Ok(models.data)
    }

    /// Check the provider and model before a turn starts, so a missing server or model fails
    /// fast with an actionable message instead of a raw connection error mid-conversation.
    pub fn ensure_ready(&self) -> Result<()> {
        let models = self.list_models().with_context(|| {
            format!(
                "LLM provider `{}` is not reachable at {}; start it first (for Ollama: G:\\Ollama\\start-ollama.bat, or `ollama serve`)",
                self.tag, self.base
            )
        })?;
        if models.iter().any(|m| model_matches(&m.id, &self.model)) {
            return Ok(());
        }
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
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

/// Convert one internal message to the OpenAI chat-completions wire format.
fn openai_message(message: &Message) -> Value {
    let mut out = json!({"role": message.role, "content": message.content});
    if let Some(calls) = &message.tool_calls {
        if !calls.is_empty() {
            out["tool_calls"] = Value::Array(calls.iter().map(tool_call_json).collect());
        }
    }
    if let Some(id) = &message.tool_call_id {
        out["tool_call_id"] = json!(id);
    }
    out
}

fn tool_call_json(call: &ToolCall) -> Value {
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

/// A model name from `/v1/models` matches the configured one with or without an explicit `:latest` tag.
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
