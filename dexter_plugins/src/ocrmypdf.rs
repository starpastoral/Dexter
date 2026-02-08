use crate::command_exec::parse_and_validate_command;
use crate::{LlmBridge, Plugin, PreviewContent, Progress};
use anyhow::Result;
use async_trait::async_trait;
use regex::Regex;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;

pub struct OcrmypdfPlugin;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OcrMode {
    Default,
    Force,
    Skip,
    Redo,
}

fn contains_flag(argv: &[String], flag: &str) -> bool {
    argv.iter().any(|a| a == flag)
}

fn contains_flag_with_value(argv: &[String], flag: &str) -> bool {
    let prefix = format!("{}=", flag);
    argv.iter().any(|a| a.starts_with(&prefix))
}

fn parse_mode(argv: &[String]) -> Option<OcrMode> {
    for (idx, arg) in argv.iter().enumerate() {
        if arg == "--mode" || arg == "-m" {
            let value = argv.get(idx + 1)?;
            return mode_from_str(value);
        }
        if let Some(value) = arg.strip_prefix("--mode=") {
            return mode_from_str(value);
        }
    }
    None
}

fn mode_from_str(value: &str) -> Option<OcrMode> {
    match value {
        "default" => Some(OcrMode::Default),
        "force" => Some(OcrMode::Force),
        "skip" => Some(OcrMode::Skip),
        "redo" => Some(OcrMode::Redo),
        _ => None,
    }
}

fn extract_percentage(re: &Regex, line: &str) -> Option<f64> {
    let caps = re.captures(line)?;
    let value = caps.get(1)?.as_str().parse::<f64>().ok()?;
    Some(value.clamp(0.0, 100.0))
}

fn validate_ocrmypdf_command(cmd: &str) -> bool {
    let argv = match parse_and_validate_command(cmd, "ocrmypdf") {
        Ok(v) => v,
        Err(_) => return false,
    };

    if argv.iter().any(|a| a.starts_with('@')) {
        return false;
    }

    let blocked_flags = [
        "--plugin",
        "--keep-temporary-files",
        "--invalidate-digital-signatures",
        "--unpaper-args",
    ];
    if blocked_flags
        .iter()
        .any(|flag| contains_flag(&argv, flag) || contains_flag_with_value(&argv, flag))
    {
        return false;
    }
    if contains_flag(&argv, "-k") {
        return false;
    }

    let mode = parse_mode(&argv);
    let force_selected = contains_flag(&argv, "--force-ocr") || mode == Some(OcrMode::Force);
    let skip_selected = contains_flag(&argv, "--skip-text") || mode == Some(OcrMode::Skip);
    let redo_selected = contains_flag(&argv, "--redo-ocr") || mode == Some(OcrMode::Redo);

    let strategy_count = [force_selected, skip_selected, redo_selected]
        .iter()
        .filter(|selected| **selected)
        .count();

    if strategy_count > 1 {
        return false;
    }

    let has_mode_flag = contains_flag(&argv, "--mode")
        || contains_flag(&argv, "-m")
        || contains_flag_with_value(&argv, "--mode");
    if has_mode_flag && mode.is_none() {
        return false;
    }

    true
}

#[async_trait]
impl Plugin for OcrmypdfPlugin {
    fn name(&self) -> &str {
        "ocrmypdf"
    }

    fn description(&self) -> &str {
        "OCR scanned PDFs into searchable PDF/PDF-A with language and cleanup controls."
    }

