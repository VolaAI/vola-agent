//! Config: config.json plus environment variables plus CLI overrides; defaults target a local Ollama over git-bash.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShellKind {
    Bash,
    Cmd,
    PowerShell,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub config_path: Option<PathBuf>,
    pub home: PathBuf,
    pub provider_tag: String,
    pub provider_type: String,
    pub api_key: Option<String>,
    pub base_url: String,
    pub model: String,
    pub num_ctx: u32,
    pub temperature: f32,
    pub keep_alive: String,
    pub request_timeout_secs: u64,
    pub max_rounds: usize,
    pub max_tool_errors: usize,
    pub cmd_timeout_secs: u64,
    pub output_limit: usize,
    pub budget_chars: usize,
    pub db_path: PathBuf,
    pub assets_dir: PathBuf,
    pub scripts_dir: PathBuf,
    pub skills_dir: PathBuf,
    pub prompt_path: PathBuf,
    pub summarizer_path: PathBuf,
    pub shell: ShellKind,
    pub bash_path: Option<PathBuf>,
    pub verbose: bool,
    pub allow_dangerous: bool,
}

/// `config.json` on disk. Every key is optional, so a partial file only changes what it names;
/// unknown keys are ignored, while a malformed value is a hard error naming the file.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileConfig {
    assets: Option<String>,
    scripts: Option<String>,
    skills: Option<String>,
    database: Option<String>,
    #[serde(alias = "SYSTEM.md")]
    system: Option<String>,
    #[serde(alias = "prompt_init", alias = "PROMPT_INIT.md")]
    memory: Option<String>,
    #[serde(alias = "LLM")]
    llm: LlmConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LlmConfig {
    default: Option<String>,
    providers: Vec<ProviderConfig>,
    /// Legacy `llm.ollama` block, treated as a provider tagged `ollama`.
    ollama: Option<ProviderSettings>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ProviderConfig {
    tag: String,
    #[serde(rename = "type")]
    kind: String,
    config: ProviderSettings,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ProviderSettings {
    url: Option<String>,
    model: Option<String>,
    num_ctx: Option<u32>,
    temperature: Option<f32>,
    keep_alive: Option<String>,
    #[serde(alias = "request_timeout")]
    request_timeout_secs: Option<u64>,
    api_key: Option<String>,
}

fn env_str(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn env_parse<T: std::str::FromStr>(key: &str) -> Option<T> {
    env_str(key).and_then(|v| v.trim().parse::<T>().ok())
}

fn env_flag(key: &str) -> bool {
    matches!(
        env_str(key).map(|v| v.to_ascii_lowercase()),
        Some(ref v) if v == "1" || v == "true" || v == "yes" || v == "on"
    )
}

fn find_bash() -> Option<PathBuf> {
    if let Some(p) = env_str("VOLA_AGENT_BASH") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    if let Some(paths) = env::var_os("PATH") {
        for dir in env::split_paths(&paths) {
            let cand = dir.join("bash.exe");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    for p in [
        "C:/Program Files/Git/bin/bash.exe",
        "C:/Program Files/Git/usr/bin/bash.exe",
        "C:/Program Files (x86)/Git/bin/bash.exe",
        "C:/msys64/usr/bin/bash.exe",
    ] {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    None
}

/// Normalize the Ollama URL: add a scheme, and add the default port 11434 for http only; an explicit https port is left to the user.
fn normalize_url(raw: &str) -> String {
    let mut s = raw.trim().trim_end_matches('/').to_string();
    if !s.starts_with("http://") && !s.starts_with("https://") {
        s = format!("http://{}", s);
    }
    let after_scheme = s.splitn(2, "://").nth(1).unwrap_or("");
    if s.starts_with("http://") && !after_scheme.contains(':') {
        s.push_str(":11434");
    }
    s
}

/// True when `dir` looks like a project root: a config.json, the current layout with the agent
/// assets under `agent/`, or older checkouts with `AGENT.md` at the top level.
fn is_project_root(dir: &Path) -> bool {
    dir.join("config.json").is_file()
        || dir.join("agent/SYSTEM.md").is_file()
        || dir.join("agent/system.md").is_file()
        || dir.join("AGENT.md").is_file()
}

/// Locate the project root: VOLA_AGENT_HOME first, then the current directory (when it holds
/// the agent assets), finally the executable's directory.
fn resolve_home() -> PathBuf {
    if let Some(p) = env_str("VOLA_AGENT_HOME") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return pb;
        }
    }
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if is_project_root(&cwd) {
        return cwd;
    }
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .filter(|p| is_project_root(p))
        .unwrap_or(cwd)
}

impl Config {
    pub fn default_db_path(home: &Path) -> PathBuf {
        home.join("agent/data/memory.db")
    }

    pub fn default_assets_dir(home: &Path) -> PathBuf {
        home.join("agent/assets")
    }

    pub fn default_scripts_dir(home: &Path) -> PathBuf {
        home.join("agent/scripts")
    }

    pub fn default_skills_dir(home: &Path) -> PathBuf {
        home.join("agent/skills")
    }

    pub fn default_prompt_path(home: &Path) -> PathBuf {
        home.join("agent/SYSTEM.md")
    }

    pub fn default_summarizer_path(home: &Path) -> PathBuf {
        home.join("agent/MEMORY.md")
    }

    /// Load `config.json` (from `VOLA_AGENT_CONFIG`, the project root, the working directory,
    /// or the executable's neighborhood), then apply environment overrides.
    pub fn load() -> Result<Config> {
        Self::load_from(None)
    }

    /// Like [`Config::load`], but an explicit path takes precedence over discovery.
    pub fn load_from(explicit: Option<&Path>) -> Result<Config> {
        let mut home = resolve_home();
        let (config_path, file) = match explicit {
            Some(p) => {
                let text = fs::read_to_string(p)
                    .with_context(|| format!("failed to read the config file: {}", p.display()))?;
                let parsed: FileConfig = serde_json::from_str(&text)
                    .with_context(|| format!("failed to parse the config file: {}", p.display()))?;
                (Some(p.to_path_buf()), parsed)
            }
            None => load_file_config(&home)?,
        };
        // Running target/release/vola-agent.exe from outside the project still finds the root:
        // a config.json two levels above the executable marks it.
        if let Some(dir) = config_path.as_ref().and_then(|p| p.parent()) {
            if env_str("VOLA_AGENT_HOME").is_none() && !is_project_root(&home) {
                home = dir.to_path_buf();
            }
        }
        let base = config_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.clone());

        let assets_dir = resolve_path(
            "VOLA_AGENT_ASSETS",
            file.assets.as_deref(),
            &base,
            &["agent/assets", "assets"],
        );
        let scripts_dir = resolve_path(
            "VOLA_AGENT_SCRIPTS",
            file.scripts.as_deref(),
            &base,
            &["agent/scripts", "scripts"],
        );
        let skills_dir = resolve_path(
            "VOLA_AGENT_SKILLS",
            file.skills.as_deref(),
            &base,
            &["agent/skills", "skills"],
        );
        let prompt_path = resolve_path(
            "VOLA_AGENT_PROMPT",
            file.system.as_deref(),
            &base,
            &[
                "agent/SYSTEM.md",
                "agent/system.md",
                "SYSTEM.md",
                "system.md",
                "AGENT.md",
            ],
        );
        let summarizer_path = resolve_path(
            "VOLA_AGENT_SUMMARIZER",
            file.memory.as_deref(),
            &base,
            &[
                "agent/MEMORY.md",
                "agent/memory.md",
                "MEMORY.md",
                "memory.md",
                "agent/PROMPT_INIT.md",
                "PROMPT_INIT.md",
                "PROMPT.md",
            ],
        );
        let db_path = resolve_path(
            "VOLA_AGENT_DB",
            file.database.as_deref(),
            &base,
            &["agent/data/memory.db", "data/memory.db"],
        );

        let LlmConfig {
            default,
            mut providers,
            ollama,
        } = file.llm;
        if providers.is_empty() {
            providers.push(ProviderConfig {
                tag: "ollama".to_string(),
                kind: "open_ai_compatible_api".to_string(),
                config: ollama.unwrap_or_default(),
            });
        }
        let wanted = default
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| providers[0].tag.clone());
        let tags: Vec<&str> = providers.iter().map(|p| p.tag.as_str()).collect();
        let selected = providers.iter().find(|p| p.tag == wanted).ok_or_else(|| {
            anyhow!(
                "llm.default `{}` does not match any provider tag; available tags: {}",
                wanted,
                tags.join(", ")
            )
        })?;
        if selected.tag.trim().is_empty() {
            return Err(anyhow!("every llm.providers entry needs a non-empty `tag`"));
        }
        let provider_type = selected.kind.trim().to_string();
        if !["open_ai_compatible_api", "ollama", "deepseek"].contains(&provider_type.as_str()) {
            return Err(anyhow!(
                "provider `{}` has unsupported type `{}`; supported types: `open_ai_compatible_api`, `ollama`, `deepseek`",
                selected.tag,
                provider_type
            ));
        }
        let provider_tag = selected.tag.clone();
        let settings = &selected.config;
        let default_url = if provider_type == "deepseek" {
            "https://api.deepseek.com/v1"
        } else {
            "http://127.0.0.1:11434"
        };
        let base_url = normalize_url(
            &settings
                .url
                .clone()
                .or_else(|| env_str("VOLA_AGENT_URL"))
                .or_else(|| env_str("OLLAMA_HOST"))
                .unwrap_or_else(|| default_url.to_string()),
        );
        let api_key = settings
            .api_key
            .clone()
            .or_else(|| env_str("VOLA_AGENT_API_KEY"))
            .or_else(|| {
                if provider_type == "deepseek" {
                    env_str("DEEPSEEK_API_KEY")
                } else {
                    env_str("OPENAI_API_KEY")
                }
            });
        let default_model = if provider_type == "deepseek" {
            "deepseek-flash"
        } else {
            "heretic-9b-64k:latest"
        };
        let model = settings
            .model
            .clone()
            .or_else(|| env_str("VOLA_AGENT_MODEL"))
            .unwrap_or_else(|| default_model.to_string());
        let num_ctx: u32 = settings
            .num_ctx
            .or_else(|| env_parse("VOLA_AGENT_NUM_CTX"))
            .unwrap_or(16_384);
        let output_limit: usize = env_parse("VOLA_AGENT_OUTPUT_LIMIT").unwrap_or(6_000);
        let budget_chars: usize = env_parse("VOLA_AGENT_BUDGET").unwrap_or((num_ctx as usize) * 2);
        let bash = find_bash();
        let shell = match env_str("VOLA_AGENT_SHELL").map(|v| v.to_ascii_lowercase()) {
            Some(ref v) if v == "cmd" => ShellKind::Cmd,
            Some(ref v) if v == "powershell" || v == "pwsh" => ShellKind::PowerShell,
            Some(ref v) if v == "bash" || v == "sh" => ShellKind::Bash,
            _ => {
                if bash.is_some() {
                    ShellKind::Bash
                } else if cfg!(windows) {
                    ShellKind::PowerShell
                } else {
                    ShellKind::Bash
                }
            }
        };
        Ok(Config {
            config_path,
            home,
            provider_tag,
            provider_type,
            api_key,
            base_url,
            model,
            num_ctx,
            temperature: settings
                .temperature
                .or_else(|| env_parse("VOLA_AGENT_TEMPERATURE"))
                .unwrap_or(0.6),
            keep_alive: settings
                .keep_alive
                .clone()
                .or_else(|| env_str("VOLA_AGENT_KEEP_ALIVE"))
                .unwrap_or_else(|| "30m".to_string()),
            request_timeout_secs: settings
                .request_timeout_secs
                .or_else(|| env_parse("VOLA_AGENT_REQUEST_TIMEOUT"))
                .unwrap_or(300),
            max_rounds: env_parse("VOLA_AGENT_MAX_ROUNDS").unwrap_or(8),
            max_tool_errors: env_parse("VOLA_AGENT_MAX_TOOL_ERRORS").unwrap_or(3),
            cmd_timeout_secs: env_parse("VOLA_AGENT_CMD_TIMEOUT").unwrap_or(60),
            output_limit,
            budget_chars,
            db_path,
            assets_dir,
            scripts_dir,
            skills_dir,
            prompt_path,
            summarizer_path,
            shell,
            bash_path: bash,
            verbose: env_flag("VOLA_AGENT_VERBOSE"),
            allow_dangerous: env_flag("VOLA_AGENT_ALLOW_DANGEROUS"),
        })
    }

    pub fn summary(&self) -> String {
        format!(
            "provider {} ({}) | model {} @ {} | num_ctx {} | shell {:?} | budget {} chars | config {} | DB {}",
            self.provider_tag,
            self.provider_type,
            self.model,
            self.base_url,
            self.num_ctx,
            self.shell,
            self.budget_chars,
            self.config_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(defaults)".to_string()),
            self.db_path.display()
        )
    }
}

/// Resolve one configured path. `config.json` wins over environment variables; a relative
/// value is taken from the config file's directory (or `base` when there is no config file).
/// With no explicit value the first existing default candidate is tried in order, uppercase
/// names before lowercase ones.
fn resolve_path(
    env_key: &str,
    configured: Option<&str>,
    base: &Path,
    defaults: &[&str],
) -> PathBuf {
    if let Some(raw) = configured.map(str::trim).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(raw);
        return if p.is_absolute() { p } else { base.join(p) };
    }
    if let Some(p) = env_str(env_key) {
        return PathBuf::from(p);
    }
    for d in defaults {
        let cand = base.join(d);
        if cand.exists() {
            return cand;
        }
    }
    base.join(defaults[0])
}

/// Config discovery order: VOLA_AGENT_CONFIG, the project root, the working directory, the
/// executable's directory, then the root two levels above it (the `target/release` layout).
fn config_candidates(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = env_str("VOLA_AGENT_CONFIG") {
        out.push(PathBuf::from(p));
    }
    out.push(home.join("config.json"));
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if cwd != home {
        out.push(cwd.join("config.json"));
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("config.json"));
            if let Some(root) = dir.parent().and_then(Path::parent) {
                out.push(root.join("config.json"));
            }
        }
    }
    out
}

