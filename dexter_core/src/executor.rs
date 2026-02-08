use crate::context::FileContext;
use crate::llm::LlmClient;
use crate::redaction::redact_sensitive_text;
use crate::safety::SafetyGuard;
use crate::CachePolicy;
use anyhow::{Context, Result};
use chrono::Utc;
use dexter_plugins::Plugin;
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use tokio::fs::{self, create_dir_all, OpenOptions};
use tokio::io::AsyncWriteExt;

pub struct Executor {
    llm_client: LlmClient,
    safety_guard: SafetyGuard,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub timestamp: String,
    pub plugin: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinnedHistoryEntry {
    pub timestamp: String,
    pub plugin: String,
    pub command: String,
    pub pinned_at: String,
}

impl Executor {
    pub fn new(llm_client: LlmClient) -> Self {
        Self {
            llm_client,
            safety_guard: SafetyGuard::default(),
        }
    }

    pub fn llm_client(&self) -> &LlmClient {
        &self.llm_client
    }

    pub async fn generate_command(
        &self,
        user_input: &str,
        context: &FileContext,
        plugin: &dyn Plugin,
    ) -> Result<String> {
        self.generate_command_with_policy(user_input, context, plugin, CachePolicy::Normal)
            .await
    }

    pub async fn generate_command_with_policy(
        &self,
        user_input: &str,
        context: &FileContext,
        plugin: &dyn Plugin,
        cache_policy: CachePolicy,
    ) -> Result<String> {
        let context_str = if let Some(summary) = &context.summary {
            summary.clone()
        } else {
            context
                .files
                .iter()
                .enumerate()
                .map(|(i, f)| format!("{}. {}", i + 1, f))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let system_prompt = plugin.get_executor_prompt(&context_str, user_input);

        let command = self
            .llm_client
            .completion_with_policy(
                &system_prompt,
                "Please generate the exact command based on the instructions above.",
                cache_policy,
            )
            .await?;
        let clean_command = command
            .trim()
            .replace("```bash", "")
            .replace("```", "")
            .trim()
            .to_string();

        // Safety Check
        self.safety_guard.check(&clean_command)?;

        // Plugin specific validation
        if !plugin.validate_command(&clean_command) {
            return Err(anyhow::anyhow!("Command failed plugin validation logic"));
        }

        Ok(clean_command)
    }

    pub async fn record_history(&self, plugin_name: &str, command: &str) -> Result<()> {
        let history_dir = history_dir()?;
        if !history_dir.exists() {
            create_dir_all(&history_dir).await?;
        }

        let history_path = history_dir.join("history.jsonl");
        let redacted_command = redact_sensitive_text(command);
        let entry = HistoryEntry {
            timestamp: Utc::now().to_rfc3339(),
            plugin: plugin_name.to_string(),
            command: redacted_command,
        };

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(history_path)
            .await?;

        let line = serde_json::to_string(&entry)?;
        file.write_all(format!("{}\n", line).as_bytes()).await?;
        Ok(())
    }

    pub async fn load_history_entries(&self) -> Result<Vec<HistoryEntry>> {
        let path = history_path()?;
        let (entries, skipped_lines) = load_history_entries_from_path(&path).await?;
        if skipped_lines > 0 {
            tracing::warn!(
                "Skipped {} invalid history line(s) in {}",
                skipped_lines,
                path.display()
            );
        }
        Ok(entries)
    }

    pub async fn load_pinned_entries(&self) -> Result<Vec<PinnedHistoryEntry>> {
        let path = pin_path()?;
        load_pinned_entries_from_path(&path).await
    }

    pub async fn set_pin(&self, entry: &HistoryEntry) -> Result<()> {
        let path = pin_path()?;
        set_pin_in_path(&path, entry).await
    }

    pub async fn unset_pin(&self, entry: &HistoryEntry) -> Result<()> {
        let path = pin_path()?;
        unset_pin_in_path(&path, entry).await
    }
}

fn history_dir() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .context("Could not find data directory")?
        .join("dexter"))
}

fn history_path() -> Result<PathBuf> {
    Ok(history_dir()?.join("history.jsonl"))
}

fn pin_path() -> Result<PathBuf> {
    Ok(history_dir()?.join("history_pins.json"))
}

fn is_same_history_entry(entry: &HistoryEntry, pin: &PinnedHistoryEntry) -> bool {
    entry.timestamp == pin.timestamp && entry.plugin == pin.plugin && entry.command == pin.command
}

async fn load_history_entries_from_path(path: &Path) -> Result<(Vec<HistoryEntry>, usize)> {
    let content = match fs::read_to_string(path).await {
        Ok(content) => content,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(err) => return Err(err.into()),
    };

    let mut entries = Vec::new();
    let mut skipped_lines = 0usize;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<HistoryEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(_) => skipped_lines += 1,
        }
    }

    Ok((entries, skipped_lines))
}

