use crate::command_exec::{parse_and_validate_command, spawn_checked_async};
use crate::{LlmBridge, Plugin, PreviewContent, Progress};
use anyhow::Result;
use async_trait::async_trait;
use std::process::Command;

pub struct QpdfPlugin;

fn contains_flag(argv: &[String], flag: &str) -> bool {
    argv.iter().any(|a| a == flag)
}

fn contains_flag_with_value(argv: &[String], flag: &str) -> bool {
    let prefix = format!("{}=", flag);
    argv.iter().any(|a| a.starts_with(&prefix))
}

fn has_bits_256(argv: &[String]) -> bool {
    argv.windows(2).any(|w| w[0] == "--bits" && w[1] == "256")
        || contains_flag_with_value(argv, "--bits")
            && argv
                .iter()
                .any(|a| a == "--bits=256" || a.eq_ignore_ascii_case("--bits=256"))
}

fn has_pages_terminator(argv: &[String]) -> bool {
    if !contains_flag(argv, "--pages") {
        return true;
    }

    // qpdf page-selection syntax requires `--` to terminate pages arguments.
    argv.iter().any(|a| a == "--")
}

fn contains_argfile(argv: &[String]) -> bool {
    argv.iter().any(|a| a.starts_with('@'))
}

fn validate_qpdf_command(cmd: &str) -> bool {
    let argv = match parse_and_validate_command(cmd, "qpdf") {
        Ok(v) => v,
        Err(_) => return false,
    };

    if contains_argfile(&argv) {
        return false;
    }

    let blocked_flags = ["--replace-input", "--allow-weak-crypto", "--allow-insecure"];
    if blocked_flags.iter().any(|flag| {
        contains_flag(&argv, flag) || argv.iter().any(|a| a.starts_with(&format!("{}=", flag)))
    }) {
        return false;
    }

    let has_check = contains_flag(&argv, "--check");
    let has_linearize = contains_flag(&argv, "--linearize");
    let has_decrypt = contains_flag(&argv, "--decrypt");
    let has_encrypt = contains_flag(&argv, "--encrypt");
    let has_pages = contains_flag(&argv, "--pages");

    // Keep qpdf scope explicit to the supported workflows.
    if !(has_check || has_linearize || has_decrypt || has_encrypt || has_pages) {
        return false;
    }

    // `--check` is an inspection workflow; avoid mixing with transformation flags.
    if has_check && (has_linearize || has_decrypt || has_encrypt || has_pages) {
        return false;
    }

    if has_encrypt && !has_bits_256(&argv) {
        return false;
    }

    if !has_pages_terminator(&argv) {
        return false;
    }

    true
}

#[async_trait]
impl Plugin for QpdfPlugin {
    fn name(&self) -> &str {
        "qpdf"
    }

    fn description(&self) -> &str {
        "PDF structural transformations: check, linearize, encrypt/decrypt, and page selection."
    }

