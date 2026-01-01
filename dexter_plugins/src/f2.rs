use anyhow::Result;
use async_trait::async_trait;
use crate::Plugin;
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
        Command::new("f2").arg("--version").status().map(|s| s.success()).unwrap_or(false)
    }

    async fn install(&self) -> Result<()> {
        // Assume user uses brew on macOS or has it installed
        // In a real app, we might provide specific instructions
        Err(anyhow::anyhow!("Please install f2 manually: 'brew install f2'"))
    }

    fn get_doc_for_router(&self) -> &str {
        "Best for batch renaming files and directories using search and replace or regex."
    }

    fn get_doc_for_executor(&self) -> &str {
        r#"f2 Command Usage:
- Simple find and replace: f2 -f "find" -r "replace"
- Regex find and replace: f2 -f "regexp" -r "replacement"
- Target specific file: f2 -f "find" -r "replace" "filename.txt"
- Undo last operation: f2 -u
- Preview changes: f2 -f "..." -r "..." (Default shows preview)
- Execute changes: f2 -f "..." -r "..." -x

Notes:
1. Always include -x if you want to apply the changes, otherwise f2 only shows a preview.
2. For maximum precision, include the specific filename as a trailing argument.
3. f2 supports full regular expressions in the -f pattern by default.
"# 
    }

    fn validate_command(&self, cmd: &str) -> bool {
        cmd.starts_with("f2 ")
    }

    async fn dry_run(&self, cmd: &str, _llm: Option<&dyn crate::LlmBridge>) -> Result<String> {
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

        if output.status.success() {
            let combined = format!("{}{}", stdout, stderr);
            Ok(combined)
        } else {
            Err(anyhow::anyhow!("f2 error: {}\n{}", stdout, stderr))
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
