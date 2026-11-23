//! SQLite short-term memory: sessions, messages, compression summaries, long-term notes, skill index.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

use crate::llm::ToolCall;
use crate::util::{fmt_time, now_secs};

#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub tool_name: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: i64,
    pub title: String,
    pub model: String,
    pub updated_at: i64,
    pub msg_count: i64,
}

#[derive(Debug, Clone)]
pub struct NoteRow {
    pub id: i64,
    pub key: Option<String>,
    pub content: String,
    pub tags: String,
    pub pinned: bool,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct SkillRow {
    pub name: String,
    pub description: String,
    pub path: String,
    pub use_count: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct SummaryRow {
    pub upto_message_id: i64,
    pub text: String,
}

pub struct MemoryStore {
    conn: Connection,
}

impl MemoryStore {
    pub fn open(path: &Path) -> Result<MemoryStore> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create the database directory: {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open the database: {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        let mut store = MemoryStore { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sessions(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL DEFAULT '',
                model TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL DEFAULT '',
                tool_name TEXT,
                tool_calls TEXT,
                tool_call_id TEXT,
                compressed INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, id);
            CREATE TABLE IF NOT EXISTS summaries(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL,
                upto_message_id INTEGER NOT NULL,
                text TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_summaries_session ON summaries(session_id, id);
            CREATE TABLE IF NOT EXISTS notes(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                key TEXT,
                content TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '',
                pinned INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS skills(
                name TEXT PRIMARY KEY,
                description TEXT NOT NULL DEFAULT '',
                path TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                use_count INTEGER NOT NULL DEFAULT 0,
                last_used_at INTEGER
            );
            "#,
        )?;
        // Older databases predate tool_call_id; add it without losing data.
        self.ensure_column("messages", "tool_call_id", "TEXT")?;
        Ok(())
    }

    fn ensure_column(&self, table: &str, column: &str, ddl: &str) -> Result<()> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({})", table))?;
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if !names.iter().any(|name| name == column) {
            self.conn.execute(
                &format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, ddl),
                [],
            )?;
        }
        Ok(())
    }

    // ---------- sessions ----------

    pub fn new_session(&mut self, title: &str, model: &str) -> Result<i64> {
        let now = now_secs();
        self.conn.execute(
            "INSERT INTO sessions(title, model, created_at, updated_at) VALUES(?1, ?2, ?3, ?3)",
            params![title, model, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn sessions(&self, limit: i64) -> Result<Vec<SessionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.title, s.model, s.updated_at,
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id)
             FROM sessions s ORDER BY s.updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(SessionRow {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    model: r.get(2)?,
                    updated_at: r.get(3)?,
                    msg_count: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn session_title(&self, id: i64) -> Option<String> {
        self.conn
            .query_row("SELECT title FROM sessions WHERE id = ?1", params![id], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_title_if_empty(&mut self, id: i64, title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET title = ?2 WHERE id = ?1 AND (title = '' OR title IS NULL)",
            params![id, title],
        )?;
        Ok(())
    }

    pub fn touch_session(&mut self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
            params![id, now_secs()],
        )?;
        Ok(())
    }

    // ---------- messages ----------

    pub fn append_message(
        &mut self,
        session_id: i64,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        tool_calls: Option<&[ToolCall]>,
        tool_call_id: Option<&str>,
    ) -> Result<i64> {
        let calls_json = tool_calls.map(|c| serde_json::to_string(c).unwrap_or_else(|_| "[]".into()));
        let now = now_secs();
        self.conn.execute(
            "INSERT INTO messages(session_id, role, content, tool_name, tool_calls, tool_call_id, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![session_id, role, content, tool_name, calls_json, tool_call_id, now],
        )?;
        self.touch_session(session_id)?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn messages_after(&self, session_id: i64, after_id: i64) -> Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, role, content, tool_name, tool_calls, tool_call_id, created_at
             FROM messages
             WHERE session_id = ?1 AND id > ?2 AND compressed = 0
             ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map(params![session_id, after_id], |r| {
                let calls_json: Option<String> = r.get(4)?;
                let tool_calls = calls_json
                    .and_then(|s| serde_json::from_str::<Vec<ToolCall>>(&s).ok())
                    .filter(|v| !v.is_empty());
                Ok(StoredMessage {
                    id: r.get(0)?,
                    role: r.get(1)?,
                    content: r.get(2)?,
                    tool_name: r.get(3)?,
                    tool_calls,
                    tool_call_id: r.get(5)?,
                    created_at: r.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn message_count(&self, session_id: i64) -> Result<i64> {
        let n = self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    // ---------- compression summaries ----------

    pub fn latest_summary(&self, session_id: i64) -> Result<Option<SummaryRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT upto_message_id, text FROM summaries
                 WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                params![session_id],
                |r| {
                    Ok(SummaryRow {
                        upto_message_id: r.get(0)?,
                        text: r.get(1)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn add_summary(&mut self, session_id: i64, upto_message_id: i64, text: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO summaries(session_id, upto_message_id, text, created_at)
             VALUES(?1, ?2, ?3, ?4)",
            params![session_id, upto_message_id, text, now_secs()],
        )?;
        self.conn.execute(
            "UPDATE messages SET compressed = 1 WHERE session_id = ?1 AND id <= ?2",
            params![session_id, upto_message_id],
        )?;
        Ok(())
    }

    pub fn summary_count(&self, session_id: i64) -> Result<i64> {
        let n = self.conn.query_row(
            "SELECT COUNT(*) FROM summaries WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    // ---------- long-term memory ----------

    pub fn add_note(
        &mut self,
        content: &str,
        tags: &str,
        pinned: bool,
        key: Option<&str>,
    ) -> Result<i64> {
        let now = now_secs();
        self.conn.execute(
            "INSERT INTO notes(key, content, tags, pinned, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?5)",
            params![key, content, tags, pinned as i64, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn search_notes(&self, query: &str, limit: i64) -> Result<Vec<NoteRow>> {
        let pattern = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            "SELECT id, key, content, tags, pinned, updated_at FROM notes
             WHERE content LIKE ?1 OR tags LIKE ?1 OR IFNULL(key,'') LIKE ?1
             ORDER BY pinned DESC, updated_at DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![pattern, limit], |r| {
                Ok(NoteRow {
                    id: r.get(0)?,
                    key: r.get(1)?,
                    content: r.get(2)?,
                    tags: r.get(3)?,
                    pinned: r.get::<_, i64>(4)? != 0,
                    updated_at: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn pinned_notes(&self, limit: i64) -> Result<Vec<NoteRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, key, content, tags, pinned, updated_at FROM notes
             WHERE pinned = 1 ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(NoteRow {
                    id: r.get(0)?,
                    key: r.get(1)?,
                    content: r.get(2)?,
                    tags: r.get(3)?,
                    pinned: r.get::<_, i64>(4)? != 0,
                    updated_at: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete_note(&mut self, id: i64) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM notes WHERE id = ?1", params![id])? > 0)
    }

    // ---------- skill index ----------

    pub fn upsert_skill(&mut self, name: &str, description: &str, path: &str) -> Result<()> {
        let now = now_secs();
        self.conn.execute(
            "INSERT INTO skills(name, description, path, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(name) DO UPDATE SET description = excluded.description,
                 path = excluded.path, updated_at = excluded.updated_at",
            params![name, description, path, now],
        )?;
        Ok(())
    }

    pub fn touch_skill(&mut self, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE skills SET use_count = use_count + 1, last_used_at = ?2 WHERE name = ?1",
            params![name, now_secs()],
        )?;
        Ok(())
    }

    pub fn skills(&self) -> Result<Vec<SkillRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, description, path, use_count, updated_at FROM skills
             ORDER BY use_count DESC, name ASC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SkillRow {
                    name: r.get(0)?,
                    description: r.get(1)?,
                    path: r.get(2)?,
                    use_count: r.get(3)?,
                    updated_at: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn stats(&self) -> Result<Stats> {
        let sessions: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
        let messages: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
        let summaries: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM summaries", [], |r| r.get(0))?;
        let notes: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))?;
        let skills: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM skills", [], |r| r.get(0))?;
        Ok(Stats {
            sessions,
            messages,
            summaries,
            notes,
            skills,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Stats {
    pub sessions: i64,
    pub messages: i64,
    pub summaries: i64,
    pub notes: i64,
    pub skills: i64,
}

/// Compact rendering of session messages (compressor input, and what the REPL prints).
pub fn render_message(role: &str, content: &str, tool_name: Option<&str>) -> String {
    match role {
        "user" => format!("User: {}", content),
        "assistant" => {
            if content.trim().is_empty() {
                "Assistant: (tool calls)".to_string()
            } else {
                format!("Assistant: {}", content)
            }
        }
        "tool" => format!(
            "[tool {} result] {}",
            tool_name.unwrap_or("?"),
            content
        ),
        other => format!("{}: {}", other, content),
    }
}

pub fn session_line(s: &SessionRow) -> String {
    format!(
        "#{:>3}  {}  ({} messages, {}, {})",
        s.id,
        if s.title.is_empty() { "untitled" } else { &s.title },
        s.msg_count,
        s.model,
        fmt_time(s.updated_at)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ToolCallFunction;

    fn temp_db() -> (MemoryStore, std::path::PathBuf) {
        let mut p = std::env::temp_dir();
        let name = format!(
            "rs-agent-test-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        p.push(name);
        (MemoryStore::open(&p).unwrap(), p)
    }

    #[test]
    fn messages_and_summary_roundtrip() {
        let (mut store, path) = temp_db();
        let sid = store.new_session("test", "m").unwrap();
        let a = store.append_message(sid, "user", "hello", None, None, None).unwrap();
        let _b = store
            .append_message(
                sid,
                "assistant",
                "",
                None,
                Some(&[ToolCall {
                    id: None,
                    function: ToolCallFunction {
                        name: "run_cmd".into(),
                        arguments: serde_json::json!({"cmd": "ls"}),
                    },
                }]),
                None,
            )
            .unwrap();
        store.add_summary(sid, a, "summary text").unwrap();

        let msgs = store.messages_after(sid, 0).unwrap();
        // Compressed messages are no longer returned
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].tool_name, None);
        assert_eq!(store.latest_summary(sid).unwrap().unwrap().text, "summary text");

        let msgs_all = store.messages_after(sid, 0).unwrap();
        assert!(msgs_all.iter().all(|m| m.id > a));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn notes_search_and_pin() {
        let (mut store, path) = temp_db();
        store
            .add_note("User environment is Windows 11", "env", true, Some("env"))
            .unwrap();
        store.add_note("staged task progress", "todo", false, None).unwrap();
        let hits = store.search_notes("Windows", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].pinned);
        assert_eq!(store.pinned_notes(5).unwrap().len(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn skill_upsert_and_touch() {
        let (mut store, path) = temp_db();
        store.upsert_skill("rust-build", "Build a Rust project", "skills/rust-build.md").unwrap();
        store.upsert_skill("rust-build", "Build a Rust project (new description)", "skills/rust-build.md").unwrap();
        store.touch_skill("rust-build").unwrap();
        let skills = store.skills().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "Build a Rust project (new description)");
        assert_eq!(skills[0].use_count, 1);
        let _ = std::fs::remove_file(path);
    }
}
