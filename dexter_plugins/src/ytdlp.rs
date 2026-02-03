use crate::{Plugin, PreviewContent};
use anyhow::Result;
use async_trait::async_trait;
use regex::Regex;
use std::process::Command;
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;

pub struct YtDlpPlugin;

fn extract_percentage(re: &Regex, line: &str) -> Option<f64> {
    let caps = re.captures(line)?;
    let value = caps.get(1)?.as_str().parse::<f64>().ok()?;
    Some(value.clamp(0.0, 100.0))
}

#[async_trait]
impl Plugin for YtDlpPlugin {
    fn name(&self) -> &str {
        "yt-dlp"
    }

    fn description(&self) -> &str {
        "A feature-rich video/audio downloader with format selection and audio extraction."
    }

    async fn is_installed(&self) -> bool {
        Command::new("yt-dlp")
            .arg("--version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn install(&self) -> Result<()> {
        Err(anyhow::anyhow!(
            "Please install yt-dlp manually:\n- macOS (brew): brew install yt-dlp\n- pipx: pipx install yt-dlp\n- pip: pip install -U yt-dlp\n- Debian/Ubuntu: sudo apt install yt-dlp"
        ))
    }

    fn get_doc_for_router(&self) -> &str {
        "Best for downloading videos or audio from supported sites, extracting audio, and choosing formats."
    }

    fn get_doc_for_executor(&self) -> &str {
        r#"yt-dlp Command Usage:
- Download best available: yt-dlp "https://example.com/video"
- Save with template: yt-dlp -o "%(title)s.%(ext)s" "https://example.com/video"
- Choose format: yt-dlp -f "bv*+ba/b" "https://example.com/video"
- Extract audio to mp3: yt-dlp -x --audio-format mp3 "https://example.com/video"
- Download playlist: yt-dlp -o "%(playlist_index)s - %(title)s.%(ext)s" "https://example.com/playlist"
- Use cookies: yt-dlp --cookies cookies.txt "https://example.com/video"
- Force single video from a playlist: yt-dlp --no-playlist "https://example.com/video"

Notes:
1. Prefer -o for output naming instead of shell redirection.
2. Use --newline for line-by-line progress (Dexter may add it automatically).
3. Do NOT use --exec (blocked for safety).
"#
    }

    fn get_executor_prompt(&self, context: &str, user_input: &str) -> String {
        format!(
            r#"You are the Download Specialist Agent for Dexter.
Your goal is to generate a valid `yt-dlp` command.

### HARD CONSTRAINTS (MUST FOLLOW):
1. OUTPUT ONLY: Output ONLY the command. No backticks, no markdown, no explanations.
2. NO SHELL CHAINS: Do NOT use pipes, `&&`, `||`, `;`, backticks, or `$()`.
3. NO --exec: This flag is blocked and must never appear.
4. DEFAULTS: Do NOT add `--no-playlist` unless the user explicitly requests a single video.
5. PRECISION: Treat URLs and filenames as literal strings; use exact characters from context.
6. NO --newline: Dexter will add `--newline` during execution if needed.

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
        let trimmed = cmd.trim();
        if !trimmed.starts_with("yt-dlp ") {
            return false;
        }

        if trimmed.contains("--exec") {
            return false;
        }

        let banned_tokens = ["&&", "||", ";", "|", "`", "$("];
        if banned_tokens.iter().any(|t| trimmed.contains(t)) {
            return false;
        }

        true
    }

    async fn dry_run(
        &self,
        cmd: &str,
        llm: Option<&dyn crate::LlmBridge>,
    ) -> Result<PreviewContent> {
        if let Some(llm) = llm {
            let system_prompt = "You are a clear and concise command explainer for Dexter. Describe what this yt-dlp command will do in simple terms. Mention source URL(s), output naming, and key options. Output plain text only.";
            let text = llm.chat(system_prompt, cmd).await?;
            Ok(PreviewContent::Text(text))
        } else {
            Ok(PreviewContent::Text(format!(
                "Executing download command: {}",
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
        progress_tx: tokio::sync::mpsc::Sender<crate::Progress>,
    ) -> Result<String> {
        let mut final_cmd = cmd.to_string();
        if !final_cmd.contains(" --newline") {
            final_cmd.push_str(" --newline");
        }

        let mut child = if cfg!(target_os = "windows") {
            tokio::process::Command::new("cmd")
                .args(["/C", &final_cmd])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?
        } else {
            tokio::process::Command::new("sh")
                .args(["-c", &final_cmd])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?
        };

        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stderr"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stdout"))?;

        let progress_re =
            Arc::new(Regex::new(r"(?i)\b(\d{1,3}(?:\.\d+)?)%\b").unwrap());

        let tx_err = progress_tx.clone();
        let re_err = progress_re.clone();
        let mut stderr_reader = tokio::io::BufReader::new(stderr).lines();
        let stderr_handle = tokio::spawn(async move {
            let mut captured_err = String::new();
            while let Ok(Some(line)) = stderr_reader.next_line().await {
                if let Some(pct) = extract_percentage(&re_err, &line) {
                    let _ = tx_err
                        .send(crate::Progress {
                            percentage: Some(pct),
                            message: format!("Downloading: {:.1}%", pct),
                        })
                        .await;
                }
                captured_err.push_str(&line);
                captured_err.push('\n');
            }
            captured_err
        });

        let tx_out = progress_tx.clone();
        let re_out = progress_re.clone();
        let mut stdout_reader = tokio::io::BufReader::new(stdout).lines();
        let stdout_handle = tokio::spawn(async move {
            let mut captured_out = String::new();
            while let Ok(Some(line)) = stdout_reader.next_line().await {
                if let Some(pct) = extract_percentage(&re_out, &line) {
                    let _ = tx_out
                        .send(crate::Progress {
                            percentage: Some(pct),
                            message: format!("Downloading: {:.1}%", pct),
                        })
                        .await;
                }
                captured_out.push_str(&line);
                captured_out.push('\n');
            }
            captured_out
        });

        let status = child.wait().await?;
        let err_output = stderr_handle.await?;
        let out_output = stdout_handle.await?;

        if status.success() {
            let combined = format!("{}\n{}", out_output, err_output);
            Ok(if combined.trim().is_empty() {
                "Command executed successfully (no output)".to_string()
            } else {
                combined
            })
        } else {
            Err(anyhow::anyhow!(format!("yt-dlp error:\n{}", err_output)))
        }
    }
}
