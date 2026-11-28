//! Skill library: Markdown files with frontmatter under `agent/skills/`.
//! The root `SKILL.md` is the library manager (conventions for adding/updating skills);
//! individual skills live in `references/`. Files are the single source of truth; the DB
//! only keeps the index and counts.

use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::util::slugify;

#[derive(Clone, Debug, Serialize)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub tags: String,
    pub path: PathBuf,
}

pub struct Skills {
    dir: PathBuf,
}

/// Parse the frontmatter (keys as `key: value` between --- fences), returning metadata and body.
pub fn parse_frontmatter(text: &str) -> (BTreeMap<String, String>, String) {
    let mut meta = BTreeMap::new();
    let normalized = text.replace("\r\n", "\n");
    if !normalized.starts_with("---\n") {
        return (meta, normalized);
    }
    let rest = &normalized[4..];
    match rest.find("\n---") {
        Some(end) => {
            for line in rest[..end].lines() {
                if let Some((k, v)) = line.split_once(':') {
                    meta.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
                }
            }
            let body_start = 4 + end + 4;
            let body = normalized[body_start..]
                .trim_start_matches('\n')
                .to_string();
            (meta, body)
        }
        None => (meta, normalized),
    }
}

impl Skills {
    pub fn new(dir: PathBuf) -> Skills {
        let _ = fs::create_dir_all(&dir);
        Skills { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Directory holding individual skills; `skill_save` writes here so the root
    /// `SKILL.md` stays the library's manager page.
    pub fn references_dir(&self) -> PathBuf {
        self.dir.join("references")
    }

    pub fn discover(&self) -> Vec<SkillMeta> {
        let mut out = Vec::new();
        collect_skills(&self.dir, 2, &mut out);
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Read a skill in full, including the body outside the frontmatter.
    pub fn load(&self, name: &str) -> Option<(SkillMeta, String)> {
        let want = slugify(name);
        let metas = self.discover();
        let meta = metas.into_iter().find(|m| m.name == want)?;
        let text = fs::read_to_string(&meta.path).ok()?;
        let (_, body) = parse_frontmatter(&text);
        Some((meta, body))
    }

    /// Create or overwrite a skill; returns (metadata, whether an existing file was replaced).
    pub fn save(
        &self,
        name: &str,
        description: &str,
        tags: &str,
        body: &str,
    ) -> Result<(SkillMeta, bool)> {
        let slug = slugify(name);
        if self.is_manager_name(&slug) {
            anyhow::bail!(
                "`{}` is the skill library manager page; edit {} with write_file instead",
                slug,
                self.dir.join("SKILL.md").display()
            );
        }
        let dir = self.references_dir();
        let path = dir.join(format!("{}.md", slug));
        let existed = path.exists();
        let content = format!(
            "---\nname: {}\ndescription: {}\ntags: {}\n---\n\n{}\n",
            slug,
            description.trim(),
            tags.trim(),
            body.trim_end()
        );
        fs::create_dir_all(&dir)?;
        fs::write(&path, content)
            .with_context(|| format!("failed to write skill: {}", path.display()))?;
        Ok((
            SkillMeta {
                name: slug,
                description: description.trim().to_string(),
                tags: tags.trim().to_string(),
                path,
            },
            existed,
        ))
    }

    /// True when `slug` is the name declared by the root manager page (`SKILL.md`).
    fn is_manager_name(&self, slug: &str) -> bool {
        let path = self.dir.join("SKILL.md");
        let Ok(text) = fs::read_to_string(&path) else {
            return false;
        };
        let (meta, _) = parse_frontmatter(&text);
        meta.get("name")
            .map(|n| slugify(n) == slug)
            .unwrap_or(false)
    }

    /// Delete a skill file. The manager page (`SKILL.md`) is protected.
    /// Returns the deleted metadata, or `None` when no such skill exists.
    pub fn delete(&self, name: &str) -> Result<Option<SkillMeta>> {
        let want = slugify(name);
        let Some(meta) = self.discover().into_iter().find(|m| m.name == want) else {
            return Ok(None);
        };
        if meta.path == self.dir.join("SKILL.md") || self.is_manager_name(&meta.name) {
            anyhow::bail!(
                "refusing to delete the skill library manager page ({}); edit it with write_file instead",
                meta.path.display()
            );
        }
        fs::remove_file(&meta.path)
            .with_context(|| format!("failed to delete skill: {}", meta.path.display()))?;
        Ok(Some(meta))
    }

    /// Render the skill index injected into the system prompt.
    pub fn render_index(&self, metas: &[SkillMeta]) -> String {
        if metas.is_empty() {
            return "- (no skills yet. After a reusable task, create one with skill_save.)"
                .to_string();
        }
        metas
            .iter()
            .map(|m| {
                if m.description.is_empty() {
                    format!("- **{}** (`{}`) ", m.name, m.path.display())
                } else {
                    format!(
                        "- **{}** — {} (`{}`) ",
                        m.name,
                        m.description,
                        m.path.display()
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("skill")
        .to_string()
}

/// Walk `dir` (and its subdirectories up to `depth`) collecting every `*.md` skill.
/// The root `SKILL.md` and everything under `references/` are all part of the library.
fn collect_skills(dir: &Path, depth: usize, out: &mut Vec<SkillMeta>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                collect_skills(&path, depth - 1, out);
            }
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let (meta, _) = parse_frontmatter(&text);
        let name = meta
            .get("name")
            .cloned()
            .unwrap_or_else(|| file_stem(&path));
        let description = meta.get("description").cloned().unwrap_or_default();
        let tags = meta.get("tags").cloned().unwrap_or_default();
        out.push(SkillMeta {
            name: slugify(&name),
            description,
            tags,
            path,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_parsing() {
        let text = "---\nname: demo\ndescription: demo skill\ntags: a,b\n---\n\nbody line one\nbody line two\n";
        let (meta, body) = parse_frontmatter(text);
        assert_eq!(meta.get("name").unwrap(), "demo");
        assert_eq!(meta.get("description").unwrap(), "demo skill");
        assert!(body.starts_with("body line one"));
        assert!(body.contains("body line two"));
    }

    #[test]
    fn no_frontmatter_keeps_body() {
        let (meta, body) = parse_frontmatter("body only");
        assert!(meta.is_empty());
        assert_eq!(body, "body only");
    }

    #[test]
    fn save_and_load_roundtrip() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("vola-agent-skills-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let skills = Skills::new(dir.clone());
        let (meta, existed) = skills
            .save(
                "Demo Skill",
                "for demonstration",
                "demo",
                "step one\nstep two",
            )
            .unwrap();
        assert!(!existed);
        assert_eq!(meta.name, "demo-skill");
        let (meta2, body) = skills.load("demo-skill").unwrap();
        assert_eq!(meta2.description, "for demonstration");
        assert!(body.contains("step two"));
        // overwrite the existing file
        let (_, existed2) = skills
            .save("demo-skill", "new description", "", "new body")
            .unwrap();
        assert!(existed2);
        assert_eq!(skills.discover().len(), 1);
        assert!(skills
            .render_index(&skills.discover())
            .contains("demo-skill"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_removes_a_skill_but_not_the_manager_page() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("vola-agent-skills-delete-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let skills = Skills::new(dir.clone());
        fs::create_dir_all(skills.references_dir()).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: skill-library\ndescription: manager\n---\n\nbody\n",
        )
        .unwrap();
        skills.save("demo", "demo skill", "", "body").unwrap();
        assert!(skills.save("skill-library", "dup", "", "body").is_err());
        let deleted = skills.delete("demo").unwrap().expect("demo deleted");
        assert_eq!(deleted.name, "demo");
        assert!(!skills.references_dir().join("demo.md").exists());
        assert!(skills.delete("demo").unwrap().is_none());
        assert!(skills.delete("skill-library").is_err());
        assert!(dir.join("SKILL.md").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovers_root_manager_and_references_skills() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("vola-agent-skills-tree-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let skills = Skills::new(dir.clone());
        fs::create_dir_all(skills.references_dir()).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: skill-library\ndescription: manager\n---\n\nbody\n",
        )
        .unwrap();
        fs::write(
            skills.references_dir().join("demo.md"),
            "---\nname: demo\ndescription: under references\n---\n\nbody\n",
        )
        .unwrap();
        let names: Vec<String> = skills.discover().into_iter().map(|m| m.name).collect();
        assert_eq!(names, vec!["demo".to_string(), "skill-library".to_string()]);
        // Self-learned skills are written into references/, never next to the manager page.
        skills.save("fresh", "", "", "body").unwrap();
        assert!(skills.references_dir().join("fresh.md").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
