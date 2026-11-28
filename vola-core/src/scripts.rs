//! Script library: executable files under the configured scripts directory.
//!
//! Metadata lives in a `# --- vola-script ---` comment block at the top of each file
//! (`#`/`//`/`--`/`;`/`%` comment markers are accepted). `INDEX.md` is generated from
//! that metadata and must not be edited by hand. Files are the single source of truth.

use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::util::slugify;

const BEGIN: &str = "--- vola-script ---";
const END: &str = "--- /vola-script ---";

#[derive(Clone, Debug, Serialize)]
pub struct ScriptMeta {
    pub name: String,
    pub when: String,
    pub lang: String,
    pub deps: String,
    pub usage: String,
    pub version: String,
    pub last_verified: String,
    pub status: String,
    pub platform: String,
    pub path: PathBuf,
}

/// Everything `script_save` needs to write one script file.
pub struct ScriptSpec<'a> {
    pub name: &'a str,
    pub lang: &'a str,
    pub when: &'a str,
    pub deps: &'a str,
    pub usage: &'a str,
    pub version: &'a str,
    pub last_verified: &'a str,
    pub status: &'a str,
    pub platform: &'a str,
    pub body: &'a str,
}

pub struct Scripts {
    dir: PathBuf,
}

/// Strip one leading comment marker so the metadata block can be parsed from any language.
fn strip_comment(line: &str) -> &str {
    let t = line.trim_start();
    for p in ["#", "//", "--", ";", "%", "*"] {
        if let Some(rest) = t.strip_prefix(p) {
            return rest.trim_start();
        }
    }
    t
}

/// Parse the `--- vola-script ---` block into a key/value map (keys lowercased).
pub fn parse_metadata(text: &str) -> BTreeMap<String, String> {
    let mut meta = BTreeMap::new();
    let mut inside = false;
    for line in text.lines().take(120) {
        let body = strip_comment(line);
        if body.starts_with(BEGIN) {
            inside = true;
            continue;
        }
        if body.starts_with(END) {
            break;
        }
        if inside {
            if let Some((k, v)) = body.split_once(':') {
                let key = k.trim().to_ascii_lowercase();
                if !key.is_empty() {
                    meta.insert(key, v.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    meta
}

/// Canonical script name: the file stem reduced to a kebab-case slug.
/// Accepts both `pdf-split` and `pdf-split.py`.
pub fn normalize_name(name: &str) -> String {
    let trimmed = name.trim();
    let stem = Path::new(trimmed)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(trimmed);
    slugify(stem)
}

fn extension_for(lang: &str, name: &str) -> String {
    if let Some(ext) = Path::new(name.trim()).extension().and_then(|e| e.to_str()) {
        let ext = ext.to_ascii_lowercase();
        if !ext.is_empty() && ext.len() <= 6 {
            return ext;
        }
    }
    match lang.trim().to_ascii_lowercase().as_str() {
        "python" | "py" | "python3" => "py",
        "bash" | "sh" | "shell" | "zsh" => "sh",
        "javascript" | "js" | "node" => "js",
        "typescript" | "ts" => "ts",
        "powershell" | "ps1" | "pwsh" => "ps1",
        "cmd" | "bat" | "batch" => "bat",
        "ruby" | "rb" => "rb",
        "lua" => "lua",
        "perl" | "pl" => "pl",
        "php" => "php",
        "r" | "rscript" => "r",
        "go" | "golang" => "go",
        "rust" | "rs" => "rs",
        "sql" => "sql",
        _ => "sh",
    }
    .to_string()
}

fn lang_from_ext(path: &Path) -> String {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "py" => "python",
        "sh" | "bash" => "bash",
        "js" => "javascript",
        "ts" => "typescript",
        "ps1" => "powershell",
        "bat" | "cmd" => "cmd",
        "rb" => "ruby",
        "lua" => "lua",
        "pl" => "perl",
        "php" => "php",
        "r" => "r",
        "go" => "go",
        "rs" => "rust",
        "sql" => "sql",
        _ => "",
    }
    .to_string()
}

fn bump_version(old: &str) -> String {
    let trimmed = old.trim();
    if trimmed.is_empty() {
        return "1.0".to_string();
    }
    if let Some((major, minor)) = trimmed.split_once('.') {
        if let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) {
            return format!("{}.{}", major, minor + 1);
        }
    }
    if let Ok(n) = trimmed.parse::<u32>() {
        return (n + 1).to_string();
    }
    "1.0".to_string()
}

fn meta_from_path(path: &Path, text: &str) -> ScriptMeta {
    let meta = parse_metadata(text);
    let name = meta.get("name").cloned().unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("script")
            .to_string()
    });
    let lang = meta
        .get("lang")
        .cloned()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| lang_from_ext(path));
    ScriptMeta {
        name: normalize_name(&name),
        when: meta.get("when").cloned().unwrap_or_default(),
        lang,
        deps: meta.get("deps").cloned().unwrap_or_default(),
        usage: meta.get("usage").cloned().unwrap_or_default(),
        version: meta.get("version").cloned().unwrap_or_default(),
        last_verified: meta.get("last_verified").cloned().unwrap_or_default(),
        status: meta.get("status").cloned().unwrap_or_default(),
        platform: meta.get("platform").cloned().unwrap_or_default(),
        path: path.to_path_buf(),
    }
}

