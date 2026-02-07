use crate::command_exec::parse_and_validate_command;
use crate::{LlmBridge, Plugin, PreviewContent, Progress};
use anyhow::Result;
use async_trait::async_trait;
use std::process::Command;

pub struct LibvipsPlugin;

fn parse_libvips_command(cmd: &str) -> Result<Vec<String>> {
    parse_and_validate_command(cmd, "vips")
        .or_else(|_| parse_and_validate_command(cmd, "vipsthumbnail"))
}

fn validate_vips_command(argv: &[String]) -> bool {
    if argv.len() < 4 {
        return false;
    }

    let operation = argv.get(1).map(|s| s.as_str()).unwrap_or_default();
    let allowed_ops = [
        "thumbnail",
        "resize",
        "crop",
        "rot",
        "flip",
        "flop",
        "autorot",
        "copy",
        "embed",
        "extract_area",
    ];

    if !allowed_ops.contains(&operation) {
        return false;
    }

    if argv.iter().any(|a| a.contains("descriptor=")) {
        return false;
    }

    true
}

fn has_output_option(argv: &[String]) -> bool {
    for (idx, arg) in argv.iter().enumerate() {
        if arg == "-o" || arg == "--output" {
            if let Some(value) = argv.get(idx + 1) {
                if !value.trim().is_empty() {
                    return true;
                }
            }
        }

        if let Some(value) = arg.strip_prefix("--output=") {
            if !value.trim().is_empty() {
                return true;
            }
        }
    }

    false
}

fn validate_vipsthumbnail_command(argv: &[String]) -> bool {
    if argv.len() < 2 {
        return false;
    }

    if argv.iter().any(|a| a == "-" || a.contains("descriptor=")) {
        return false;
    }

    if !has_output_option(argv) {
        return false;
    }

    true
}

fn validate_libvips_command(cmd: &str) -> bool {
    let argv = match parse_libvips_command(cmd) {
        Ok(v) => v,
        Err(_) => return false,
    };

    if argv.iter().any(|a| a.starts_with('@')) {
        return false;
    }

    let blocked_tokens = ["[descriptor=0]", "[descriptor=1]"];
    if argv
        .iter()
        .any(|arg| blocked_tokens.iter().any(|blocked| arg.contains(blocked)))
    {
        return false;
    }

    let program = argv.first().map(|s| s.as_str()).unwrap_or_default();
    if program == "vips" {
        return validate_vips_command(&argv);
    }

    if program == "vipsthumbnail" {
        return validate_vipsthumbnail_command(&argv);
    }

    false
}

fn progress_message(argv: &[String]) -> String {
    match argv.first().map(|s| s.as_str()) {
        Some("vips") => {
            let op = argv
                .get(1)
                .cloned()
                .unwrap_or_else(|| "operation".to_string());
            format!("Running libvips {} operation...", op)
        }
        Some("vipsthumbnail") => "Generating thumbnails with libvips...".to_string(),
        _ => "Running libvips command...".to_string(),
    }
}

#[async_trait]
impl Plugin for LibvipsPlugin {
    fn name(&self) -> &str {
        "libvips"
    }

    fn description(&self) -> &str {
        "High-performance image processing with vips/vipsthumbnail for resize, crop, rotate, and conversion workflows."
    }

