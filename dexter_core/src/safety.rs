use anyhow::{anyhow, Result};
use regex::Regex;

pub struct SafetyGuard {
    blacklist_patterns: Vec<Regex>,
}

impl Default for SafetyGuard {
    fn default() -> Self {
        Self {
            blacklist_patterns: vec![
                Regex::new(r"(?i)^rm\s+").unwrap(),
                Regex::new(r"(?i)^mv\s+/\s*").unwrap(),
                Regex::new(r"(?i)^dd\s+").unwrap(),
                Regex::new(r"(?i):.*\(\s*\)\s*\{\s*:.*\|.*:.*\}\s*;.*:").unwrap(), // fork bomb
                Regex::new(r"(?i)^sudo\s+rm").unwrap(),
                Regex::new(r"(?i)>\s*/dev/sd[a-z]").unwrap(), // writing to raw device
                Regex::new(r"(?i)mkfs").unwrap(),
            ],
        }
    }
}

impl SafetyGuard {
    pub fn check(&self, cmd: &str) -> Result<()> {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("Command is empty"));
        }

        for pattern in &self.blacklist_patterns {
            if pattern.is_match(trimmed) {
                return Err(anyhow!(
                    "Command blocked by safety guard. Pattern matched: {}",
                    pattern
                ));
            }
        }

        // Additional heuristics: shell composition and risky redirection.
        let shell_meta = ["&&", "||", ";", "|", "`", "$(", ">", "<"];
        if shell_meta.iter().any(|meta| trimmed.contains(meta)) {
            return Err(anyhow!("Command blocked: shell composition is not allowed"));
        }

        // Additional heuristic: blocked redirection targets
        if trimmed.contains(" > /dev/") || trimmed.contains(" > /sys/") {
            return Err(anyhow!(
                "Command blocked: potentially destructive redirection"
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_blacklist() {
        let guard = SafetyGuard::default();

        assert!(guard.check("ls -la").is_ok());
        assert!(guard.check("rm -rf /").is_err());
        assert!(guard.check("sudo rm something").is_err());
        assert!(guard.check("dd if=/dev/zero of=/dev/sda").is_err());
        assert!(guard.check("cat file > /dev/sda1").is_err());
        assert!(guard.check("ffmpeg -i a.mp4 b.mp4; rm -rf /").is_err());
        assert!(guard.check("yt-dlp \"url\" && echo hacked").is_err());
    }
}
