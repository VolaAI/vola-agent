//! Small helpers: time, truncation, string handling.

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Show a timestamp in UTC+8 (override with VOLA_AGENT_UTC_OFFSET, in hours).
pub fn fmt_time(ts: i64) -> String {
    let offset: i64 = std::env::var("VOLA_AGENT_UTC_OFFSET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let local = ts + offset * 3600;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Truncate by character count, keeping the first 60% and the last 40%.
pub fn truncate_middle(s: &str, limit: usize) -> (String, bool) {
    let n = s.chars().count();
    if n <= limit || limit == 0 {
        return (s.to_string(), false);
    }
    let head_n = limit * 3 / 5;
    let tail_n = limit - head_n;
    let head: String = s.chars().take(head_n).collect();
    let tail: String = s
        .chars()
        .rev()
        .take(tail_n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    (
        format!(
            "{}\n...\n[output truncated] original length {} chars, kept the first {} and last {} chars\n...\n{}",
            head, n, head_n, tail_n, tail
        ),
        true,
    )
}

/// One-line summary: collapse newlines and extra whitespace, then truncate with an ellipsis once the text exceeds `limit` characters.
pub fn snippet(s: &str, limit: usize) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if limit == 0 || one.chars().count() <= limit {
        return one;
    }
    let mut out: String = one.chars().take(limit).collect();
    out.push_str("...");
    out
}

pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Reduce any name to a safe file/skill name: lowercase, alphanumerics and '-' only.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if c == '_' || c == '-' || c == ' ' || c == '.' || c == '/' {
            if !last_dash && !out.is_empty() {
                out.push('-');
                last_dash = true;
            }
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "skill".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_keeps_head_and_tail() {
        let s: String = "0123456789".repeat(20);
        let (out, cut) = truncate_middle(&s, 100);
        assert!(cut);
        assert!(out.starts_with("0123456789"));
        assert!(out.ends_with("0123456789"));
        assert!(out.contains("output truncated"));
    }

    #[test]
    fn snippet_collapses_newlines_and_marks_cut() {
        let s = "line one\n\nline two   line three";
        assert_eq!(snippet(s, 40), "line one line two line three");
        let long: String = "x".repeat(50);
        let cut = snippet(&long, 10);
        assert_eq!(cut.chars().count(), 13); // 10 characters + "..."
        assert!(cut.ends_with("..."));
    }

    #[test]
    fn short_strings_pass_through() {
        let (out, cut) = truncate_middle("hello", 10);
        assert_eq!(out, "hello");
        assert!(!cut);
    }

    #[test]
    fn slug_and_time() {
        assert_eq!(slugify("Rust Build/Env"), "rust-build-env");
        assert_eq!(fmt_time(0).starts_with("1970-01-01"), true);
    }
}