fn load_file_config(home: &Path) -> Result<(Option<PathBuf>, FileConfig)> {
    for cand in config_candidates(home) {
        if !cand.is_file() {
            continue;
        }
        let text = fs::read_to_string(&cand)
            .with_context(|| format!("failed to read the config file: {}", cand.display()))?;
        let parsed: FileConfig = serde_json::from_str(&text)
            .with_context(|| format!("failed to parse the config file: {}", cand.display()))?;
        return Ok((Some(cand), parsed));
    }
    Ok((None, FileConfig::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_normalization() {
        assert_eq!(normalize_url("127.0.0.1:11434"), "http://127.0.0.1:11434");
        assert_eq!(normalize_url("localhost"), "http://localhost:11434");
        assert_eq!(
            normalize_url("http://127.0.0.1:11434/"),
            "http://127.0.0.1:11434"
        );
        assert_eq!(
            normalize_url("https://ark.example.com"),
            "https://ark.example.com"
        );
    }

    #[test]
    fn config_json_keys() {
        let file: FileConfig = serde_json::from_str(
            r#"{
                "assets": "data/assets",
                "scripts": "data/scripts",
                "skills": "data/skill",
                "database": "data/db.sqlite",
                "system": "prompts/SYSTEM.md",
                "memory": "prompts/MEMORY.md",
                "llm": {"default": "ollama", "providers": [{"tag": "ollama", "type": "open_ai_compatible_api", "config": {"url": "127.0.0.1:11434", "model": "m", "num_ctx": 8192, "temperature": 0.2}}]}
            }"#,
        )
        .unwrap();
        assert_eq!(file.assets.as_deref(), Some("data/assets"));
        assert_eq!(file.scripts.as_deref(), Some("data/scripts"));
        assert_eq!(file.skills.as_deref(), Some("data/skill"));
        assert_eq!(file.database.as_deref(), Some("data/db.sqlite"));
        assert_eq!(file.system.as_deref(), Some("prompts/SYSTEM.md"));
        assert_eq!(file.memory.as_deref(), Some("prompts/MEMORY.md"));
        assert_eq!(file.llm.default.as_deref(), Some("ollama"));
        assert_eq!(file.llm.providers.len(), 1);
        assert_eq!(file.llm.providers[0].tag, "ollama");
        assert_eq!(file.llm.providers[0].kind, "open_ai_compatible_api");
        assert_eq!(
            file.llm.providers[0].config.url.as_deref(),
            Some("127.0.0.1:11434")
        );
        assert_eq!(file.llm.providers[0].config.model.as_deref(), Some("m"));
        assert_eq!(file.llm.providers[0].config.num_ctx, Some(8192));
    }

    #[test]
    fn legacy_llm_ollama_block_is_still_accepted() {
        let file: FileConfig =
            serde_json::from_str(r#"{"llm": {"ollama": {"model": "old-model"}}}"#).unwrap();
        let legacy = file.llm.ollama.expect("legacy ollama block");
        assert_eq!(legacy.model.as_deref(), Some("old-model"));
    }

    #[test]
    fn configured_paths_are_relative_to_the_config_file() {
        let base = Path::new("E:/proj");
        assert_eq!(
            resolve_path(
                "VOLA_AGENT_NO_SUCH_KEY",
                Some("assets/icons"),
                base,
                &["assets"]
            ),
            PathBuf::from("E:/proj/assets/icons")
        );
        assert_eq!(
            resolve_path(
                "VOLA_AGENT_NO_SUCH_KEY",
                Some("D:/assets/icons"),
                base,
                &["assets"]
            ),
            PathBuf::from("D:/assets/icons")
        );
        assert_eq!(
            resolve_path("VOLA_AGENT_NO_SUCH_KEY", None, base, &["some/deep/missing"]),
            PathBuf::from("E:/proj/some/deep/missing")
        );
    }
}
