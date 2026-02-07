use crate::context::FileContext;
use crate::llm::LlmClient;
use crate::safety::SafetyGuard;
use crate::CachePolicy;
use anyhow::{Context, Result};
use dexter_plugins::Plugin;
use regex::Regex;
use serde_json::json;
use std::sync::OnceLock;
use tokio::fs::{create_dir_all, OpenOptions};
use tokio::io::AsyncWriteExt;

pub struct Executor {
    llm_client: LlmClient,
    safety_guard: SafetyGuard,
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
        let history_dir = dirs::data_dir()
            .context("Could not find data directory")?
            .join("dexter");

        if !history_dir.exists() {
            create_dir_all(&history_dir).await?;
        }

        let history_path = history_dir.join("history.jsonl");
        let redacted_command = redact_sensitive(command);
        let entry = json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "plugin": plugin_name,
            "command": redacted_command,
        });

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(history_path)
            .await?;

        file.write_all(format!("{}\n", entry).as_bytes()).await?;
        Ok(())
    }
}

fn redact_sensitive(command: &str) -> String {
    static COOKIES_RE: OnceLock<Regex> = OnceLock::new();
    static AUTH_RE: OnceLock<Regex> = OnceLock::new();
    static QUERY_TOKEN_RE: OnceLock<Regex> = OnceLock::new();

    let cookies_re = COOKIES_RE
        .get_or_init(|| Regex::new(r#"(?i)(--cookies(?:=|\s+))("[^"]*"|'[^']*'|\S+)"#).unwrap());
    let auth_re = AUTH_RE.get_or_init(|| {
        Regex::new(r#"(?i)(authorization\s*:\s*bearer\s+)([A-Za-z0-9._~+/=-]+)"#).unwrap()
    });
    let query_token_re = QUERY_TOKEN_RE.get_or_init(|| {
        Regex::new(r#"(?i)([?&](?:token|access_token|api_key|apikey|key)=)([^&\s"']+)"#).unwrap()
    });

    let step1 = cookies_re.replace_all(command, "$1[REDACTED]").to_string();
    let step2 = auth_re.replace_all(&step1, "$1[REDACTED]").to_string();
    query_token_re
        .replace_all(&step2, "$1[REDACTED]")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_masks_cookies_and_tokens() {
        let input = r#"yt-dlp --cookies cookies.txt "https://a.com/v?id=1&token=abc123""#;
        let redacted = redact_sensitive(input);
        assert!(!redacted.contains("cookies.txt"));
        assert!(!redacted.contains("abc123"));
        assert!(redacted.contains("--cookies [REDACTED]"));
        assert!(redacted.contains("token=[REDACTED]"));
    }
}