    async fn is_installed(&self) -> bool {
        let has_vips = Command::new("vips")
            .arg("--version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        let has_thumbnail = Command::new("vipsthumbnail")
            .arg("--version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        has_vips || has_thumbnail
    }

    async fn install(&self) -> Result<()> {
        Err(anyhow::anyhow!(
            "Please install libvips tools manually:\n- macOS (brew): brew install vips\n- Debian/Ubuntu: sudo apt install libvips-tools"
        ))
    }

    fn get_doc_for_router(&self) -> &str {
        "Best for fast image resize/crop/rotate/thumbnail/conversion workflows using vips or vipsthumbnail."
    }

    fn get_doc_for_executor(&self) -> &str {
        r#"libvips Command Usage:
- Resize image: vips resize input.jpg output.jpg 0.5
- Smart thumbnail: vips thumbnail input.jpg output.jpg 512
- Crop region: vips crop input.jpg output.jpg 100 80 640 480
- Auto rotate by EXIF: vips autorot input.jpg output.jpg
- Batch thumbnail with output pattern: vipsthumbnail input.jpg -s 256 -o tn_%s.jpg

Safety Constraints:
1. Use `vips` with explicit operation and file paths, or `vipsthumbnail` with explicit `-o/--output`.
2. Do NOT use descriptor-based stdin/stdout forms like `[descriptor=0]`.
3. Limit `vips` operations to common image transforms: thumbnail/resize/crop/rot/flip/flop/autorot/copy/embed/extract_area.
4. Do NOT use shell redirection; keep all IO in command arguments."#
    }

    fn get_executor_prompt(&self, context: &str, user_input: &str) -> String {
        format!(
            r#"You are the Image Processing Specialist Agent for Dexter.
Your goal is to generate a valid `vips` or `vipsthumbnail` command for libvips.

### HARD CONSTRAINTS (MUST FOLLOW):
1. OUTPUT ONLY: Output ONLY the command. No backticks, no markdown, no explanations.
2. NO SHELL CHAINS: Do NOT use pipes, `&&`, `||`, `;`, backticks, or `$()`.
3. SAFE IO: Do NOT use descriptor-based stdin/stdout patterns (e.g. `[descriptor=0]`).
4. vips SCOPE: If using `vips`, only use one of: `thumbnail`, `resize`, `crop`, `rot`, `flip`, `flop`, `autorot`, `copy`, `embed`, `extract_area`.
5. vipsthumbnail OUTPUT: If using `vipsthumbnail`, include `-o/--output`.
6. PRECISION: Treat file paths and filenames as literal strings from context.

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
        validate_libvips_command(cmd)
    }

    async fn dry_run(&self, cmd: &str, llm: Option<&dyn LlmBridge>) -> Result<PreviewContent> {
        if let Some(llm) = llm {
            let system_prompt = "You are a clear and concise command explainer for Dexter. Describe what this libvips command will do, including operation type (resize/crop/rotate/thumbnail), input files, output files, and sizing parameters. Output plain text only.";
            let text = llm.chat(system_prompt, cmd).await?;
            Ok(PreviewContent::Text(text))
        } else {
            Ok(PreviewContent::Text(format!(
                "Executing image processing command: {}",
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
        let argv = parse_libvips_command(cmd)?;
        if !validate_libvips_command(cmd) {
            return Err(anyhow::anyhow!("Command failed libvips validation logic"));
        }

        let _ = progress_tx
            .send(Progress {
                percentage: None,
                message: progress_message(&argv),
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
            Err(anyhow::anyhow!("libvips error: {}\n{}", stdout, stderr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_allows_common_libvips_workflows() {
        assert!(validate_libvips_command(
            "vips resize input.jpg output.jpg 0.5"
        ));
        assert!(validate_libvips_command(
            "vips crop input.jpg output.jpg 10 20 400 300"
        ));
        assert!(validate_libvips_command(
            "vipsthumbnail input.jpg -s 256 -o tn_%s.jpg"
        ));
    }

    #[test]
    fn validate_rejects_shell_injection() {
        assert!(!validate_libvips_command(
            "vips resize input.jpg output.jpg 0.5; rm -rf /"
        ));
        assert!(!validate_libvips_command(
            "vipsthumbnail input.jpg -o tn_%s.jpg && echo hacked"
        ));
    }

    #[test]
    fn validate_rejects_unsafe_or_out_of_scope_patterns() {
        assert!(!validate_libvips_command(
            "vips thumbnail_source [descriptor=0] out.jpg 128"
        ));
        assert!(!validate_libvips_command("vips black out.jpg 100 100"));
        assert!(!validate_libvips_command("vipsthumbnail input.jpg -s 128"));
    }
}
