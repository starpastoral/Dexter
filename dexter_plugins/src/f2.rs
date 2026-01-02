use crate::{DiffItem, Plugin, PreviewContent};
use anyhow::Result;
use async_trait::async_trait;
use std::process::Command;

pub struct F2Plugin;

#[async_trait]
impl Plugin for F2Plugin {
    fn name(&self) -> &str {
        "f2"
    }

    fn description(&self) -> &str {
        "A fast, safe, and powerful batch renamer written in Go."
    }

    async fn is_installed(&self) -> bool {
        Command::new("f2")
            .arg("--version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn install(&self) -> Result<()> {
        // Assume user uses brew on macOS or has it installed
        // In a real app, we might provide specific instructions
        Err(anyhow::anyhow!(
            "Please install F2 manually: 'brew install f2'"
        ))
    }

    fn get_doc_for_router(&self) -> &str {
        "Best for batch renaming files and directories using search and replace or regex."
    }

    fn get_doc_for_executor(&self) -> &str {
        r#"f2 Command Usage:
- Simple find/replace: f2 -f "old" -r "new"
- Regex find/replace: f2 -f "(\d+)" -r "IMG_$1"
- Target specific file: f2 -f "old" -r "new" "file.txt"
- Undo last operation: f2 -u -x
- Variable Syntax:
    - Use {{var}} for file attributes (e.g., {{ext}}, {{isoDate}}).
    - Use $1 for Regex capture groups.
    - Correction: Use double curly braces {{id}}, NOT {id}.
    - Counter: Use {{%03d}} for zero-padded numbers, NOT %03d.

Complex Examples:
- Rename with Regex capture + 3-digit counter + Execute:
  f2 -f 'Photo_(\d+)' -r 'Trip_$1_{{%03d}}' -x
- Undo the last operation:
  f2 -u -x

Notes:
1. Always include -x if you want to apply the changes, otherwise f2 only shows a preview.
2. For maximum precision, include the specific filename as a trailing argument.
3. f2 supports full regular expressions in the -f pattern by default.
"#
    }

    fn get_executor_prompt(&self, context: &str, user_input: &str) -> String {
        format!(
            r#"You are the Renaming Specialist Agent for Dexter. 
Your goal is to generate a precise `f2` command (a powerful batch renamer).

### HARD CONSTRAINTS (MUST FOLLOW):
1. CHARACTER PRECISION: The file list in the context is the ABSOLUTE TRUTH. 
   - Wave Dash (`〜`, U+301C) and Full-width Tilde (`～`, U+FF5E) are DIFFERENT.
   - You MUST match the EXACT character code from the context.
2. EXPLICIT TARGETING: You MUST ALWAYS include the specific filename as a trailing argument in your command (e.g., `f2 -f "old" -r "new" "exact_filename.txt"`).
3. OUTPUT ONLY: Output ONLY the command. No backticks, no markdown, no explanations.
4. NO EXECUTION FLAGS: Do not include `-x` or `-X`.

### Documentation:
{}

### Context:
{}

### User Request:
{}
"#,
            self.get_doc_for_executor(),
            context,
            user_input
        )
    }

    fn validate_command(&self, cmd: &str) -> bool {
        cmd.starts_with("f2 ")
    }

    async fn dry_run(
        &self,
        cmd: &str,
        _llm: Option<&dyn crate::LlmBridge>,
    ) -> Result<PreviewContent> {
        // Ensure no-color and strip -x/-X
        let mut safe_cmd = cmd.replace(" -x", "").replace(" -X", "");

        if !safe_cmd.contains(" --no-color") {
            safe_cmd.push_str(" --no-color");
        }

        let final_cmd = safe_cmd;

        let mut cmd_obj = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", &final_cmd]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", &final_cmd]);
            c
        };

        let cwd = std::env::current_dir()?;
        let output = cmd_obj.current_dir(cwd).output()?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{}{}", stdout, stderr);

        if !output.status.success() {
            return Err(anyhow::anyhow!("f2 error: {}", combined));
        }

        // Parse Output
        let mut diffs = Vec::new();
        for line in combined.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Ignorable lines
            if (trimmed.starts_with('*') || trimmed.starts_with('-') || trimmed.starts_with('+'))
                && (trimmed.contains("---") || trimmed.contains("***") || trimmed.len() > 10)
            {
                continue;
            }
            if trimmed.contains("headers") || trimmed.contains("ORIGINAL") {
                continue;
            }

            // Strategy 1: " -> "
            if trimmed.contains(" -> ") {
                let parts: Vec<&str> = trimmed.split(" -> ").collect();
                if parts.len() == 2 {
                    diffs.push(DiffItem {
                        original: parts[0].trim().to_string(),
                        new: parts[1].trim().to_string(),
                    });
                    continue;
                }
            }

            // Strategy 2: "|" table style
            if trimmed.contains('|') {
                let parts: Vec<&str> = trimmed
                    .split('|')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect();

                if parts.len() >= 2 {
                    let old_name = parts[0];
                    let new_name = parts[1];

                    // Filter headers
                    let old_lower = old_name.to_lowercase();
                    if old_lower.contains("original") || old_lower.contains("filename") {
                        continue;
                    }

                    diffs.push(DiffItem {
                        original: old_name.to_string(),
                        new: new_name.to_string(),
                    });
                }
            }
        }

        if !diffs.is_empty() {
            Ok(PreviewContent::DiffList(diffs))
        } else {
            // If we couldn't parse logic diffs, just return text
            Ok(PreviewContent::Text(combined))
        }
    }

    async fn execute(&self, cmd: &str) -> Result<String> {
        let mut final_cmd = cmd.to_string();
        if !final_cmd.contains(" --no-color") {
            final_cmd.push_str(" --no-color");
        }

        let mut cmd_obj = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", &final_cmd]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", &final_cmd]);
            c
        };

        let cwd = std::env::current_dir()?;
        let output = cmd_obj.current_dir(cwd).output()?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            let combined = format!("{}{}", stdout, stderr);
            Ok(combined)
        } else {
            Err(anyhow::anyhow!("f2 error: {}\n{}", stdout, stderr))
        }
    }
}
