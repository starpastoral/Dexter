use crate::command_exec::{contains_arg, parse_and_validate_command, spawn_checked};
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
    - Use $1, $2, etc., for Regex capture groups.
    - Correction: Use double curly braces {{id}}, NOT {id}.
    - Counter: Use {{%03d}} for zero-padded numbers, NOT %03d.

Complex Examples:
- Rename with Regex capture + 3-digit counter + Execute:
  f2 -f 'Photo_(\d+)' -r 'Trip_$1_{{%03d}}' -x
- Remove enclosing brackets but keep content (e.g., "【1】" -> "1"):
  f2 -f '【(\d+)】' -r '$1'
- Multiple replacements with spacing (e.g., "【1】【2】" -> "1 2"):
  f2 -f '【(\d+)】【(\d+)】' -r '$1 $2'
- Undo the last operation:
  f2 -u -x

Notes:
1. CAPTURE GROUPS: To preserve part of the matched text (like a number inside brackets), you MUST wrap that part in parentheses `()` in the `-f` pattern and refer to it as `$1` in the `-r` pattern.
2. Always include -x if you want to apply the changes, otherwise f2 only shows a preview.
3. For maximum precision, include the specific filename as a trailing argument.
4. f2 supports full regular expressions in the -f pattern by default.
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
2. SMART TARGETING: Although the context provides folder content, do NOT automatically include specific filenames as trailing arguments.
   - ONLY include specific filenames if the user explicitly intends to target those specific files.
   - For general batch renaming, rely on the `-f` pattern to match files.
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
        parse_and_validate_command(cmd, "f2").is_ok()
    }

    async fn dry_run(
        &self,
        cmd: &str,
        _llm: Option<&dyn crate::LlmBridge>,
    ) -> Result<PreviewContent> {
        let argv = build_f2_argv(cmd, false)?;
        let cwd = std::env::current_dir()?;
        let output = spawn_checked(&argv, cwd)?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{}{}", stdout, stderr);

        // Parse Output even if it failed, as f2 returns non-zero on conflicts
        let mut diffs = Vec::new();
        for line in combined.lines() {
            let mut trimmed = line.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }

            // More aggressive cleaning for f2's fancy table corners/borders in error cases
            trimmed = trimmed
                .replace("|*", "|")
                .replace("*|", "|")
                .replace("—", "")
                .replace("*", "")
                .trim()
                .to_string();

            if trimmed.is_empty() {
                continue;
            }

            // Strategy 1: " -> "
            if trimmed.contains(" -> ") {
                let parts: Vec<&str> = trimmed.split(" -> ").collect();
                if parts.len() == 2 {
                    diffs.push(DiffItem {
                        original: parts[0].trim().to_string(),
                        new: parts[1].trim().to_string(),
                        status: None, // No status in this format
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
                    let status = parts.get(2).map(|s| s.to_string());

                    // Filter headers or empty-ish rows
                    let old_lower = old_name.to_lowercase();
                    if old_lower.contains("original")
                        || old_lower.contains("filename")
                        || old_name.chars().all(|c| c == ' ')
                    {
                        continue;
                    }

                    diffs.push(DiffItem {
                        original: old_name.to_string(),
                        new: new_name.to_string(),
                        status,
                    });
                }
            }
        }

        if !diffs.is_empty() {
            Ok(PreviewContent::DiffList(diffs))
        } else if !output.status.success() {
            // If we couldn't parse any diffs AND it failed, then return the error
            Err(anyhow::anyhow!("f2 error: {}", combined))
        } else {
            // If it succeeded but we couldn't parse logic diffs, just return text
            Ok(PreviewContent::Text(combined))
        }
    }

    async fn execute(&self, cmd: &str) -> Result<String> {
        let argv = build_f2_argv(cmd, true)?;
        let cwd = std::env::current_dir()?;
        let output = spawn_checked(&argv, cwd)?;

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

fn build_f2_argv(cmd: &str, execute_mode: bool) -> Result<Vec<String>> {
    let mut argv = parse_and_validate_command(cmd, "f2")?;
    argv.retain(|a| a != "-x" && a != "-X");

    if execute_mode && !contains_arg(&argv, "-x") && !contains_arg(&argv, "-X") {
        argv.push("-x".to_string());
    }
    if !contains_arg(&argv, "--no-color") {
        argv.push("--no-color".to_string());
    }

    Ok(argv)
}
