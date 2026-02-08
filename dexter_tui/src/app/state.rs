use anyhow::{anyhow, Result};
use dexter_core::{
    CachePolicy, ClarifyOption, Config, ContextScanner, Executor, HistoryEntry, LlmClient,
    PinnedHistoryEntry, RouteOutcome, Router, SafetyGuard,
};
use dexter_plugins::{
    F2Plugin, FFmpegPlugin, JdupesPlugin, LibvipsPlugin, OcrmypdfPlugin, PandocPlugin, Plugin,
    PreviewContent, QpdfPlugin, WhisperCppPlugin, YtDlpPlugin,
};
use ratatui::layout::Rect;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

use crate::app::editor::char_count;
use crate::app::session_log::SessionLogger;
use crate::theme::Theme;

#[derive(Clone, PartialEq, Debug)]
pub enum AppState {
    Input,
    History,
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
    ToggleHistory,
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
    CloseHistory,
    ExecuteHistoryCommand,
    ToggleHistoryPin,
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

#[derive(Clone, Debug)]
pub struct HistoryItem {
    pub entry: HistoryEntry,
    pub pinned_at: Option<String>,
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
    pub history_items: Vec<HistoryItem>,
    pub history_selected: usize,
    pub proposal_rect: Option<Rect>,
    pub settings_button_rect: Option<Rect>,
    pub history_button_rect: Option<Rect>,
    pub history_return_state: Option<AppState>,
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
            history_items: Vec::new(),
            history_selected: 0,
            proposal_rect: None,
            settings_button_rect: None,
            history_button_rect: None,
            history_return_state: None,
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

    pub fn can_open_history(&self) -> bool {
        !is_processing_state(&self.state)
    }

    pub fn history_selected_is_pinned(&self) -> bool {
        self.history_items
            .get(self.history_selected)
            .and_then(|item| item.pinned_at.as_ref())
            .is_some()
    }

    pub async fn open_history_view(&mut self) -> Result<()> {
        if !self.can_open_history() {
            self.push_log("Cannot open history while a task is running.".to_string());
            self.dirty = true;
            return Ok(());
        }

        if self.state != AppState::History {
            self.history_return_state = Some(self.state.clone());
        }
        self.reload_history_items().await?;
        self.state = AppState::History;
        self.focus = FocusArea::FooterButtons;
        self.footer_focus = 0;
        self.output_scroll = 0;
        self.dirty = true;
        Ok(())
    }

    pub fn close_history_view(&mut self) {
        let return_state = self.history_return_state.take().unwrap_or(AppState::Input);
        self.state = return_state;
        self.focus = match self.state {
            AppState::Input | AppState::EditingCommand => FocusArea::Proposal,
            _ => FocusArea::FooterButtons,
        };
        self.footer_focus = 0;
        self.output_scroll = 0;
        self.dirty = true;
    }

    pub async fn toggle_history_pin_for_selected(&mut self) -> Result<()> {
        let Some(selected) = self.history_items.get(self.history_selected).cloned() else {
            self.dirty = true;
            return Ok(());
        };

        if selected.pinned_at.is_some() {
            self.executor.unset_pin(&selected.entry).await?;
            self.push_log(format!(
                "History unpinned [{}] {}",
                selected.entry.plugin, selected.entry.command
            ));
        } else {
            self.executor.set_pin(&selected.entry).await?;
            self.push_log(format!(
                "History pinned [{}] {}",
                selected.entry.plugin, selected.entry.command
            ));
        }

        self.reload_history_items().await?;
        self.dirty = true;
        Ok(())
    }

    pub async fn execute_history_selected_command(&mut self) -> Result<()> {
        let Some(selected) = self.history_items.get(self.history_selected).cloned() else {
            self.push_log("No history command selected.".to_string());
            self.dirty = true;
            return Ok(());
        };

        if !self
            .plugins
            .iter()
            .any(|p| p.name() == selected.entry.plugin)
        {
            self.push_log(format!(
                "History command plugin not available: {}",
                selected.entry.plugin
            ));
            self.dirty = true;
            return Ok(());
        }

        self.selected_plugin = Some(selected.entry.plugin.clone());
        self.generated_command = Some(selected.entry.command.clone());
        self.command_draft = selected.entry.command.clone();
        self.command_cursor = char_count(&self.command_draft);
        self.notice = None;
        self.clarify = None;
        self.dry_run_output = None;
        self.output_scroll = 0;
        self.history_return_state = None;
        self.focus = FocusArea::FooterButtons;
        self.footer_focus = 0;
        self.log_block(
            "HISTORY_EXECUTE_SELECTED",
            &format!(
                "plugin={}\ncommand={}",
                selected.entry.plugin, selected.entry.command
            ),
        );
        self.execute_command().await?;
        self.dirty = true;
        Ok(())
    }