    async fn is_installed(&self) -> bool {
        Command::new("qpdf")
            .arg("--version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn install(&self) -> Result<()> {
        Err(anyhow::anyhow!(
            "Please install qpdf manually:\n- macOS (brew): brew install qpdf\n- Debian/Ubuntu: sudo apt install qpdf\n- Windows (choco): choco install qpdf"
        ))
    }

    fn get_doc_for_router(&self) -> &str {
        "Best for PDF structural operations: validation checks, web linearization, page extraction/merge, and encryption/decryption."
    }

    fn get_doc_for_executor(&self) -> &str {
        r#"qpdf Command Usage:
- Check PDF syntax/structure: qpdf --check input.pdf
- Linearize for web viewing: qpdf --linearize input.pdf output.pdf
- Decrypt using password: qpdf --password=secret --decrypt input.pdf output.pdf
- Encrypt (256-bit only): qpdf --encrypt --user-password=u --owner-password=o --bits=256 -- input.pdf output.pdf
- Extract/merge pages: qpdf --empty --pages a.pdf b.pdf 1-z:even -- out.pdf

Safety Constraints:
1. Do NOT use --replace-input.
2. Do NOT use --allow-weak-crypto or --allow-insecure.
3. Do NOT use @argfile syntax (e.g., @args.txt).
4. For --pages commands, include `--` to terminate page-selection arguments.
5. Encryption must use --bits=256."#
    }

    fn get_executor_prompt(&self, context: &str, user_input: &str) -> String {
        format!(
            r#"You are the PDF Structure Specialist Agent for Dexter.
Your goal is to generate a valid `qpdf` command.

### HARD CONSTRAINTS (MUST FOLLOW):
1. OUTPUT ONLY: Output ONLY the command. No backticks, no markdown, no explanations.
2. NO SHELL CHAINS: Do NOT use pipes, `&&`, `||`, `;`, backticks, or `$()`.
3. BLOCKED FLAGS: Do NOT use `--replace-input`, `--allow-weak-crypto`, `--allow-insecure`.
4. NO ARG FILES: Do NOT use `@filename` argument-file syntax.
5. ENCRYPTION POLICY: If using `--encrypt`, you MUST use `--bits=256`.
6. SCOPE: Prefer these workflows only: `--check`, `--linearize`, `--decrypt`, `--encrypt`, `--pages ... --`.
7. PAGES SYNTAX: If using `--pages`, include terminating `--` before output.
8. PRECISION: Treat paths and filenames as literal strings from context.
9. ARG SHAPE:
   - `--check` should only inspect input PDF (no transform output path).
   - `--linearize`, `--decrypt`, and `--encrypt` require explicit input + output paths.

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
        validate_qpdf_command(cmd)
    }

    async fn dry_run(&self, cmd: &str, llm: Option<&dyn LlmBridge>) -> Result<PreviewContent> {
        if let Some(llm) = llm {
            let system_prompt = "You are a clear and concise command explainer for Dexter. Describe what this qpdf command will do in plain language. Mention input/output files, whether it checks, linearizes, decrypts, encrypts, or selects pages. Output plain text only.";
            let text = llm.chat(system_prompt, cmd).await?;
            Ok(PreviewContent::Text(text))
        } else {
            Ok(PreviewContent::Text(format!(
                "Executing PDF structure command: {}",
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
        let argv = parse_and_validate_command(cmd, "qpdf")?;
        if !validate_qpdf_command(cmd) {
            return Err(anyhow::anyhow!("Command failed qpdf validation logic"));
        }

        let phase = if argv.iter().any(|a| a == "--check") {
            "Checking PDF structure..."
        } else if argv.iter().any(|a| a == "--linearize") {
            "Linearizing PDF..."
        } else if argv.iter().any(|a| a == "--decrypt") {
            "Decrypting PDF..."
        } else if argv.iter().any(|a| a == "--encrypt") {
            "Encrypting PDF (256-bit)..."
        } else if argv.iter().any(|a| a == "--pages") {
            "Selecting/merging PDF pages..."
        } else {
            "Processing PDF with qpdf..."
        };

        let _ = progress_tx
            .send(Progress {
                percentage: None,
                message: phase.to_string(),
            })
            .await;

        let cwd = std::env::current_dir()?;
        let output = spawn_checked_async(&argv, cwd).await?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(format!("{}{}", stdout, stderr))
        } else {
            Err(anyhow::anyhow!("qpdf error: {}\n{}", stdout, stderr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_allows_supported_qpdf_workflows() {
        assert!(validate_qpdf_command("qpdf --check input.pdf"));
        assert!(validate_qpdf_command(
            "qpdf --linearize input.pdf output.pdf"
        ));
        assert!(validate_qpdf_command(
            "qpdf --password=secret --decrypt input.pdf output.pdf"
        ));
        assert!(validate_qpdf_command(
            "qpdf --encrypt --user-password=u --owner-password=o --bits=256 -- input.pdf output.pdf"
        ));
        assert!(validate_qpdf_command(
            "qpdf --empty --pages a.pdf b.pdf 1-z:even -- out.pdf"
        ));
    }

    #[test]
    fn validate_rejects_shell_injection() {
        assert!(!validate_qpdf_command("qpdf --check input.pdf; rm -rf /"));
        assert!(!validate_qpdf_command(
            "qpdf --check input.pdf && echo hacked"
        ));
    }

    #[test]
    fn validate_rejects_blocked_and_unsafe_flags() {
        assert!(!validate_qpdf_command("qpdf --replace-input input.pdf"));
        assert!(!validate_qpdf_command(
            "qpdf --allow-weak-crypto --encrypt --bits=256 -- input.pdf output.pdf"
        ));
        assert!(!validate_qpdf_command(
            "qpdf --allow-insecure --encrypt --bits=256 -- input.pdf output.pdf"
        ));
        assert!(!validate_qpdf_command("qpdf @args.txt"));
        assert!(!validate_qpdf_command(
            "qpdf --encrypt --bits=128 -- input.pdf output.pdf"
        ));
    }

    #[test]
    fn validate_rejects_invalid_mode_combinations() {
        assert!(!validate_qpdf_command(
            "qpdf --check --linearize input.pdf output.pdf"
        ));
        assert!(!validate_qpdf_command(
            "qpdf --empty --pages a.pdf 1-z out.pdf"
        ));
    }

    #[test]
    fn validate_allows_quoted_paths() {
        assert!(validate_qpdf_command(
            "qpdf --linearize \"in file.pdf\" \"out file.pdf\""
        ));
    }
}