async fn load_pinned_entries_from_path(path: &Path) -> Result<Vec<PinnedHistoryEntry>> {
    let content = match fs::read_to_string(path).await {
        Ok(content) => content,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };

    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let entries = serde_json::from_str::<Vec<PinnedHistoryEntry>>(trimmed)
        .with_context(|| format!("Invalid pin history JSON at {}", path.display()))?;
    Ok(entries)
}

async fn write_pinned_entries_atomic(path: &Path, entries: &[PinnedHistoryEntry]) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent).await?;
    }

    let data = serde_json::to_string_pretty(entries)?;
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, data).await?;
    fs::rename(&tmp_path, path).await?;
    Ok(())
}

async fn set_pin_in_path(path: &Path, entry: &HistoryEntry) -> Result<()> {
    let mut pins = match load_pinned_entries_from_path(path).await {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(
                "Pin file is invalid at {}. Rebuilding from empty: {}",
                path.display(),
                err
            );
            Vec::new()
        }
    };

    pins.retain(|pin| !is_same_history_entry(entry, pin));
    pins.push(PinnedHistoryEntry {
        timestamp: entry.timestamp.clone(),
        plugin: entry.plugin.clone(),
        command: entry.command.clone(),
        pinned_at: Utc::now().to_rfc3339(),
    });

    write_pinned_entries_atomic(path, &pins).await
}

async fn unset_pin_in_path(path: &Path, entry: &HistoryEntry) -> Result<()> {
    let path_exists = path.exists();
    let mut pins = match load_pinned_entries_from_path(path).await {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(
                "Pin file is invalid at {}. Resetting to empty: {}",
                path.display(),
                err
            );
            Vec::new()
        }
    };

    pins.retain(|pin| !is_same_history_entry(entry, pin));
    if !path_exists && pins.is_empty() {
        return Ok(());
    }

    write_pinned_entries_atomic(path, &pins).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redaction::redact_sensitive_text;
    use tempfile::tempdir;

    #[test]
    fn redact_masks_cookies_and_tokens() {
        let input = r#"yt-dlp --cookies cookies.txt "https://a.com/v?id=1&token=abc123""#;
        let redacted = redact_sensitive_text(input);
        assert!(!redacted.contains("cookies.txt"));
        assert!(!redacted.contains("abc123"));
        assert!(redacted.contains("--cookies [REDACTED]"));
        assert!(redacted.contains("token=[REDACTED]"));
    }

    #[tokio::test]
    async fn load_history_entries_skips_invalid_lines() {
        let tmp = tempdir().unwrap();
        let history_path = tmp.path().join("history.jsonl");
        let valid_a = HistoryEntry {
            timestamp: "2026-02-08T10:00:00Z".to_string(),
            plugin: "f2".to_string(),
            command: "f2 -f old new".to_string(),
        };
        let valid_b = HistoryEntry {
            timestamp: "2026-02-08T11:00:00Z".to_string(),
            plugin: "ffmpeg".to_string(),
            command: "ffmpeg -i a.mp4 b.mp3".to_string(),
        };
        let payload = format!(
            "{}\n{{broken json}}\n{}\n",
            serde_json::to_string(&valid_a).unwrap(),
            serde_json::to_string(&valid_b).unwrap(),
        );
        fs::write(&history_path, payload).await.unwrap();

        let (entries, skipped) = load_history_entries_from_path(&history_path).await.unwrap();
        assert_eq!(entries, vec![valid_a, valid_b]);
        assert_eq!(skipped, 1);
    }

    #[tokio::test]
    async fn pin_and_unpin_roundtrip() {
        let tmp = tempdir().unwrap();
        let pins_path = tmp.path().join("history_pins.json");
        let entry = HistoryEntry {
            timestamp: "2026-02-08T10:00:00Z".to_string(),
            plugin: "f2".to_string(),
            command: "f2 -f old new".to_string(),
        };

        set_pin_in_path(&pins_path, &entry).await.unwrap();
        let pins_after_set = load_pinned_entries_from_path(&pins_path).await.unwrap();
        assert_eq!(pins_after_set.len(), 1);
        assert_eq!(pins_after_set[0].timestamp, entry.timestamp);
        assert_eq!(pins_after_set[0].plugin, entry.plugin);
        assert_eq!(pins_after_set[0].command, entry.command);
        assert!(!pins_after_set[0].pinned_at.is_empty());

        unset_pin_in_path(&pins_path, &entry).await.unwrap();
        let pins_after_unset = load_pinned_entries_from_path(&pins_path).await.unwrap();
        assert!(pins_after_unset.is_empty());
    }
}