    pub fn history_move_up(&mut self) {
        if self.history_items.is_empty() {
            self.history_selected = 0;
            self.output_scroll = 0;
        } else if self.history_selected > 0 {
            self.history_selected -= 1;
            self.sync_history_scroll_to_selection();
        }
        self.dirty = true;
    }

    pub fn history_move_down(&mut self) {
        if self.history_items.is_empty() {
            self.history_selected = 0;
            self.output_scroll = 0;
        } else if self.history_selected + 1 < self.history_items.len() {
            self.history_selected += 1;
            self.sync_history_scroll_to_selection();
        }
        self.dirty = true;
    }

    pub fn history_page_up(&mut self) {
        if self.history_items.is_empty() {
            self.history_selected = 0;
            self.output_scroll = 0;
            self.dirty = true;
            return;
        }

        const PAGE_STEP: usize = 10;
        self.history_selected = self.history_selected.saturating_sub(PAGE_STEP);
        self.sync_history_scroll_to_selection();
        self.dirty = true;
    }

    pub fn history_page_down(&mut self) {
        if self.history_items.is_empty() {
            self.history_selected = 0;
            self.output_scroll = 0;
            self.dirty = true;
            return;
        }

        const PAGE_STEP: usize = 10;
        let max_idx = self.history_items.len().saturating_sub(1);
        self.history_selected = (self.history_selected + PAGE_STEP).min(max_idx);
        self.sync_history_scroll_to_selection();
        self.dirty = true;
    }

    pub fn history_home(&mut self) {
        self.history_selected = 0;
        self.sync_history_scroll_to_selection();
        self.dirty = true;
    }

    pub fn history_end(&mut self) {
        self.history_selected = self.history_items.len().saturating_sub(1);
        self.sync_history_scroll_to_selection();
        self.dirty = true;
    }

    async fn reload_history_items(&mut self) -> Result<()> {
        let entries = self.executor.load_history_entries().await?;
        let pinned = match self.executor.load_pinned_entries().await {
            Ok(items) => items,
            Err(err) => {
                self.push_log(format!(
                    "History pin data is invalid. Falling back to empty pins: {}",
                    err
                ));
                Vec::new()
            }
        };
        self.history_items = merge_history_items(entries, pinned);
        self.history_selected = clamp_history_selection(self.history_selected, &self.history_items);
        self.sync_history_scroll_to_selection();
        Ok(())
    }

    fn sync_history_scroll_to_selection(&mut self) {
        if self.history_items.is_empty() {
            self.output_scroll = 0;
            return;
        }

        const HISTORY_HEADER_LINES: u16 = 4;
        let selected_line = HISTORY_HEADER_LINES.saturating_add(self.history_selected as u16);
        self.output_scroll = selected_line.saturating_sub(2);
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
        self.routing_result_rx = None;
        self.generation_result_rx = None;
        self.dry_run_result_rx = None;
        self.progress_rx = None;
        self.execution_result_rx = None;
        self.progress = None;
        self.last_progress_log_line = None;
        self.last_progress_log_at = None;
        self.history_return_state = None;
        self.focus = FocusArea::Proposal;
        self.footer_focus = 0;
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
        self.history_return_state = None;
        self.input_cursor = char_count(&self.input);
        self.focus = FocusArea::Proposal;
        self.footer_focus = 0;
        self.dirty = true;
    }

