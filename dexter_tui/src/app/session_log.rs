use dexter_core::redact_sensitive_text;
use std::fs::{create_dir_all, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct SessionLogger {
    path: Option<PathBuf>,
    writer: Mutex<Option<BufWriter<File>>>,
}

impl SessionLogger {
    pub fn new() -> Self {
        let (path, writer) = if let Some(log_path) = build_log_path() {
            if let Some(parent) = log_path.parent() {
                let _ = create_dir_all(parent);
            }
            let writer = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .ok()
                .map(BufWriter::new);
            if writer.is_some() {
                (Some(log_path), writer)
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        Self {
            path,
            writer: Mutex::new(writer),
        }
    }

    pub fn display_path(&self) -> Option<String> {
        self.path.as_ref().map(|p| p.display().to_string())
    }

    pub fn event(&self, label: &str, message: &str) {
        let sanitized = sanitize_for_log(message);
        let bounded = truncate_with_notice(&sanitized, MAX_EVENT_BYTES);
        let line = format!("[{}] {} {}", now_millis(), label, bounded);
        self.append_line(&line);
    }

    pub fn block(&self, label: &str, body: &str) {
        self.append_line(&format!("[{}] {} BEGIN", now_millis(), label));
        let sanitized = sanitize_for_log(body);
        let bounded = truncate_with_notice(&sanitized, MAX_BLOCK_BYTES);
        for line in bounded.lines() {
            self.append_line(line);
        }
        self.append_line(&format!("[{}] {} END", now_millis(), label));
    }

    fn append_line(&self, line: &str) {
        let Ok(mut guard) = self.writer.lock() else {
            return;
        };
        let Some(writer) = guard.as_mut() else {
            return;
        };
        if writeln!(writer, "{}", line).is_ok() {
            let _ = writer.flush();
        }
    }
}

fn build_log_path() -> Option<PathBuf> {
    let base = dirs::data_dir().or_else(|| std::env::current_dir().ok())?;
    let mut path = base.join("dexter").join("logs");
    let ts = now_millis();
    path.push(format!("session-{}.log", ts));
    Some(path)
}

const MAX_EVENT_BYTES: usize = 4096;
const MAX_BLOCK_BYTES: usize = 65536;

fn sanitize_for_log(input: &str) -> String {
    redact_sensitive_text(input)
}

fn truncate_with_notice(input: &str, limit: usize) -> String {
    if input.len() <= limit {
        return input.to_string();
    }

    let mut out = String::new();
    for ch in input.chars() {
        if out.len() + ch.len_utf8() > limit.saturating_sub(64) {
            break;
        }
        out.push(ch);
    }
    let omitted = input.len().saturating_sub(out.len());
    out.push_str(&format!("\n...[truncated {} bytes]", omitted));
    out
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_masks_common_secrets() {
        let raw = r#"Authorization: Bearer abc123token
x-api-key: supersecret
https://a.com/path?token=abc&x=1
yt-dlp --cookies cookies.txt "https://a.com""#;

        let masked = sanitize_for_log(raw);
        assert!(!masked.contains("abc123token"));
        assert!(!masked.contains("supersecret"));
        assert!(!masked.contains("cookies.txt"));
        assert!(masked.contains("Authorization: Bearer [REDACTED]"));
        assert!(masked.contains("x-api-key: [REDACTED]"));
        assert!(masked.contains("token=[REDACTED]"));
    }

    #[test]
    fn truncate_adds_notice_when_over_limit() {
        let s = "x".repeat(1000);
        let out = truncate_with_notice(&s, 120);
        assert!(out.contains("[truncated"));
        assert!(out.len() < 260);
    }
}
