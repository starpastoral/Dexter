use crate::command_exec::parse_and_validate_command;
use crate::{LlmBridge, Plugin, PreviewContent, Progress};
use anyhow::Result;
use async_trait::async_trait;
use std::process::Command;

pub struct JdupesPlugin;

fn contains_flag(argv: &[String], flag: &str) -> bool {
    argv.iter().any(|a| a == flag)
}

fn contains_flag_with_value(argv: &[String], flag: &str) -> bool {
    let prefix = format!("{}=", flag);
    argv.iter().any(|a| a.starts_with(&prefix))
}

fn option_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-C" | "--chunk-size"
            | "-o"
            | "--order"
            | "-P"
            | "--print"
            | "-X"
            | "--ext-filter"
            | "-y"
            | "--hash-db"
    )
}

fn option_is_inline_with_value(arg: &str) -> bool {
    arg.starts_with("--chunk-size=")
        || arg.starts_with("--order=")
        || arg.starts_with("--print=")
        || arg.starts_with("--ext-filter=")
        || arg.starts_with("--hash-db=")
}

fn has_target_path(argv: &[String]) -> bool {
    let mut i = 1;
    while i < argv.len() {
        let arg = &argv[i];

        if arg == "--" {
            return i + 1 < argv.len();
        }

        if option_is_inline_with_value(arg) {
            i += 1;
            continue;
        }

        if option_takes_value(arg) {
            i += 2;
            continue;
        }

        if arg.starts_with('-') {
            i += 1;
            continue;
        }

        return true;
    }

    false
}

fn validate_jdupes_command(cmd: &str) -> bool {
    let argv = match parse_and_validate_command(cmd, "jdupes") {
        Ok(v) => v,
        Err(_) => return false,
    };

    if argv.iter().any(|a| a.starts_with('@')) {
        return false;
    }

    if !has_target_path(&argv) {
        return false;
    }

    let wants_delete = contains_flag(&argv, "-d") || contains_flag(&argv, "--delete");
    let has_no_prompt = contains_flag(&argv, "-N") || contains_flag(&argv, "--no-prompt");
    if wants_delete && !has_no_prompt {
        return false;
    }
    if has_no_prompt && !wants_delete {
        return false;
    }

    let blocked_flags = [
        "-l",
        "--link-soft",
        "-L",
        "--link-hard",
        "-B",
        "--dedupe",
        "-Q",
        "--quick",
        "-T",
        "--partial-only",
        "-t",
        "--no-change-check",
        "-U",
        "--no-trav-check",
    ];

    if blocked_flags
        .iter()
        .any(|flag| contains_flag(&argv, flag) || contains_flag_with_value(&argv, flag))
    {
        return false;
    }

    true
}

#[async_trait]
impl Plugin for JdupesPlugin {
    fn name(&self) -> &str {
        "jdupes"
    }

    fn description(&self) -> &str {
        "Find duplicate files quickly with safe scan/summary workflows and optional controlled delete mode."
    }

