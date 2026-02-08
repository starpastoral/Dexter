use anyhow::Result;
use tokio::fs;

#[derive(Debug, Clone)]
pub struct FileContext {
    pub files: Vec<String>,
    pub summary: Option<String>,
}

pub struct ContextScanner;

impl ContextScanner {
    pub async fn scan_cwd() -> Result<FileContext> {
        let cwd = std::env::current_dir()?;
        let mut entries = fs::read_dir(cwd).await?;
        let mut files = Vec::new();
        let mut file_count = 0;
        let mut dir_count = 0;

        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if file_type.is_file() {
                if let Ok(name) = entry.file_name().into_string() {
                    if !name.starts_with('.') {
                        files.push(name);
                        file_count += 1;
                    }
                }
            } else if file_type.is_dir() {
                dir_count += 1;
            }
        }

        files.sort();

        if files.len() > 20 {
            // Fallback to summary
            let summary = format!(
                "Directory contains {} files and {} subdirectories.\nTop 5 files:\n{}",
                file_count,
                dir_count,
                files
                    .iter()
                    .take(5)
                    .enumerate()
                    .map(|(i, f)| format!("{}. {}", i + 1, f))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            Ok(FileContext {
                files: files.into_iter().take(20).collect(),
                summary: Some(summary),
            })
        } else {
            Ok(FileContext {
                files,
                summary: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_scan_cwd_limits() -> Result<()> {
        let dir = tempdir()?;
        let dir_path = dir.path();

        // Create 25 files
        for i in 0..25 {
            File::create(dir_path.join(format!("file_{}.txt", i)))?;
        }

        // Change CWD to temp dir for test
        let original_cwd = std::env::current_dir()?;
        std::env::set_current_dir(dir_path)?;

        let context = ContextScanner::scan_cwd().await?;

        assert_eq!(context.files.len(), 20); // Limit is 20
        assert!(context.summary.is_some());
        assert!(context.summary.unwrap().contains("25 files"));

        std::env::set_current_dir(original_cwd)?;
        Ok(())
    }
}
