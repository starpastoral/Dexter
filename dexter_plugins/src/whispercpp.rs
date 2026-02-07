use crate::command_exec::parse_and_validate_command;
use crate::{LlmBridge, Plugin, PreviewContent, Progress};
use anyhow::Result;
use async_trait::async_trait;
use regex::Regex;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;

pub struct WhisperCppPlugin;

fn parse_whisper_command(cmd: &str) -> Result<Vec<String>> {
    parse_and_validate_command(cmd, "whisper-cli")
        .or_else(|_| parse_and_validate_command(cmd, "whisper-cpp"))
}

fn contains_flag(argv: &[String], flag: &str) -> bool {
    argv.iter().any(|a| a == flag)
}

fn contains_flag_with_value(argv: &[String], flag: &str) -> bool {
    let prefix = format!("{}=", flag);
    argv.iter().any(|a| a.starts_with(&prefix))
}

fn has_value_for_flag(argv: &[String], short: &str, long: &str) -> bool {
    for (idx, arg) in argv.iter().enumerate() {
        if arg == short || arg == long {
            if let Some(value) = argv.get(idx + 1) {
                if !value.trim().is_empty() {
                    return true;
                }
            }
        }

        if let Some(value) = arg.strip_prefix(&format!("{}=", long)) {
            if !value.trim().is_empty() {
                return true;
            }
        }
    }

    false
}

fn extract_percentage(re: &Regex, line: &str) -> Option<f64> {
    let caps = re.captures(line)?;
    let value = caps.get(1)?.as_str().parse::<f64>().ok()?;
    Some(value.clamp(0.0, 100.0))
}

fn validate_whisper_command(cmd: &str) -> bool {
    let argv = match parse_whisper_command(cmd) {
        Ok(v) => v,
        Err(_) => return false,
    };

    if argv.iter().any(|a| a.starts_with('@')) {
        return false;
    }

    // Keep the command scope predictable and file-driven.
    if !has_value_for_flag(&argv, "-m", "--model") {
        return false;
    }
    if !has_value_for_flag(&argv, "-f", "--file") {
        return false;
    }

    let blocked_flags = ["--grammar", "--grammar-rule", "--grammar-penalty"];
    if blocked_flags
        .iter()
        .any(|flag| contains_flag(&argv, flag) || contains_flag_with_value(&argv, flag))
    {
        return false;
    }

    true
}

#[async_trait]
impl Plugin for WhisperCppPlugin {
    fn name(&self) -> &str {
        "whisper-cpp"
    }

    fn description(&self) -> &str {
        "Local speech-to-text via whisper.cpp (transcription, translation, and subtitle outputs)."
    }

    async fn is_installed(&self) -> bool {
        let has_whisper_cli = Command::new("whisper-cli")
            .arg("-h")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        let has_whisper_cpp = Command::new("whisper-cpp")
            .arg("-h")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        has_whisper_cli || has_whisper_cpp
    }

    async fn install(&self) -> Result<()> {
        Err(anyhow::anyhow!(
            "Please install whisper.cpp manually:\n- macOS (brew): brew install whisper-cpp\n- Or build from source: https://github.com/ggml-org/whisper.cpp"
        ))
    }

    fn get_doc_for_router(&self) -> &str {
        "Best for local audio transcription/translation with whisper.cpp, including TXT/SRT/VTT/JSON subtitle outputs."
    }

    fn get_doc_for_executor(&self) -> &str {
        r#"whisper.cpp Command Usage (whisper-cli):
- Basic transcription: whisper-cli -m models/ggml-base.en.bin -f input.wav -otxt
- SRT subtitle output: whisper-cli -m models/ggml-base.en.bin -f input.wav -osrt -of output/base
- Translate to English: whisper-cli -m models/ggml-base.bin -f input.wav -l de -tr -otxt
- Multiple outputs: whisper-cli -m models/ggml-base.en.bin -f input.wav -otxt -osrt -ovtt -ojf

Safety Constraints:
1. Always provide explicit model path via -m/--model.
2. Always provide explicit input audio file via -f/--file.
3. Do NOT use grammar injection flags (`--grammar`, `--grammar-rule`, `--grammar-penalty`).
4. Use output flags (-otxt/-osrt/-ovtt/-oj/-ojf) and optional -of prefix for deterministic files."#
    }

