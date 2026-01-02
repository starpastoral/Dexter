use crate::context::FileContext;
use crate::llm::LlmClient;
use crate::safety::SafetyGuard;
use anyhow::{Context, Result};
use dexter_plugins::Plugin;
use serde_json::json;
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
            .completion(&system_prompt, user_input)
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
        let entry = json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "plugin": plugin_name,
            "command": command,
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