    async fn is_installed(&self) -> bool {
        Command::new("jdupes")
            .arg("--version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn install(&self) -> Result<()> {
        Err(anyhow::anyhow!(
            "Please install jdupes manually:\n- macOS (brew): brew install jdupes\n- Debian/Ubuntu: sudo apt install jdupes"
        ))
    }

    fn get_doc_for_router(&self) -> &str {
        "Best for scanning directories to find duplicate files, summarize duplicate size, report unique files, and optionally delete duplicates in controlled mode."
    }

    fn get_doc_for_executor(&self) -> &str {
        r#"jdupes Command Usage:
- Scan current directory recursively: jdupes -r .
- Summarize duplicates only: jdupes -r -m ~/Downloads
- Print duplicate sizes: jdupes -r -S ~/Pictures
- Print unique files only: jdupes -r -u ~/Documents
- JSON output: jdupes -r -j ~/Media
- Controlled delete mode: jdupes -r -d -N ~/Downloads

Safety Constraints:
1. If using delete mode, `-d/--delete` MUST be paired with `-N/--no-prompt`.
2. Do NOT use linking/dedupe actions: -l/--link-soft, -L/--link-hard, -B/--dedupe.
3. Do NOT use unsafe speed shortcuts: -Q/--quick, -T/--partial-only, -t/--no-change-check, -U/--no-trav-check.
4. Always include at least one explicit target path or directory."#
    }

    fn get_executor_prompt(&self, context: &str, user_input: &str) -> String {
        format!(
            r#"You are the Duplicate File Analysis Specialist Agent for Dexter.
Your goal is to generate a valid `jdupes` command.

### HARD CONSTRAINTS (MUST FOLLOW):
1. OUTPUT ONLY: Output ONLY the command. No backticks, no markdown, no explanations.
2. NO SHELL CHAINS: Do NOT use pipes, `&&`, `||`, `;`, backticks, or `$()`.
3. DELETE RULE: If using deletion, you MUST include BOTH `-d/--delete` and `-N/--no-prompt`.
4. BLOCKED FLAGS: Do NOT use `-l`, `--link-soft`, `-L`, `--link-hard`, `-B`, `--dedupe`.
5. NO UNSAFE SHORTCUTS: Do NOT use `-Q`, `--quick`, `-T`, `--partial-only`, `-t`, `--no-change-check`, or `-U`, `--no-trav-check`.
6. TARGET REQUIRED: Include at least one target path/directory.
7. PRECISION: Treat file paths as literal strings from context.

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
        validate_jdupes_command(cmd)
    }

    async fn dry_run(&self, cmd: &str, llm: Option<&dyn LlmBridge>) -> Result<PreviewContent> {
        if let Some(llm) = llm {
            let system_prompt = "You are a clear and concise command explainer for Dexter. Describe what this jdupes command will scan, whether it is recursive, and whether it only reports duplicates or deletes duplicates (controlled -d -N mode). Output plain text only.";
            let text = llm.chat(system_prompt, cmd).await?;
            Ok(PreviewContent::Text(text))
        } else {
            Ok(PreviewContent::Text(format!(
                "Executing duplicate scan command: {}",
                cmd
            )))
        }
    }

    async fn execute(&self, cmd: &str) -> Result<String> {
        self.execute_with_progress(cmd, tokio::sync::mpsc::channel(1).0)
            .await
    }

    async fn execute_with_progress(
        &self,
        cmd: &str,
        progress_tx: tokio::sync::mpsc::Sender<Progress>,
    ) -> Result<String> {
        let argv = parse_and_validate_command(cmd, "jdupes")?;
        if !validate_jdupes_command(cmd) {
            return Err(anyhow::anyhow!("Command failed jdupes validation logic"));
        }

        let wants_delete = contains_flag(&argv, "-d") || contains_flag(&argv, "--delete");
        let phase = if wants_delete {
            "Deleting duplicate files (preserve-first mode)..."
        } else {
            "Scanning for duplicate files..."
        };

        let _ = progress_tx
            .send(Progress {
                percentage: None,
                message: phase.to_string(),
            })
            .await;

        let output = tokio::process::Command::new(&argv[0])
            .args(argv.iter().skip(1))
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(format!("{}{}", stdout, stderr))
        } else {
            Err(anyhow::anyhow!("jdupes error: {}\n{}", stdout, stderr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_allows_common_jdupes_workflows() {
        assert!(validate_jdupes_command("jdupes -r ."));
        assert!(validate_jdupes_command("jdupes -r -m ~/Downloads"));
        assert!(validate_jdupes_command("jdupes -r -S \"/tmp/media files\""));
    }

    #[test]
    fn validate_rejects_shell_injection() {
        assert!(!validate_jdupes_command("jdupes -r .; rm -rf /"));
        assert!(!validate_jdupes_command("jdupes -r . && echo hacked"));
    }

    #[test]
    fn validate_allows_controlled_delete_mode() {
        assert!(validate_jdupes_command("jdupes -r -d -N ."));
        assert!(validate_jdupes_command(
            "jdupes --recurse --delete --no-prompt ~/Downloads"
        ));
    }

    #[test]
    fn validate_rejects_unsafe_or_missing_target() {
        assert!(!validate_jdupes_command("jdupes -r -d ."));
        assert!(!validate_jdupes_command("jdupes -r -N ."));
        assert!(!validate_jdupes_command("jdupes -r -L ."));
        assert!(!validate_jdupes_command("jdupes -r -Q ."));
        assert!(!validate_jdupes_command("jdupes -r"));
    }
}