    fn get_executor_prompt(&self, context: &str, user_input: &str) -> String {
        format!(
            r#"You are the Speech Transcription Specialist Agent for Dexter.
Your goal is to generate a valid `whisper-cli` command for whisper.cpp.

### HARD CONSTRAINTS (MUST FOLLOW):
1. OUTPUT ONLY: Output ONLY the command. No backticks, no markdown, no explanations.
2. NO SHELL CHAINS: Do NOT use pipes, `&&`, `||`, `;`, backticks, or `$()`.
3. REQUIRED FLAGS: Always include `-m/--model` and `-f/--file`.
4. BLOCKED FLAGS: Do NOT use `--grammar`, `--grammar-rule`, `--grammar-penalty`.
5. USE whisper-cli: Generate command with `whisper-cli` (not shell wrappers).
6. PRECISION: Treat paths and filenames as literal strings from context.

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
        validate_whisper_command(cmd)
    }

    async fn dry_run(&self, cmd: &str, llm: Option<&dyn LlmBridge>) -> Result<PreviewContent> {
        if let Some(llm) = llm {
            let system_prompt = "You are a clear and concise command explainer for Dexter. Describe what this whisper.cpp command will do, including model choice, input audio, language/translation behavior, and output formats (txt/srt/vtt/json). Output plain text only.";
            let text = llm.chat(system_prompt, cmd).await?;
            Ok(PreviewContent::Text(text))
        } else {
            Ok(PreviewContent::Text(format!(
                "Executing speech transcription command: {}",
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
        let argv = parse_whisper_command(cmd)?;
        if !validate_whisper_command(cmd) {
            return Err(anyhow::anyhow!(
                "Command failed whisper.cpp validation logic"
            ));
        }

        let _ = progress_tx
            .send(Progress {
                percentage: None,
                message: "Running whisper.cpp transcription...".to_string(),
            })
            .await;

        let mut command = tokio::process::Command::new(&argv[0]);
        command
            .args(argv.iter().skip(1))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn()?;

        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stderr"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stdout"))?;

        let progress_re = Arc::new(Regex::new(r"(?i)\b(\d{1,3}(?:\.\d+)?)%\b").unwrap());

        let tx_err = progress_tx.clone();
        let re_err = progress_re.clone();
        let mut stderr_reader = tokio::io::BufReader::new(stderr).lines();
        let stderr_handle = tokio::spawn(async move {
            let mut captured_err = String::new();
            while let Ok(Some(line)) = stderr_reader.next_line().await {
                if let Some(pct) = extract_percentage(&re_err, &line) {
                    let _ = tx_err
                        .send(Progress {
                            percentage: Some(pct),
                            message: format!("Transcribing: {:.1}%", pct),
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
                        .send(Progress {
                            percentage: Some(pct),
                            message: format!("Transcribing: {:.1}%", pct),
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

        let _ = progress_tx
            .send(Progress {
                percentage: None,
                message: "Finalizing transcription output...".to_string(),
            })
            .await;

        if status.success() {
            let combined = format!("{}\n{}", out_output, err_output);
            Ok(if combined.trim().is_empty() {
                "Command executed successfully (no output)".to_string()
            } else {
                combined
            })
        } else {
            Err(anyhow::anyhow!("whisper.cpp error:\n{}", err_output))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_allows_common_whisper_workflows() {
        assert!(validate_whisper_command(
            "whisper-cli -m models/ggml-base.en.bin -f input.wav -otxt"
        ));
        assert!(validate_whisper_command(
            "whisper-cli -m models/ggml-base.bin -f speech.mp3 -l auto -osrt -of out/transcript"
        ));
        assert!(validate_whisper_command(
            "whisper-cpp -m models/ggml-base.bin -f sample.ogg -tr -ovtt"
        ));
    }

    #[test]
    fn validate_rejects_shell_injection() {
        assert!(!validate_whisper_command(
            "whisper-cli -m model.bin -f input.wav -otxt; rm -rf /"
        ));
        assert!(!validate_whisper_command(
            "whisper-cli -m model.bin -f input.wav && echo hacked"
        ));
    }

    #[test]
    fn validate_rejects_blocked_or_missing_required_flags() {
        assert!(!validate_whisper_command(
            "whisper-cli -m model.bin -f input.wav --grammar rules.gbnf"
        ));
        assert!(!validate_whisper_command("whisper-cli -f input.wav -otxt"));
        assert!(!validate_whisper_command("whisper-cli -m model.bin -otxt"));
    }
}
