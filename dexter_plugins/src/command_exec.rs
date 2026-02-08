use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use tokio::process::{Child, Command as TokioCommand};

const FORBIDDEN_EXACT_TOKENS: &[&str] = &[";", "&&", "||", "|", ">", "<", ">>", "<<"];
const FORBIDDEN_SUBSTRINGS: &[&str] = &["`", "$(", "${", ";", "&&", "||", "|", ">", "<"];

pub fn parse_and_validate_command(raw: &str, expected_program: &str) -> Result<Vec<String>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("Command is empty"));
    }

    let argv = shell_words::split(trimmed).map_err(|e| anyhow!("Invalid command syntax: {}", e))?;
    if argv.is_empty() {
        return Err(anyhow!("Command is empty"));
    }

    let program = &argv[0];
    if !program_matches(program, expected_program) {
        return Err(anyhow!(
            "Unexpected command program: expected `{}`, got `{}`",
            expected_program,
            program
        ));
    }

    for token in &argv {
        if FORBIDDEN_EXACT_TOKENS.contains(&token.as_str()) {
            return Err(anyhow!("Unsafe token detected: {}", token));
        }
        if FORBIDDEN_SUBSTRINGS.iter().any(|bad| token.contains(bad)) {
            return Err(anyhow!("Unsafe token detected: {}", token));
        }
    }

    Ok(argv)
}

pub fn spawn_checked(argv: &[String], cwd: impl AsRef<Path>) -> Result<Output> {
    if argv.is_empty() {
        return Err(anyhow!("Command is empty"));
    }

    let mut cmd = Command::new(&argv[0]);
    cmd.args(argv.iter().skip(1));
    let output = cmd.current_dir(cwd).output()?;
    Ok(output)
}

pub async fn spawn_checked_async(argv: &[String], cwd: impl AsRef<Path>) -> Result<Output> {
    if argv.is_empty() {
        return Err(anyhow!("Command is empty"));
    }

    let mut cmd = TokioCommand::new(&argv[0]);
    cmd.args(argv.iter().skip(1)).current_dir(cwd.as_ref());
    let output = cmd.output().await?;
    Ok(output)
}

pub fn spawn_checked_piped(argv: &[String], cwd: impl AsRef<Path>) -> Result<Child> {
    if argv.is_empty() {
        return Err(anyhow!("Command is empty"));
    }

    let mut cmd = TokioCommand::new(&argv[0]);
    cmd.args(argv.iter().skip(1))
        .current_dir(cwd.as_ref())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn()?;
    Ok(child)
}

pub fn contains_arg(argv: &[String], arg: &str) -> bool {
    argv.iter().any(|a| a == arg)
}

fn program_matches(actual: &str, expected: &str) -> bool {
    if actual == expected {
        return true;
    }

    if cfg!(windows) {
        let actual_lower = actual.to_ascii_lowercase();
        let expected_lower = expected.to_ascii_lowercase();
        return actual_lower == expected_lower || actual_lower == format!("{}.exe", expected_lower);
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_injection_tokens() {
        assert!(parse_and_validate_command("ffmpeg -i a.mp4 b.mp4; rm -rf /", "ffmpeg").is_err());
        assert!(parse_and_validate_command("yt-dlp \"url\" && echo x", "yt-dlp").is_err());
    }

    #[test]
    fn parse_allows_quoted_arguments() {
        let argv =
            parse_and_validate_command("pandoc \"in file.md\" -o \"out file.pdf\"", "pandoc")
                .expect("valid command");
        assert_eq!(argv[0], "pandoc");
        assert_eq!(argv[1], "in file.md");
        assert_eq!(argv[2], "-o");
        assert_eq!(argv[3], "out file.pdf");
    }

    #[test]
    fn spawn_checked_rejects_empty_argv() {
        assert!(spawn_checked(&[], ".").is_err());
    }

    #[tokio::test]
    async fn spawn_checked_async_rejects_empty_argv() {
        assert!(spawn_checked_async(&[], ".").await.is_err());
    }

    #[test]
    fn spawn_checked_piped_rejects_empty_argv() {
        assert!(spawn_checked_piped(&[], ".").is_err());
    }
}
