use crate::{Plugin, PreviewContent};
use anyhow::Result;
use async_trait::async_trait;
use std::process::Command;

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
- Convert video format: ffmpeg -i input.mp4 output.mkv
- Extract audio: ffmpeg -i input.mp4 -vn -acodec libmp3lame output.mp3
- Change resolution: ffmpeg -i input.mp4 -vf scale=1280:720 output_720p.mp4
- Fast seek and clip: ffmpeg -ss 00:00:10 -i input.mp4 -t 00:00:30 -c copy output.mp4
- Compress video: ffmpeg -i input.mp4 -vcodec libx265 -crf 28 output.mp4

Important: Always specify the input with -i and the output file at the end.
"#
    }

    fn get_executor_prompt(&self, context: &str, user_input: &str) -> String {
        format!(
            r#"You are the Media Processing Specialist Agent for Dexter. 
Your goal is to generate a valid `ffmpeg` command.

### GUIDELINES:
1. INPUT/OUTPUT: Always use `-i` for inputs. Place the output filename at the end of the command.
2. PRECISION: Use the exact filenames provided in the context.
3. OUTPUT ONLY: Output ONLY the command. No backticks, no markdown, no explanations.
4. SAFE DEFAULTS: If specific technical parameters (like bitrate) are not provided, use sensible defaults or skip them.

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
        let mut cmd_obj = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", cmd]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", cmd]);
            c
        };

        let cwd = std::env::current_dir()?;
        let output = cmd_obj.current_dir(cwd).output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(anyhow::anyhow!(
                String::from_utf8_lossy(&output.stderr).to_string()
            ))
        }
    }
}
