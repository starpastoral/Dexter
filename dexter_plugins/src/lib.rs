pub mod f2;
pub mod ffmpeg;

pub use f2::F2Plugin;
pub use ffmpeg::FFmpegPlugin;

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait LlmBridge: Send + Sync {
    async fn chat(&self, system: &str, user: &str) -> Result<String>;
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
    async fn dry_run(&self, cmd: &str, llm: Option<&dyn LlmBridge>) -> Result<PreviewContent>;
    async fn execute(&self, cmd: &str) -> Result<String>;
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
}
