use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct SessionLogger {
    path: Option<PathBuf>,
}

impl SessionLogger {
    pub fn new() -> Self {
        let path = build_log_path();
        if let Some(path) = &path {
            if let Some(parent) = path.parent() {
                let _ = create_dir_all(parent);
            }
            let _ = OpenOptions::new().create(true).append(true).open(path);
        }
        Self { path }
    }

    pub fn display_path(&self) -> Option<String> {
        self.path.as_ref().map(|p| p.display().to_string())
    }

    pub fn event(&self, label: &str, message: &str) {
        let line = format!("[{}] {} {}", now_millis(), label, message);
        self.append_line(&line);
    }

    pub fn block(&self, label: &str, body: &str) {
        self.append_line(&format!("[{}] {} BEGIN", now_millis(), label));
        for line in body.lines() {
            self.append_line(line);
        }
        self.append_line(&format!("[{}] {} END", now_millis(), label));
    }

    fn append_line(&self, line: &str) {
        let Some(path) = &self.path else {
            return;
        };
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{}", line);
        }
    }
}

fn build_log_path() -> Option<PathBuf> {
    let base = dirs::data_dir().or_else(|| std::env::current_dir().ok())?;
    let mut path = base.join("dexter").join("logs");
    let ts = now_millis();
    path.push(format!("session-{}.log", ts));
    Some(path)
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