fn collect_scripts(dir: &Path, depth: usize, out: &mut Vec<ScriptMeta>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if file_name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if depth > 0 {
                collect_scripts(&path, depth - 1, out);
            }
            continue;
        }
        if file_name.eq_ignore_ascii_case("INDEX.md") {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("md") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        out.push(meta_from_path(&path, &text));
    }
}

impl Scripts {
    pub fn new(dir: PathBuf) -> Scripts {
        let _ = fs::create_dir_all(&dir);
        Scripts { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn discover(&self) -> Vec<ScriptMeta> {
        let mut out = Vec::new();
        collect_scripts(&self.dir, 1, &mut out);
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn find(&self, name: &str) -> Option<ScriptMeta> {
        let want = normalize_name(name);
        self.discover().into_iter().find(|m| m.name == want)
    }

    /// Read a script in full, including its metadata header.
    pub fn load(&self, name: &str) -> Option<(ScriptMeta, String)> {
        let meta = self.find(name)?;
        let text = fs::read_to_string(&meta.path).ok()?;
        Some((meta, text))
    }

    /// Create or overwrite a script; returns (metadata, whether an existing file was replaced).
    /// The metadata header is generated unless the body already carries one.
    pub fn save(&self, spec: &ScriptSpec) -> Result<(ScriptMeta, bool)> {
        let slug = normalize_name(spec.name);
        let ext = extension_for(spec.lang, spec.name);
        let path = self.dir.join(format!("{}.{}", slug, ext));
        let existed = path.exists();
        let body = spec.body.trim_end();
        let content = if body.contains(BEGIN) {
            format!("{}\n", body)
        } else {
            let version = if spec.version.trim().is_empty() {
                if existed {
                    fs::read_to_string(&path)
                        .ok()
                        .and_then(|t| parse_metadata(&t).get("version").cloned())
                        .map(|v| bump_version(&v))
                        .unwrap_or_else(|| "1.0".to_string())
                } else {
                    "1.0".to_string()
                }
            } else {
                spec.version.trim().to_string()
            };
            let last_verified = spec.last_verified.trim();
            let lang = if spec.lang.trim().is_empty() {
                let inferred = lang_from_ext(&path);
                if inferred.is_empty() {
                    "bash".to_string()
                } else {
                    inferred
                }
            } else {
                spec.lang.trim().to_string()
            };
            let mut header = String::new();
            header.push_str("# --- vola-script ---\n");
            header.push_str(&format!("# name: {}\n", slug));
            header.push_str(&format!("# when: {}\n", spec.when.trim()));
            header.push_str(&format!("# lang: {}\n", lang));
            if !spec.deps.trim().is_empty() {
                header.push_str(&format!("# deps: {}\n", spec.deps.trim()));
            }
            if !spec.usage.trim().is_empty() {
                header.push_str(&format!("# usage: {}\n", spec.usage.trim()));
            }
            header.push_str(&format!("# version: {}\n", version));
            if !last_verified.is_empty() {
                header.push_str(&format!("# last_verified: {}\n", last_verified));
            }
            if !spec.status.trim().is_empty() {
                header.push_str(&format!("# status: {}\n", spec.status.trim()));
            }
            if !spec.platform.trim().is_empty() {
                header.push_str(&format!("# platform: {}\n", spec.platform.trim()));
            }
            header.push_str("# --- /vola-script ---\n\n");
            format!("{}{}\n", header, body)
        };
        fs::create_dir_all(&self.dir)?;
        fs::write(&path, content)
            .with_context(|| format!("failed to write script: {}", path.display()))?;
        let text = fs::read_to_string(&path).unwrap_or_default();
        Ok((meta_from_path(&path, &text), existed))
    }

    pub fn delete(&self, name: &str) -> Result<bool> {
        let Some(meta) = self.find(name) else {
            return Ok(false);
        };
        fs::remove_file(&meta.path)
            .with_context(|| format!("failed to delete script: {}", meta.path.display()))?;
        Ok(true)
    }

    /// Render the compact script index injected into the system prompt.
    pub fn render_index(&self, metas: &[ScriptMeta]) -> String {
        if metas.is_empty() {
            return "- (none; use script_save after a reusable task)".to_string();
        }
        metas
            .iter()
            .map(|m| {
                let mut line = format!("- {}", m.name);
                let mut tags = Vec::new();
                if !m.lang.is_empty() {
                    tags.push(m.lang.clone());
                }
                if !m.version.is_empty() {
                    tags.push(format!("v{}", m.version));
                }
                if !tags.is_empty() {
                    line.push_str(&format!(" ({})", tags.join(" ")));
                }
                if !m.when.is_empty() {
                    line.push_str(&format!(" — {}", m.when));
                }
                if !m.status.is_empty() {
                    line.push_str(&format!(" [{}]", m.status));
                }
                if !m.usage.is_empty() {
                    line.push_str(&format!("; usage: {}", m.usage));
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Regenerate `INDEX.md` from the files; only writes when the content changed.
    pub fn write_index(&self) -> Result<()> {
        let metas = self.discover();
        let content = format!(
            "# Scripts (generated; do not edit)\n\n{}\n",
            self.render_index(&metas)
        );
        let path = self.dir.join("INDEX.md");
        if fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
            fs::create_dir_all(&self.dir)?;
            fs::write(&path, content)
                .with_context(|| format!("failed to write script index: {}", path.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("vola-agent-scripts-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn spec<'a>(name: &'a str, body: &'a str) -> ScriptSpec<'a> {
        ScriptSpec {
            name,
            lang: "python",
            when: "split a pdf",
            deps: "pypdf",
            usage: "pdf-split.py <input.pdf>",
            version: "",
            last_verified: "",
            status: "",
            platform: "",
            body,
        }
    }

    #[test]
    fn parses_metadata_block() {
        let text = "# --- vola-script ---\n# name: pdf-split\n# when: split a pdf\n# lang: python\n# version: 1.3\n# --- /vola-script ---\n\nprint('hi')\n";
        let meta = parse_metadata(text);
        assert_eq!(meta.get("name").unwrap(), "pdf-split");
        assert_eq!(meta.get("when").unwrap(), "split a pdf");
        assert_eq!(meta.get("version").unwrap(), "1.3");
    }

    #[test]
    fn save_load_and_delete_roundtrip() {
        let dir = temp_dir("roundtrip");
        let scripts = Scripts::new(dir.clone());
        let (meta, existed) = scripts.save(&spec("pdf-split", "print('hi')")).unwrap();
        assert!(!existed);
        assert_eq!(meta.name, "pdf-split");
        assert_eq!(meta.version, "1.0");
        assert_eq!(meta.lang, "python");
        assert!(meta.path.ends_with("pdf-split.py"));
        let (loaded, text) = scripts.load("pdf-split.py").unwrap();
        assert_eq!(loaded.name, "pdf-split");
        assert!(text.contains("print('hi')"));
        assert!(text.contains("# --- vola-script ---"));
        // Updating the same name bumps the version.
        let (meta2, existed2) = scripts.save(&spec("pdf-split", "print('bye')")).unwrap();
        assert!(existed2);
        assert_eq!(meta2.version, "1.1");
        assert_eq!(scripts.discover().len(), 1);
        assert!(scripts
            .render_index(&scripts.discover())
            .contains("pdf-split"));
        assert!(scripts.delete("pdf-split").unwrap());
        assert!(scripts.discover().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovers_files_without_metadata() {
        let dir = temp_dir("plain");
        let scripts = Scripts::new(dir.clone());
        fs::write(dir.join("port-check.sh"), "echo ok\n").unwrap();
        let metas = scripts.discover();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].name, "port-check");
        assert_eq!(metas[0].lang, "bash");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_is_generated_and_skipped_by_discovery() {
        let dir = temp_dir("index");
        let scripts = Scripts::new(dir.clone());
        scripts.save(&spec("pdf-split", "print('hi')")).unwrap();
        scripts.write_index().unwrap();
        assert!(dir.join("INDEX.md").exists());
        assert_eq!(scripts.discover().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