    pub fn is_processing_state(&self) -> bool {
        is_processing_state(&self.state)
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

fn is_processing_state(state: &AppState) -> bool {
    matches!(
        state,
        AppState::Routing
            | AppState::Generating
            | AppState::Executing
            | AppState::DryRunning
            | AppState::PendingRouting
            | AppState::PendingGeneration
            | AppState::PendingDryRun
    )
}

fn clamp_history_selection(current: usize, items: &[HistoryItem]) -> usize {
    if items.is_empty() {
        0
    } else {
        current.min(items.len() - 1)
    }
}

fn merge_history_items(
    history_entries: Vec<HistoryEntry>,
    pinned_entries: Vec<PinnedHistoryEntry>,
) -> Vec<HistoryItem> {
    let mut pin_map: HashMap<(String, String, String), String> = HashMap::new();
    for pin in pinned_entries {
        let key = (
            pin.timestamp.clone(),
            pin.plugin.clone(),
            pin.command.clone(),
        );
        match pin_map.get_mut(&key) {
            Some(existing) => {
                if pin.pinned_at > *existing {
                    *existing = pin.pinned_at;
                }
            }
            None => {
                pin_map.insert(key, pin.pinned_at);
            }
        }
    }

    let mut items: Vec<HistoryItem> = history_entries
        .into_iter()
        .map(|entry| {
            let key = (
                entry.timestamp.clone(),
                entry.plugin.clone(),
                entry.command.clone(),
            );
            HistoryItem {
                pinned_at: pin_map.get(&key).cloned(),
                entry,
            }
        })
        .collect();

    items.sort_by(|a, b| {
        let base_order = match (&a.pinned_at, &b.pinned_at) {
            (Some(ap), Some(bp)) => compare_desc_timestamp(ap, bp)
                .then_with(|| compare_desc_timestamp(&a.entry.timestamp, &b.entry.timestamp)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => compare_desc_timestamp(&a.entry.timestamp, &b.entry.timestamp),
        };

        base_order
            .then_with(|| a.entry.plugin.cmp(&b.entry.plugin))
            .then_with(|| a.entry.command.cmp(&b.entry.command))
    });

    items
}

fn compare_desc_timestamp(a: &str, b: &str) -> Ordering {
    b.cmp(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_sort_pins_first_then_pin_time_desc_then_execution_time_desc() {
        let history_entries = vec![
            HistoryEntry {
                timestamp: "2026-02-08T10:00:00Z".to_string(),
                plugin: "f2".to_string(),
                command: "cmd-a".to_string(),
            },
            HistoryEntry {
                timestamp: "2026-02-08T11:00:00Z".to_string(),
                plugin: "ffmpeg".to_string(),
                command: "cmd-b".to_string(),
            },
            HistoryEntry {
                timestamp: "2026-02-08T12:00:00Z".to_string(),
                plugin: "pandoc".to_string(),
                command: "cmd-c".to_string(),
            },
            HistoryEntry {
                timestamp: "2026-02-08T09:00:00Z".to_string(),
                plugin: "qpdf".to_string(),
                command: "cmd-d".to_string(),
            },
        ];
        let pinned_entries = vec![
            PinnedHistoryEntry {
                timestamp: "2026-02-08T11:00:00Z".to_string(),
                plugin: "ffmpeg".to_string(),
                command: "cmd-b".to_string(),
                pinned_at: "2026-02-08T20:00:00Z".to_string(),
            },
            PinnedHistoryEntry {
                timestamp: "2026-02-08T10:00:00Z".to_string(),
                plugin: "f2".to_string(),
                command: "cmd-a".to_string(),
                pinned_at: "2026-02-08T21:00:00Z".to_string(),
            },
        ];

        let merged = merge_history_items(history_entries, pinned_entries);
        let ordered_commands: Vec<String> = merged
            .iter()
            .map(|item| item.entry.command.clone())
            .collect();
        assert_eq!(ordered_commands, vec!["cmd-a", "cmd-b", "cmd-c", "cmd-d"]);
        assert!(merged[0].pinned_at.is_some());
        assert!(merged[1].pinned_at.is_some());
        assert!(merged[2].pinned_at.is_none());
    }

    #[test]
    fn clamp_history_selection_keeps_index_in_bounds() {
        let empty: Vec<HistoryItem> = Vec::new();
        assert_eq!(clamp_history_selection(5, &empty), 0);

        let items = vec![HistoryItem {
            entry: HistoryEntry {
                timestamp: "2026-02-08T10:00:00Z".to_string(),
                plugin: "f2".to_string(),
                command: "cmd".to_string(),
            },
            pinned_at: None,
        }];
        assert_eq!(clamp_history_selection(9, &items), 0);
    }

    #[test]
    fn processing_states_are_blocked_for_history_open() {
        assert!(is_processing_state(&AppState::PendingRouting));
        assert!(is_processing_state(&AppState::Executing));
        assert!(!is_processing_state(&AppState::Input));
        assert!(!is_processing_state(&AppState::History));
    }
}
