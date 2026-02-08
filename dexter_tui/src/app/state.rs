use anyhow::{anyhow, Result};
use dexter_core::{
    CachePolicy, ClarifyOption, Config, ContextScanner, Executor, LlmClient, RouteOutcome, Router,
    SafetyGuard,
};
use dexter_plugins::{
    F2Plugin, FFmpegPlugin, JdupesPlugin, LibvipsPlugin, OcrmypdfPlugin, PandocPlugin, Plugin,
    PreviewContent, QpdfPlugin, WhisperCppPlugin, YtDlpPlugin,
};
use ratatui::layout::Rect;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

use crate::app::editor::char_count;
use crate::app::session_log::SessionLogger;
use crate::theme::Theme;

#[derive(Clone, PartialEq, Debug)]
pub enum AppState {
    Input,
    Routing,
    Generating,
    AwaitingConfirmation,
    EditingCommand,
    Executing,
    Finished(String),
    Error(String),
    Clarifying,
    PendingRouting,
    PendingGeneration,
    PendingDryRun,
    DryRunning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusArea {
    Proposal,
    FooterButtons,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterAction {
    Submit,
    ClearInput,
    ToggleDebug,
    Settings,
    Quit,
    Retry,
    Execute,
    BackToInput,
    EditCommand,
    EditInput,
    Regenerate,
    PreviewEditedCommand,
    CancelEditCommand,
    ResetToInput,
    ClarifySelect(usize),
}

#[derive(Clone, Debug)]
pub struct FooterButton {
    pub rect: Rect,
    pub action: FooterAction,
}

#[derive(Clone, Debug)]
pub struct ClarifyPayload {
    pub question: String,
    pub options: Vec<ClarifyOption>,
}

pub struct App {
    pub state: AppState,
    pub input: String,
    pub input_cursor: usize,
    pub router: Router,
    pub executor: Executor,
    pub plugins: Vec<Arc<dyn Plugin>>,
    pub selected_plugin: Option<String>,
    pub generated_command: Option<String>,
    pub command_draft: String,
    pub command_cursor: usize,
    pub logs: Vec<String>,
    pub tick_count: u64,
    pub current_context: Option<dexter_core::context::FileContext>,
    pub dry_run_output: Option<PreviewContent>,
    pub show_debug: bool,
    pub config: Config,
    pub theme: Theme,
    pub notice: Option<String>,
    pub clarify: Option<ClarifyPayload>,
    pub focus: FocusArea,
    pub footer_buttons: Vec<FooterButton>,
    pub footer_focus: usize,
    pub output_scroll: u16,
    pub output_max_scroll: u16,
    pub output_text_width: u16,
    pub output_scrollbar_rect: Option<Rect>,
    pub proposal_rect: Option<Rect>,
    pub settings_button_rect: Option<Rect>,
    pub routing_result_rx: Option<oneshot::Receiver<Result<RouteOutcome>>>,
    pub generation_result_rx: Option<oneshot::Receiver<Result<String>>>,
    pub dry_run_result_rx: Option<oneshot::Receiver<Result<PreviewContent>>>,
    pub progress_rx: Option<mpsc::Receiver<dexter_plugins::Progress>>,
    pub execution_result_rx: Option<oneshot::Receiver<Result<String>>>,
    pub progress: Option<dexter_plugins::Progress>,
    pub last_progress_log_line: Option<String>,
    pub last_progress_log_at: Option<Instant>,
    pub generation_cache_policy: CachePolicy,
    pub pending_open_settings: bool,
    pub dirty: bool,
    pub session_logger: SessionLogger,
}

impl App {
    pub fn new(config: Config) -> Self {
        let providers = config.configured_providers();
        let router_client = LlmClient::with_routes(
            providers.clone(),
            config.models.router_routes.clone(),
            config.models.router_model.clone(),
            config.models.router_fallback_models.clone(),
        );
        let executor_client = LlmClient::with_routes(
            providers,
            config.models.executor_routes.clone(),
            config.models.executor_model.clone(),
            config.models.executor_fallback_models.clone(),
        );

        let theme = Theme::from_config(&config.theme);
        let session_logger = SessionLogger::new();
        let mut app = Self {
            state: AppState::Input,
            input: String::new(),
            input_cursor: 0,
            router: Router::new(router_client),
            executor: Executor::new(executor_client),
            plugins: vec![
                Arc::new(F2Plugin) as Arc<dyn Plugin>,
                Arc::new(FFmpegPlugin) as Arc<dyn Plugin>,
                Arc::new(PandocPlugin) as Arc<dyn Plugin>,
                Arc::new(QpdfPlugin) as Arc<dyn Plugin>,
                Arc::new(OcrmypdfPlugin) as Arc<dyn Plugin>,
                Arc::new(YtDlpPlugin) as Arc<dyn Plugin>,
                Arc::new(WhisperCppPlugin) as Arc<dyn Plugin>,
                Arc::new(JdupesPlugin) as Arc<dyn Plugin>,
                Arc::new(LibvipsPlugin) as Arc<dyn Plugin>,
            ],
            selected_plugin: None,
            generated_command: None,
            command_draft: String::new(),
            command_cursor: 0,
            logs: Vec::new(),
            tick_count: 0,
            current_context: None,
            dry_run_output: None,
            show_debug: false,
            config,
            theme,
            notice: None,
            clarify: None,
            focus: FocusArea::Proposal,
            footer_buttons: Vec::new(),
            footer_focus: 0,
            output_scroll: 0,
            output_max_scroll: 0,
            output_text_width: 0,
            output_scrollbar_rect: None,
            proposal_rect: None,
            settings_button_rect: None,
            routing_result_rx: None,
            generation_result_rx: None,
            dry_run_result_rx: None,
            progress_rx: None,
            execution_result_rx: None,
            progress: None,
            last_progress_log_line: None,
            last_progress_log_at: None,
            generation_cache_policy: CachePolicy::Normal,
            pending_open_settings: false,
            dirty: true,
            session_logger,
        };
        app.push_log("Dexter initialized. Ready for your command.");
        if let Some(path) = app.session_logger.display_path() {
            app.push_log(format!("Session log file: {}", path));
        }
        app
    }

    pub fn apply_config(&mut self, config: Config) {
        let providers = config.configured_providers();
        let router_client = LlmClient::with_routes(
            providers.clone(),
            config.models.router_routes.clone(),
            config.models.router_model.clone(),
            config.models.router_fallback_models.clone(),
        );
        let executor_client = LlmClient::with_routes(
            providers,
            config.models.executor_routes.clone(),
            config.models.executor_model.clone(),
            config.models.executor_fallback_models.clone(),
        );

        self.router = Router::new(router_client);
        self.executor = Executor::new(executor_client);
        self.theme = Theme::from_config(&config.theme);
        self.config = config;
        self.dirty = true;
    }

    pub async fn update_context(&mut self) -> Result<()> {
        let context = ContextScanner::scan_cwd().await?;
        let summary = format_context_lines(&context);
        self.session_logger.block("CONTEXT_SCAN", &summary);
        self.push_log(format!("Context scanned ({} files).", context.files.len()));
        self.current_context = Some(context);
        self.dirty = true;
        Ok(())
    }

    pub async fn execute_command(&mut self) -> Result<()> {
        if let Some(cmd) = self.generated_command.clone() {
            let plugin_name = self.selected_plugin.clone().unwrap_or_default();

            let plugin = self
                .plugins
                .iter()
                .find(|p| p.name() == plugin_name)
                .ok_or_else(|| anyhow!("Plugin not found"))?
                .clone();

            if let Err(e) = SafetyGuard::default().check(&cmd) {
                self.push_log(format!("Safety check failed before execution: {}", e));
                self.log_block("EXECUTE_BLOCKED", &format!("command={}\nreason={}", cmd, e));
                self.state = AppState::Error(format!("Safety check failed: {}", e));
                self.dirty = true;
                return Ok(());
            }
            if !plugin.validate_command(&cmd) {
                self.push_log("Plugin validation failed before execution.".to_string());
                self.log_block(
                    "EXECUTE_BLOCKED",
                    &format!("command={}\nreason=plugin_validation", cmd),
                );
                self.state = AppState::Error("Command failed plugin validation logic".to_string());
                self.dirty = true;
                return Ok(());
            }

            self.state = AppState::Executing;
            self.output_scroll = 0;
            self.push_log(format!("Executing [{}]: {}", plugin_name, cmd));
            if let Err(e) = self.executor.record_history(&plugin_name, &cmd).await {
                self.push_log(format!("History log failed: {}", e));
            }
            self.session_logger.block(
                "EXECUTE_COMMAND",
                &format!("plugin={}\ncommand={}", plugin_name, cmd),
            );

            let final_cmd = cmd;

            let (prog_tx, prog_rx) = mpsc::channel(10);
            let (res_tx, res_rx) = oneshot::channel();
            tokio::spawn(async move {
                let result = plugin.execute_with_progress(&final_cmd, prog_tx).await;
                let _ = res_tx.send(result);
            });

            self.progress_rx = Some(prog_rx);
            self.execution_result_rx = Some(res_rx);
            self.progress = None;
            self.last_progress_log_line = None;
            self.last_progress_log_at = None;
            self.dirty = true;
        }
        Ok(())
    }

    pub fn push_log<S: Into<String>>(&mut self, message: S) {
        let message = message.into();
        self.session_logger.event("LOG", &message);
        self.logs.push(message);
        const MAX_LOG_LINES: usize = 500;
        if self.logs.len() > MAX_LOG_LINES {
            let overflow = self.logs.len() - MAX_LOG_LINES;
            self.logs.drain(0..overflow);
        }
    }

    pub fn log_block(&self, label: &str, body: &str) {
        self.session_logger.block(label, body);
    }

    pub fn reset_for_new_request(&mut self) {
        self.generated_command = None;
        self.command_draft.clear();
        self.command_cursor = 0;
        self.dry_run_output = None;
        self.output_scroll = 0;
        self.selected_plugin = None;
        self.notice = None;
        self.clarify = None;
        self.generation_cache_policy = CachePolicy::Normal;
        self.routing_result_rx = None;
        self.generation_result_rx = None;
        self.dry_run_result_rx = None;
        self.progress_rx = None;
        self.execution_result_rx = None;
        self.progress = None;
        self.last_progress_log_line = None;
        self.last_progress_log_at = None;
        self.dirty = true;
    }

    pub fn reset_to_input_preserve_text(&mut self) {
        self.state = AppState::Input;
        self.generated_command = None;
        self.command_draft.clear();
        self.command_cursor = 0;
        self.dry_run_output = None;
        self.output_scroll = 0;
        self.selected_plugin = None;
        self.notice = None;
        self.clarify = None;
        self.generation_cache_policy = CachePolicy::Normal;
        self.last_progress_log_line = None;
        self.last_progress_log_at = None;
        self.input_cursor = char_count(&self.input);
        self.focus = FocusArea::Proposal;
        self.footer_focus = 0;
        self.dirty = true;
    }

    pub fn is_processing_state(&self) -> bool {
        matches!(
            self.state,
            AppState::Routing
                | AppState::Generating
                | AppState::Executing
                | AppState::DryRunning
                | AppState::PendingRouting
                | AppState::PendingGeneration
                | AppState::PendingDryRun
        )
    }
}

fn format_context_lines(ctx: &dexter_core::context::FileContext) -> String {
    if ctx.files.is_empty() {
        return "No non-hidden files found in current directory.".to_string();
    }

    let mut out = Vec::new();
    out.push(format!("File count: {}", ctx.files.len()));
    for (idx, file) in ctx.files.iter().enumerate() {
        out.push(format!("{:02}. {}", idx + 1, file));
    }
    if let Some(summary) = &ctx.summary {
        out.push(format!("Summary: {}", summary));
    }
    out.join("\n")
}
