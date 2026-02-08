use crate::command_exec::{parse_and_validate_command, spawn_checked_async};
use crate::{LlmBridge, Plugin, PreviewContent, Progress};
use anyhow::Result;
use async_trait::async_trait;
use regex::Regex;
use std::process::Command;
use std::sync::OnceLock;

pub struct PandocPlugin;

fn strip_surrounding_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn extract_output_path(cmd: &str) -> Option<String> {
    static OUTPUT_RE: OnceLock<Regex> = OnceLock::new();
    let output_re = OUTPUT_RE.get_or_init(|| {
        Regex::new(r#"(?i)(^|\s)(-o|--output)(\s+|=)("[^"]+"|'[^']+'|\S+)"#).unwrap()
    });

    if let Some(caps) = output_re.captures(cmd) {
        let raw = caps.get(4)?.as_str();
        return Some(strip_surrounding_quotes(raw).to_string());
    }

    // Support: -oout.ext (no space)
    static OUTPUT_COMPACT_RE: OnceLock<Regex> = OnceLock::new();
    let compact_re = OUTPUT_COMPACT_RE.get_or_init(|| Regex::new(r#"(?i)(^|\s)-o(\S+)"#).unwrap());
    let caps = compact_re.captures(cmd)?;
    let raw = caps.get(2)?.as_str();
    Some(strip_surrounding_quotes(raw).to_string())
}

fn validate_pandoc_command(cmd: &str) -> bool {
    if parse_and_validate_command(cmd, "pandoc").is_err() {
        return false;
    }

    let trimmed = cmd.trim();

    let banned_tokens = ["&&", "||", ";", "|", "`", "$(", ">", "<"];
    if banned_tokens.iter().any(|t| trimmed.contains(t)) {
        return false;
    }

    static BANNED_ARGS_RE: OnceLock<Regex> = OnceLock::new();
    let banned_args_re =
        BANNED_ARGS_RE.get_or_init(|| Regex::new(r"(?i)(^|\s)--(lua-)?filter(\s|=|$)").unwrap());
    if banned_args_re.is_match(trimmed) {
        return false;
    }

    // Disallow stdin/stdout usage: `pandoc - ...` or `... -o -`
    static DASH_TOKEN_RE: OnceLock<Regex> = OnceLock::new();
    let dash_token_re = DASH_TOKEN_RE.get_or_init(|| Regex::new(r"(^|\s)-(\s|$)").unwrap());
    if dash_token_re.is_match(trimmed) {
        return false;
    }

    let output_path = match extract_output_path(trimmed) {
        Some(path) => path,
        None => return false,
    };

    if output_path.trim() == "-" || output_path.trim().is_empty() {
        return false;
    }

    true
}

#[async_trait]
impl Plugin for PandocPlugin {
    fn name(&self) -> &str {
        "pandoc"
    }

    fn description(&self) -> &str {
        "A universal document converter (Markdown/DOCX/HTML/PDF and more)."
    }

    async fn is_installed(&self) -> bool {
        Command::new("pandoc")
            .arg("--version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn install(&self) -> Result<()> {
        Err(anyhow::anyhow!(
            "Please install pandoc manually:\n- macOS (brew): brew install pandoc\n- Windows (choco): choco install pandoc\n- Linux: use your package manager (e.g. apt/yum/pacman)\n\nNote: PDF output often requires a TeX engine (e.g. MacTeX/TeX Live)."
        ))
    }

    fn get_doc_for_router(&self) -> &str {
        "Best for converting documents between formats (Markdown/DOCX/HTML/PDF) and generating PDF/Word/HTML from Markdown."
    }

    fn get_doc_for_executor(&self) -> &str {
        r#"pandoc Command Usage:
- Markdown -> PDF: pandoc input.md -o output.pdf
- Markdown -> DOCX: pandoc input.md -o output.docx
- DOCX -> Markdown: pandoc input.docx -o output.md
- Markdown -> HTML: pandoc input.md -t html -o output.html

Notes:
1. Always specify output with -o/--output. Do NOT use shell redirection (>).
2. Do NOT use --filter or --lua-filter (blocked for safety).
3. PDF output may require a TeX engine (e.g. MacTeX/TeX Live) to be installed."#
    }

    fn get_executor_prompt(&self, context: &str, user_input: &str) -> String {
        format!(
            r#"You are the Document Conversion Specialist Agent for Dexter.
Your goal is to generate a valid `pandoc` command.

### HARD CONSTRAINTS (MUST FOLLOW):
1. OUTPUT ONLY: Output ONLY the command. No backticks, no markdown, no explanations.
2. NO SHELL CHAINS: Do NOT use pipes, `&&`, `||`, `;`, backticks, or `$()`.
3. NO REDIRECTION: Do NOT use `>` or `<`. Always use `-o`/`--output` to specify the output file.
4. REQUIRED OUTPUT: The command MUST include `-o <file>` or `--output=<file>` (or `--output <file>`).
5. NO FILTERS: Do NOT use `--filter` or `--lua-filter`.
6. NO STDIN/STDOUT: Do NOT use `-` as an input or output filename.
7. PRECISION: Treat filenames in the context as literal strings; use the exact characters.

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
        validate_pandoc_command(cmd)
    }

    async fn dry_run(&self, cmd: &str, llm: Option<&dyn LlmBridge>) -> Result<PreviewContent> {
        if let Some(llm) = llm {
            let system_prompt = "You are a clear and concise command explainer for Dexter. Describe what this pandoc command will do in simple terms. Mention input file(s), output file, and the output format. If output is PDF, mention that a TeX engine may be required. Output plain text only.";
            let text = llm.chat(system_prompt, cmd).await?;
            Ok(PreviewContent::Text(text))
        } else {
            Ok(PreviewContent::Text(format!(
                "Executing document conversion: {}",
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
        let _ = progress_tx
            .send(Progress {
                percentage: None,
                message: "Converting document...".to_string(),
            })
            .await;

        let argv = parse_and_validate_command(cmd, "pandoc")?;
        let cwd = std::env::current_dir()?;
        let output = spawn_checked_async(&argv, cwd).await?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(format!("{}{}", stdout, stderr))
        } else {
            Err(anyhow::anyhow!("pandoc error: {}\n{}", stdout, stderr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_allows_basic_conversions() {
        assert!(validate_pandoc_command("pandoc input.md -o output.pdf"));
        assert!(validate_pandoc_command(
            "pandoc \"in file.md\" -o \"out file.pdf\""
        ));
        assert!(validate_pandoc_command(
            "pandoc input.md --output=output.html -t html"
        ));
        assert!(validate_pandoc_command("pandoc input.md -ooutput.docx"));
    }

    #[test]
    fn validate_rejects_missing_output() {
        assert!(!validate_pandoc_command("pandoc input.md -t html"));
    }

    #[test]
    fn validate_rejects_shell_chains_and_redirection() {
        assert!(!validate_pandoc_command(
            "pandoc input.md -o out.html | cat"
        ));
        assert!(!validate_pandoc_command(
            "pandoc input.md -t html > out.html"
        ));
        assert!(!validate_pandoc_command(
            "pandoc input.md -o out.html && echo ok"
        ));
    }

    #[test]
    fn validate_rejects_filters() {
        assert!(!validate_pandoc_command(
            "pandoc input.md -o out.html --filter myfilter"
        ));
        assert!(!validate_pandoc_command(
            "pandoc input.md -o out.html --lua-filter=my.lua"
        ));
    }

    #[test]
    fn validate_rejects_stdin_stdout() {
        assert!(!validate_pandoc_command("pandoc - -o out.html"));
        assert!(!validate_pandoc_command("pandoc input.md -o -"));
        assert!(!validate_pandoc_command("pandoc input.md --output=\"-\""));
    }
}