    async fn is_installed(&self) -> bool {
        Command::new("ocrmypdf")
            .arg("--version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn install(&self) -> Result<()> {
        Err(anyhow::anyhow!(
            "Please install OCRmyPDF manually:\n- macOS (brew): brew install ocrmypdf\n- Debian/Ubuntu: sudo apt install ocrmypdf\n- pipx: pipx install ocrmypdf"
        ))
    }

    fn get_doc_for_router(&self) -> &str {
        "Best for making scanned PDFs searchable with OCR, language selection, deskew/rotation cleanup, and sidecar text extraction."
    }

    fn get_doc_for_executor(&self) -> &str {
        r#"ocrmypdf Command Usage:
- Basic OCR: ocrmypdf input.pdf output.pdf
- OCR with languages: ocrmypdf -l eng+deu input.pdf output.pdf
- Skip pages that already have text: ocrmypdf --skip-text input.pdf output.pdf
- Re-do existing OCR layer: ocrmypdf --redo-ocr input.pdf output.pdf
- Force OCR for all pages: ocrmypdf --force-ocr input.pdf output.pdf
- Rotate + deskew: ocrmypdf --rotate-pages --deskew input.pdf output.pdf
- Generate sidecar text: ocrmypdf --sidecar output.txt input.pdf output.pdf

Safety Constraints:
1. Do NOT use --plugin.
2. Do NOT use -k/--keep-temporary-files.
3. Do NOT use --invalidate-digital-signatures.
4. Do NOT use --unpaper-args.
5. OCR mode flags are mutually exclusive: choose one of force/skip/redo/default.
6. Use input/output file arguments directly (no shell redirection)."#
    }

    fn get_executor_prompt(&self, context: &str, user_input: &str) -> String {
        format!(
            r#"You are the OCR Specialist Agent for Dexter.
Your goal is to generate a valid `ocrmypdf` command.

### HARD CONSTRAINTS (MUST FOLLOW):
1. OUTPUT ONLY: Output ONLY the command. No backticks, no markdown, no explanations.
2. NO SHELL CHAINS: Do NOT use pipes, `&&`, `||`, `;`, backticks, or `$()`.
3. BLOCKED FLAGS: Do NOT use `--plugin`, `-k`, `--keep-temporary-files`, `--invalidate-digital-signatures`, or `--unpaper-args`.
4. MODE RULE: Do NOT combine `--force-ocr`, `--skip-text`, `--redo-ocr`, or conflicting `--mode` values.
5. SCOPE: Prefer these workflows: basic OCR, `-l/--language`, `--rotate-pages`, `--deskew`, one OCR mode strategy, `--sidecar`.
6. PRECISION: Treat filenames/paths as literal strings from context.
7. DEFAULT ROBUSTNESS: If user did not explicitly request force/redo, prefer `--skip-text` to avoid failing on PDFs that already contain text layers.

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
        validate_ocrmypdf_command(cmd)
    }

    async fn dry_run(&self, cmd: &str, llm: Option<&dyn LlmBridge>) -> Result<PreviewContent> {
        if let Some(llm) = llm {
            let system_prompt = "You are a clear and concise command explainer for Dexter. Describe what this OCRmyPDF command will do, including OCR mode, language, cleanup options, and output artifacts like sidecar files. Output plain text only.";
            let text = llm.chat(system_prompt, cmd).await?;
            Ok(PreviewContent::Text(text))
        } else {
            Ok(PreviewContent::Text(format!(
                "Executing OCR command: {}",
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
        let argv = parse_and_validate_command(cmd, "ocrmypdf")?;
        if !validate_ocrmypdf_command(cmd) {
            return Err(anyhow::anyhow!("Command failed ocrmypdf validation logic"));
        }

        let _ = progress_tx
            .send(Progress {
                percentage: None,
                message: "Running OCRmyPDF...".to_string(),
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
                            message: format!("OCR progress: {:.1}%", pct),
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
                            message: format!("OCR progress: {:.1}%", pct),
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
                message: "Finalizing OCR output...".to_string(),
            })
            .await;

        if status.success() {
            let combined = format!("{}\n{}", out_output, err_output);
            Ok(if combined.trim().is_empty() {
                "Command executed successfully (no output)".to_string()
            } else {
                combined
            })
        } else if should_retry_with_skip_text(&argv, &err_output) {
            let _ = progress_tx
                .send(Progress {
                    percentage: None,
                    message: "Retrying with --skip-text (existing text layer detected)..."
                        .to_string(),
                })
                .await;

            let retry_argv = inject_skip_text_arg(&argv);
            let retry_output = tokio::process::Command::new(&retry_argv[0])
                .args(retry_argv.iter().skip(1))
                .output()
                .await?;

            let retry_stdout = String::from_utf8_lossy(&retry_output.stdout).to_string();
            let retry_stderr = String::from_utf8_lossy(&retry_output.stderr).to_string();
            if retry_output.status.success() {
                let combined = format!(
                    "Initial run failed due to existing text layer; retried with --skip-text.\n{}\n{}",
                    retry_stdout, retry_stderr
                );
                Ok(combined)
            } else {
                Err(anyhow::anyhow!(
                    "ocrmypdf error (initial + retry with --skip-text):\ninitial:\n{}\nretry:\n{}\n{}",
                    err_output,
                    retry_stdout,
                    retry_stderr
                ))
            }
        } else {
            Err(anyhow::anyhow!("ocrmypdf error:\n{}", err_output))
        }
    }
}

fn should_retry_with_skip_text(argv: &[String], err_output: &str) -> bool {
    if contains_flag(argv, "--skip-text")
        || contains_flag(argv, "--force-ocr")
        || contains_flag(argv, "--redo-ocr")
    {
        return false;
    }
    if parse_mode(argv) == Some(OcrMode::Force) || parse_mode(argv) == Some(OcrMode::Redo) {
        return false;
    }
    let err = err_output.to_lowercase();
    err.contains("already has text")
        || err.contains("priorocrfounderror")
        || err.contains("use --skip-text")
}

fn inject_skip_text_arg(argv: &[String]) -> Vec<String> {
    if argv.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(argv.len() + 1);
    out.push(argv[0].clone());
    out.push("--skip-text".to_string());
    out.extend(argv.iter().skip(1).cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_allows_common_ocr_workflows() {
        assert!(validate_ocrmypdf_command("ocrmypdf input.pdf output.pdf"));
        assert!(validate_ocrmypdf_command(
            "ocrmypdf -l eng+deu input.pdf output.pdf"
        ));
        assert!(validate_ocrmypdf_command(
            "ocrmypdf --skip-text input.pdf output.pdf"
        ));
        assert!(validate_ocrmypdf_command(
            "ocrmypdf --rotate-pages --deskew input.pdf output.pdf"
        ));
        assert!(validate_ocrmypdf_command(
            "ocrmypdf --sidecar output.txt input.pdf output.pdf"
        ));
        assert!(validate_ocrmypdf_command(
            "ocrmypdf \"input file.pdf\" \"output file.pdf\""
        ));
    }

    #[test]
    fn validate_rejects_shell_injection() {
        assert!(!validate_ocrmypdf_command(
            "ocrmypdf input.pdf output.pdf && echo hacked"
        ));
        assert!(!validate_ocrmypdf_command(
            "ocrmypdf input.pdf output.pdf; rm -rf /"
        ));
    }

    #[test]
    fn validate_rejects_blocked_flags() {
        assert!(!validate_ocrmypdf_command(
            "ocrmypdf --plugin myplugin input.pdf output.pdf"
        ));
        assert!(!validate_ocrmypdf_command(
            "ocrmypdf -k input.pdf output.pdf"
        ));
        assert!(!validate_ocrmypdf_command(
            "ocrmypdf --keep-temporary-files input.pdf output.pdf"
        ));
        assert!(!validate_ocrmypdf_command(
            "ocrmypdf --invalidate-digital-signatures input.pdf output.pdf"
        ));
        assert!(!validate_ocrmypdf_command(
            "ocrmypdf --clean --unpaper-args '--layout double' input.pdf output.pdf"
        ));
    }

    #[test]
    fn validate_rejects_conflicting_mode_options() {
        assert!(!validate_ocrmypdf_command(
            "ocrmypdf --force-ocr --skip-text input.pdf output.pdf"
        ));
        assert!(!validate_ocrmypdf_command(
            "ocrmypdf --redo-ocr -m force input.pdf output.pdf"
        ));
        assert!(!validate_ocrmypdf_command(
            "ocrmypdf -m skip --force-ocr input.pdf output.pdf"
        ));
        assert!(validate_ocrmypdf_command(
            "ocrmypdf -m skip input.pdf output.pdf"
        ));
    }

    #[test]
    fn retry_detection_for_existing_text_layer_errors() {
        let argv = vec![
            "ocrmypdf".to_string(),
            "input.pdf".to_string(),
            "output.pdf".to_string(),
        ];
        assert!(should_retry_with_skip_text(
            &argv,
            "PriorOcrFoundError: page already has text"
        ));
        assert!(should_retry_with_skip_text(
            &argv,
            "Use --skip-text to bypass pages that already have text"
        ));
    }

    #[test]
    fn retry_disabled_when_mode_explicitly_selected() {
        let argv = vec![
            "ocrmypdf".to_string(),
            "--force-ocr".to_string(),
            "input.pdf".to_string(),
            "output.pdf".to_string(),
        ];
        assert!(!should_retry_with_skip_text(&argv, "page already has text"));
    }

    #[test]
    fn inject_skip_text_places_flag_after_binary() {
        let argv = vec![
            "ocrmypdf".to_string(),
            "-l".to_string(),
            "eng".to_string(),
            "input.pdf".to_string(),
            "output.pdf".to_string(),
        ];
        let injected = inject_skip_text_arg(&argv);
        assert_eq!(injected[0], "ocrmypdf");
        assert_eq!(injected[1], "--skip-text");
        assert_eq!(injected[2], "-l");
    }
}
