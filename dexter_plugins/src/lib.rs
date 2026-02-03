pub mod f2;
pub mod ffmpeg;
pub mod ytdlp;

pub use f2::F2Plugin;
pub use ffmpeg::FFmpegPlugin;
pub use ytdlp::YtDlpPlugin;

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait LlmBridge: Send + Sync {
    async fn chat(&self, system: &str, user: &str) -> Result<String>;
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub percentage: Option<f64>, // 0.0 - 100.0
    pub message: String,
}

#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;

    // Installation
    async fn is_installed(&self) -> bool;
    async fn install(&self) -> Result<()>;

    // AI Interaction
    fn get_doc_for_router(&self) -> &str; // Short description
    fn get_doc_for_executor(&self) -> &str; // Full docs
    fn get_executor_prompt(&self, context: &str, user_input: &str) -> String; // Full system prompt

    // Execution
    fn validate_command(&self, cmd: &str) -> bool;
    async fn execute(&self, cmd: &str) -> Result<String>;
    async fn dry_run(&self, cmd: &str, llm: Option<&dyn LlmBridge>) -> Result<PreviewContent>;

    // New method with default implementation
    async fn execute_with_progress(
        &self,
        cmd: &str,
        _progress_tx: tokio::sync::mpsc::Sender<Progress>,
    ) -> Result<String> {
        // Default: just call execute
        self.execute(cmd).await
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PreviewContent {
    Text(String),
    DiffList(Vec<DiffItem>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffItem {
    pub original: String,
    pub new: String,
    pub status: Option<String>,
}
