use anyhow::{Result, anyhow};
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
                return Err(anyhow!("Command blocked by safety guard. Pattern matched: {}", pattern));
            }
        }
        
        // Additional heuristic: blocked characters
        if trimmed.contains(" > /dev/") || trimmed.contains(" > /sys/") {
             return Err(anyhow!("Command blocked: potentially destructive redirection"));
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
    }
}
