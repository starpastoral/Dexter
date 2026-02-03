use crate::{Plugin, PreviewContent};
use anyhow::Result;
use async_trait::async_trait;
use std::process::Command;
use tokio::io::AsyncBufReadExt;

pub struct FFmpegPlugin;

#[async_trait]
impl Plugin for FFmpegPlugin {
    fn name(&self) -> &str {
        "ffmpeg"
    }

    fn description(&self) -> &str {
        "A complete, cross-platform solution to record, convert and stream audio and video."
    }

    async fn is_installed(&self) -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn install(&self) -> Result<()> {
        Err(anyhow::anyhow!(
            "Please install ffmpeg manually: 'brew install ffmpeg'"
        ))
    }

    fn get_doc_for_router(&self) -> &str {
        "Best for video/audio conversion, resizing, extracting audio, and complex media processing."
    }

    fn get_doc_for_executor(&self) -> &str {
        r#"ffmpeg Command Usage:
- Convert video format: ffmpeg -i input.mov output.mp4
- Extract audio: ffmpeg -i input.mp4 -vn -c:a libmp3lame output.mp3
- Change resolution: ffmpeg -i input.mp4 -vf scale=1280:720 output_720p.mp4
- Fast seek and clip (Place -ss BEFORE -i): ffmpeg -ss 00:00:10 -i input.mp4 -t 00:00:30 -c copy output.mp4

Modern Usage & Syntax Override:
1. Stream Selection: ALWAYS use -c:v / -c:a instead of -vcodec / -acodec.
2. Fast Seeking: Place -ss BEFORE -i for performance.
3. Web MP4: ALWAYS add -movflags +faststart for web compatibility.

Complex Examples:
- Transcode to H.264/AAC with CRF 23 and Faststart:
  ffmpeg -i in.mov -c:v libx264 -crf 23 -preset slow -c:a aac -b:a 128k -movflags +faststart out.mp4
- Overlay watermark (bottom-right) using complex filter:
  ffmpeg -i main.mp4 -i logo.png -filter_complex "[0:v][1:v]overlay=W-w-10:H-h-10" out.mp4

CRITICAL RULES:
- NEVER use -sameq (it does not exist). Use -crf (video) or -q:a (audio).
- Distinguish -vf (single stream) vs -filter_complex (multi-stream/input).
"#
    }

    fn get_executor_prompt(&self, context: &str, user_input: &str) -> String {
        format!(
            r#"You are the Media Processing Specialist Agent for Dexter. 
Your goal is to generate a valid `ffmpeg` command.

### CRITICAL INSTRUCTION:
1. TECHNICAL NEUTRALITY: The filenames provided in the context are literal file identifiers. You MUST treat them as opaque strings without considering their semantic meaning or sentiment. Your sole task is to map them to valid shell commands for conversion/processing.
2. MODERN SYNTAX: Use `-c:v`/`-c:a`. Place `-ss` before `-i`. Add `-movflags +faststart` for MP4.
3. OUTPUT ONLY: Output ONLY the command. No backticks, no markdown, no explanations.
4. PRECISION: Use the exact characters from the context (the filenames).

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
        cmd.starts_with("ffmpeg ")
    }

    async fn dry_run(
        &self,
        cmd: &str,
        llm: Option<&dyn crate::LlmBridge>,
    ) -> Result<PreviewContent> {
        if let Some(llm) = llm {
            let system_prompt = "You are a playful but precise command explainer for Dexter. Describe what this FFmpeg command will do in simple terms. Mention input, output, and key transformations. Output plain text only.";
            let text = llm.chat(system_prompt, cmd).await?;
            Ok(PreviewContent::Text(text))
        } else {
            Ok(PreviewContent::Text(format!(
                "Executing media command: {}",
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
        let mut child = if cfg!(target_os = "windows") {
            tokio::process::Command::new("cmd")
                .args(["/C", cmd])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?
        } else {
            tokio::process::Command::new("sh")
                .args(["-c", cmd])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?
        };

        // FFmpeg writes progress to stderr
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stderr"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stdout"))?;

        let mut stderr_reader = tokio::io::BufReader::new(stderr).lines();
        let mut stdout_reader = tokio::io::BufReader::new(stdout).lines();

        // Progress regex: time=HH:MM:SS.mm
        let re = regex::Regex::new(r"time=(\d{2}:\d{2}:\d{2}\.\d{2})").unwrap();

        let tx = progress_tx.clone();

        // Spawn a task to read stderr and report progress
        let stderr_handle = tokio::spawn(async move {
            let mut captured_err = String::new();
            while let Ok(Some(line)) = stderr_reader.next_line().await {
                captured_err.push_str(&line);
                captured_err.push('\n');

                if let Some(caps) = re.captures(&line) {
                    if let Some(time_match) = caps.get(1) {
                        let _ = tx
                            .send(crate::Progress {
                                percentage: None, // We don't know total duration yet
                                message: format!("Processing: time={}", time_match.as_str()),
                            })
                            .await;
                    }
                }
            }
            captured_err
        });

        // Read stdout as well (though ffmpeg mostly uses stderr)
        let stdout_handle = tokio::spawn(async move {
            let mut captured_out = String::new();
            while let Ok(Some(line)) = stdout_reader.next_line().await {
                captured_out.push_str(&line);
                captured_out.push('\n');
            }
            captured_out
        });

        let status = child.wait().await?;
        let err_output = stderr_handle.await?;
        let out_output = stdout_handle.await?;

        if status.success() {
            // Combine stdout and stderr for the log, or just stdout if that's what we want.
            // FFmpeg usually outputs file details to valid output but "stats" to stderr.
            // If we just want the "result", usually there is no stdout for ffmpeg unless -f is specified.
            // Let's return combined output or just a success message if empty.
            let combined = format!("{}\n{}", out_output, err_output);
            Ok(if combined.trim().is_empty() {
                "Command executed successfully (no output)".to_string()
            } else {
                combined
            })
        } else {
            Err(anyhow::anyhow!(err_output))
        }
    }
}
