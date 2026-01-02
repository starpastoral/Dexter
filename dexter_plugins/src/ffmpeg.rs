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

### GUIDELINES:
1. MODERN SYNTAX: Use `-c:v`/`-c:a`. Place `-ss` before `-i`. Add `-movflags +faststart` for MP4.
2. INPUT/OUTPUT: Always use `-i` for inputs. Place the output filename at the end.
3. PRECISION: Use the exact filenames provided in the context.
4. OUTPUT ONLY: Output ONLY the command. No backticks, no markdown, no explanations.
5. SAFE DEFAULTS: If specific technical parameters are not provided, use sensible defaults.

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
