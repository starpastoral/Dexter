use crate::llm::LlmClient;
use crate::context::FileContext;
use crate::safety::SafetyGuard;
use dexter_plugins::Plugin;
use anyhow::{Result, Context};
use serde_json::json;
use tokio::fs::{OpenOptions, create_dir_all};
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
            context.files.iter()
                .enumerate()
                .map(|(i, f)| format!("{}. {}", i + 1, f))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let system_prompt = format!(
            r#"You are the Executor Agent for Dexter.
Your goal is to generate a specific, safe CLI command based on the User Intent and the selected Tool's documentation.

Selected Tool: {}
Description: {}

Tool Documentation:
{}

Current Working Directory Context:
{}

### HARD CONSTRAINTS (MUST FOLLOW):
1. CHARACTER PRECISION: The file list in the context is the ABSOLUTE TRUTH. 
   - Wave Dash (`〜`, U+301C) and Full-width Tilde (`～`, U+FF5E) are different.
   - You MUST match the EXACT character code from the context.
   - TIP: Copy and paste the character from the context. Do NOT type what you think it is.
   - If the name has ambiguous symbols, you may use the dot wildcard `.` in your pattern to be safe.
2. EXPLICIT TARGETING: You MUST ALWAYS include the specific filename as a trailing argument in your command (e.g., `f2 -f "old" -r "new" "exact_filename.txt"`). This is non-negotiable for precision.
3. OUTPUT ONLY: Output ONLY the command. No markdown, no explanations, no backticks unless part of the shell syntax.
4. NO EXECUTION FLAGS: Do not include `-x` or `-X` in the command. Dexter will add these later.

Rules:
1. Generate the command based on User Intent: {}
2. Use the Current Working Directory Context:
{}
3. The command must be valid for the current OS (macOS/Unix).

User Input: {}
"#,
            plugin.name(),
            plugin.description(),
            plugin.get_doc_for_executor(),
            context_str,
            user_input,
            context_str,
            user_input
        );

        let command = self.llm_client.completion(&system_prompt, user_input).await?;
        let clean_command = command.trim().replace("```bash", "").replace("```", "").trim().to_string();

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
