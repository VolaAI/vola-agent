//! File reads, writes and directory listing (relative paths resolve against the project workdir).

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::util::{char_len, truncate_middle};

pub fn resolve(cfg: &Config, raw: &str) -> PathBuf {
    let trimmed = raw.trim().trim_matches('"');
    if let Some(rest) = trimmed.strip_prefix('~') {
        let rest = rest.trim_start_matches(['/', '\\']);
        if rest.is_empty() {
            return cfg.home.clone();
        }
        return cfg.home.join(rest);
    }
    let p = PathBuf::from(trimmed);
    if p.is_absolute() {
        p
    } else {
        cfg.home.join(p)
    }
}

pub fn read_file(cfg: &Config, path: &str, offset: usize, limit: usize) -> Result<String> {
    let p = resolve(cfg, path);
    let bytes = fs::read(&p).with_context(|| format!("failed to read: {}", p.display()))?;
    let text = String::from_utf8_lossy(&bytes).to_string();
    let total_lines = text.lines().count();
    let total_chars = char_len(&text);
    if offset == 0 && total_chars <= cfg.output_limit {
        return Ok(format!(
            "[file] {}\n[lines] {}\n[chars] {}\n\n{}",
            p.display(),
            total_lines,
            total_chars,
            text
        ));
    }
    let start = offset.max(1) - 1;
    let end = start.saturating_add(limit.max(1));
    let chunk: String = text
        .lines()
        .skip(start)
        .take(limit.max(1))
        .enumerate()
        .map(|(i, line)| format!("{:>5}| {}\n", start + i + 1, line))
        .collect();
    let (chunk, cut) = truncate_middle(&chunk, cfg.output_limit);
    Ok(format!(
        "[file] {}\n[total lines] {}  showing lines {}..{}{}\n\n{}{}",
        p.display(),
        total_lines,
        start + 1,
        end.min(total_lines.max(end)),
        if cut { " (truncated)" } else { "" },
        chunk,
        if total_lines > end {
            format!("\n[hint] {} lines remain; continue with offset={}", total_lines - end, end + 1)
        } else {
            String::new()
        }
    ))
}

pub fn write(cfg: &Config, path: &str, content: &str) -> Result<String> {
    let p = resolve(cfg, path);
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).with_context(|| format!("failed to create the directory: {}", dir.display()))?;
    }
    fs::write(&p, content).with_context(|| format!("write failed: {}", p.display()))?;
    Ok(format!(
        "[written] {}\n[bytes] {}\n[lines] {}",
        p.display(),
        content.len(),
        content.lines().count()
    ))
}

pub fn list_dir(cfg: &Config, path: &str, depth: usize) -> Result<String> {
    let root = resolve(cfg, if path.trim().is_empty() { "." } else { path });
    if !root.exists() {
        anyhow::bail!("directory does not exist: {}", root.display());
    }
    let mut out = String::new();
    out.push_str(&format!("[directory] {}\n", root.display()));
    let mut count = 0usize;
    walk(&root, 0, depth.max(1).min(4), &mut out, &mut count)?;
    if count == 0 {
        out.push_str("(empty directory)\n");
    }
    let (text, cut) = truncate_middle(&out, cfg.output_limit);
    Ok(format!("{}{}", text, if cut { "\n[truncated: directory too large, narrow the scope]" } else { "" }))
}

fn walk(dir: &Path, level: usize, max_depth: usize, out: &mut String, count: &mut usize) -> Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        out.push_str(&format!("{}(unreadable)\n", indent(level)));
        return Ok(());
    };
    let mut items: Vec<_> = entries.flatten().collect();
    items.sort_by_key(|e| e.file_name());
    for entry in items {
        if *count >= 400 {
            out.push_str(&format!("{}... (listing too long, stopped)\n", indent(level)));
            return Ok(());
        }
        *count += 1;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        if is_dir {
            out.push_str(&format!("{}{}/\n", indent(level), name));
            if level + 1 < max_depth {
                walk(&path, level + 1, max_depth, out, count)?;
            }
        } else {
            out.push_str(&format!("{}{}  ({} B)\n", indent(level), name, size));
        }
    }
    Ok(())
}

fn indent(level: usize) -> String {
    "  ".repeat(level)
}
