use anyhow::{anyhow, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
        KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use dexter_core::{
    CachePolicy, ClarifyOption, Config, ContextScanner, Executor, LlmClient, ModelRoute,
    ProviderAuth, ProviderConfig, ProviderKind, RouteOutcome, Router, SafetyGuard,
};
use dexter_plugins::{F2Plugin, FFmpegPlugin, PandocPlugin, Plugin, PreviewContent, YtDlpPlugin};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Table, Wrap,
    },
    Frame, Terminal,
};
use std::io::{stdout, Stdout};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

mod theme;
use theme::Theme;

#[derive(Clone, PartialEq, Debug)]
enum AppState {
    Input,
    Routing,
    Generating,
    AwaitingConfirmation,
    EditingCommand,
    Executing,
    Finished(String),
    Error(String),
    Clarifying,
    // New states for non-blocking execution flow
    PendingRouting,
    PendingGeneration,
    PendingDryRun,
    DryRunning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusArea {
    Proposal,
    FooterButtons,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FooterAction {
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
struct FooterButton {
    rect: Rect,
    action: FooterAction,
}

struct App {
    state: AppState,
    input: String,
    input_cursor: usize,
    router: Router,
    executor: Executor,
    plugins: Vec<Arc<dyn Plugin>>,
    selected_plugin: Option<String>,
    generated_command: Option<String>,
    command_draft: String,
    command_cursor: usize,
    logs: Vec<String>,
    tick_count: u64,
    current_context: Option<dexter_core::context::FileContext>,
    dry_run_output: Option<PreviewContent>,
    show_debug: bool,
    config: Config,
    theme: Theme,
    notice: Option<String>,
    clarify: Option<ClarifyPayload>,

    // Focus + interactive footer buttons
    focus: FocusArea,
    footer_buttons: Vec<FooterButton>,
    footer_focus: usize,

    // Output scrolling
    output_scroll: u16,
    output_max_scroll: u16,
    output_text_width: u16,
    output_scrollbar_rect: Option<Rect>,
    proposal_rect: Option<Rect>,
    settings_button_rect: Option<Rect>,

    // Async execution handling
    routing_result_rx: Option<oneshot::Receiver<Result<RouteOutcome>>>,
    generation_result_rx: Option<oneshot::Receiver<Result<String>>>,
    dry_run_result_rx: Option<oneshot::Receiver<Result<PreviewContent>>>,
    progress_rx: Option<mpsc::Receiver<dexter_plugins::Progress>>,
    execution_result_rx: Option<oneshot::Receiver<Result<String>>>,
    progress: Option<dexter_plugins::Progress>,
    generation_cache_policy: CachePolicy,
    pending_open_settings: bool,
}

#[derive(Clone, Debug)]
struct ClarifyPayload {
    question: String,
    options: Vec<ClarifyOption>,
}

impl App {
    fn new(config: Config) -> Self {
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

        Self {
            state: AppState::Input,
            input: String::new(),
            input_cursor: 0,
            router: Router::new(router_client),
            executor: Executor::new(executor_client),
            plugins: vec![
                Arc::new(F2Plugin) as Arc<dyn Plugin>,
                Arc::new(FFmpegPlugin) as Arc<dyn Plugin>,
                Arc::new(PandocPlugin) as Arc<dyn Plugin>,
                Arc::new(YtDlpPlugin) as Arc<dyn Plugin>,
            ],
            selected_plugin: None,
            generated_command: None,
            command_draft: String::new(),
            command_cursor: 0,
            logs: vec!["Dexter initialized. Ready for your command.".to_string()],
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
            generation_cache_policy: CachePolicy::Normal,
            pending_open_settings: false,
        }
    }

    fn apply_config(&mut self, config: Config) {
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
    }

    async fn update_context(&mut self) -> Result<()> {
        let context = ContextScanner::scan_cwd().await?;
        self.current_context = Some(context);
        Ok(())
    }

    async fn execute_command(&mut self) -> Result<()> {
        if let Some(ref cmd) = self.generated_command {
            let plugin_name = self.selected_plugin.clone().unwrap();

            let plugin = self
                .plugins
                .iter()
                .find(|p| p.name() == plugin_name)
                .ok_or_else(|| anyhow!("Plugin not found"))?;

            // Re-check safety/validity for user-edited commands.
            if let Err(e) = SafetyGuard::default().check(cmd) {
                self.state = AppState::Error(format!("Safety check failed: {}", e));
                return Ok(());
            }
            if !plugin.validate_command(cmd) {
                self.state = AppState::Error("Command failed plugin validation logic".to_string());
                return Ok(());
            }

            self.state = AppState::Executing;
            self.output_scroll = 0;
            self.logs
                .push(format!("Executing [{}]: {}", plugin_name, cmd));
            // Record to history
            if let Err(e) = self.executor.record_history(&plugin_name, cmd).await {
                self.logs.push(format!("History log failed: {}", e));
            }

            let plugin = plugin.clone();

            // Adjust command for actual execution if needed (e.g., adding -x for f2)
            let final_cmd = if plugin_name == "f2" && !cmd.contains(" -x") && !cmd.contains(" -X") {
                format!("{} -x", cmd)
            } else {
                cmd.clone()
            };

            let (prog_tx, prog_rx) = mpsc::channel(10);
            let (res_tx, res_rx) = oneshot::channel();

            // Spawn execution task
            tokio::spawn(async move {
                let result = plugin.execute_with_progress(&final_cmd, prog_tx).await;
                let _ = res_tx.send(result);
            });

            self.progress_rx = Some(prog_rx);
            self.execution_result_rx = Some(res_rx);
            self.progress = None; // Reset progress
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = Config::load().await?;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;

    // Try enabling kitty keyboard disambiguation for cleaner modifier handling.
    // We intentionally avoid REPORT_ALL_KEYS_AS_ESCAPE_CODES because it can
    // interfere with IME/CJK text input in some terminals.
    let mut keyboard_enhancement_enabled = false;
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
        if execute!(stdout, PushKeyboardEnhancementFlags(flags)).is_ok() {
            keyboard_enhancement_enabled = true;
        }
    }

    // Mouse support enables clickable "buttons" in the footer.
    let mouse_capture_enabled = execute!(stdout, EnableMouseCapture).is_ok();
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let args: Vec<String> = std::env::args().collect();
    let force_setup = args.contains(&"--setup".to_string());

    // Check for API Keys or forced setup
    if !config.has_keys() || force_setup {
        match run_setup_wizard(&mut terminal, config.clone()).await {
            Ok(new_config) => {
                config = new_config;
            }
            Err(e) => {
                // Restore terminal
                disable_raw_mode()?;
                if mouse_capture_enabled {
                    let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
                }
                if keyboard_enhancement_enabled {
                    let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
                }
                execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                terminal.show_cursor()?;
                return Err(e);
            }
        }
    }

    let mut app = App::new(config);

    // Run app loop
    let res = run_app(&mut terminal, &mut app).await;

    // Restore terminal
    disable_raw_mode()?;
    if mouse_capture_enabled {
        let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
    }
    if keyboard_enhancement_enabled {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err);
    }

    Ok(())
}

async fn run_app(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    // Initial context fetch
    let _ = app.update_context().await;

    loop {
        app.tick_count += 1;

        // Non-blocking automatic state transitions
        match app.state {
            AppState::PendingRouting => {
                app.state = AppState::Routing;
                let _ = app.update_context().await;
                let input = app.input.clone();
                let context =
                    app.current_context
                        .clone()
                        .unwrap_or(dexter_core::context::FileContext {
                            files: Vec::new(),
                            summary: None,
                        });
                let plugins = app.plugins.clone();
                let llm = app.router.llm_client().clone();

                let (tx, rx) = oneshot::channel();
                tokio::spawn(async move {
                    let router = Router::new(llm);
                    let res = router.route(&input, &context, &plugins).await;
                    let _ = tx.send(res);
                });
                app.routing_result_rx = Some(rx);
            }
            AppState::PendingGeneration => {
                app.state = AppState::Generating;
                let plugin_name = match app.selected_plugin.clone() {
                    Some(p) => p,
                    None => {
                        app.state = AppState::Error("No plugin selected".to_string());
                        continue;
                    }
                };
                let plugin = match app.plugins.iter().find(|p| p.name() == plugin_name) {
                    Some(p) => p.clone(),
                    None => {
                        app.state = AppState::Error("Plugin not found".to_string());
                        continue;
                    }
                };
                let input = app.input.clone();
                let context =
                    app.current_context
                        .clone()
                        .unwrap_or(dexter_core::context::FileContext {
                            files: Vec::new(),
                            summary: None,
                        });
                let llm = app.executor.llm_client().clone();
                let cache_policy = app.generation_cache_policy;
                app.generation_cache_policy = CachePolicy::Normal;

                let (tx, rx) = oneshot::channel();
                tokio::spawn(async move {
                    let executor = Executor::new(llm);
                    let res = executor
                        .generate_command_with_policy(
                            &input,
                            &context,
                            plugin.as_ref(),
                            cache_policy,
                        )
                        .await;
                    let _ = tx.send(res);
                });
                app.generation_result_rx = Some(rx);
            }
            AppState::PendingDryRun => {
                app.state = AppState::DryRunning;
                let cmd = match app.generated_command.clone() {
                    Some(c) => c,
                    None => {
                        app.state = AppState::Error("No command available for preview".to_string());
                        continue;
                    }
                };
                let plugin_name = match app.selected_plugin.clone() {
                    Some(p) => p,
                    None => {
                        app.state = AppState::Error("No plugin selected".to_string());
                        continue;
                    }
                };
                let plugin = match app.plugins.iter().find(|p| p.name() == plugin_name) {
                    Some(p) => p.clone(),
                    None => {
                        app.state = AppState::Error("Plugin not found".to_string());
                        continue;
                    }
                };
                let llm = app.executor.llm_client().clone();

                let (tx, rx) = oneshot::channel();
                tokio::spawn(async move {
                    if let Err(e) = SafetyGuard::default().check(&cmd) {
                        let _ = tx.send(Err(anyhow!("Safety check failed: {}", e)));
                        return;
                    }
                    if !plugin.validate_command(&cmd) {
                        let _ = tx.send(Err(anyhow!("Command failed plugin validation logic")));
                        return;
                    }
                    let res = plugin.dry_run(&cmd, Some(&llm)).await;
                    let _ = tx.send(res);
                });
                app.dry_run_result_rx = Some(rx);
            }
            AppState::Routing => {
                if let Some(rx) = &mut app.routing_result_rx {
                    if let Ok(result) = rx.try_recv() {
                        app.routing_result_rx = None;
                        match result {
                            Ok(outcome) => match outcome {
                                RouteOutcome::Selected { plugin, .. } => {
                                    app.selected_plugin = Some(plugin.clone());
                                    app.logs.push(format!("Routed to plugin: {}", plugin));
                                    app.generation_cache_policy = CachePolicy::Normal;
                                    app.state = AppState::PendingGeneration;
                                }
                                RouteOutcome::Unsupported { reason } => {
                                    app.notice = Some(format!(
                                        "This request isn’t supported.\n{}\nTry: convert formats or rename files (rename only, no conversion).",
                                        reason
                                    ));
                                    app.logs
                                        .push("Routing result: unsupported request".to_string());
                                    app.state = AppState::Input;
                                    app.focus = FocusArea::Proposal;
                                    app.footer_focus = 0;
                                }
                                RouteOutcome::Clarify {
                                    question, options, ..
                                } => {
                                    app.clarify = Some(ClarifyPayload { question, options });
                                    app.notice = None;
                                    app.logs.push("Routing requires clarification".to_string());
                                    app.state = AppState::Clarifying;
                                    app.focus = FocusArea::FooterButtons;
                                    app.footer_focus = 0;
                                }
                            },
                            Err(e) => {
                                app.state = AppState::Error(format!("Routing error: {}", e));
                            }
                        }
                    }
                }
            }
            AppState::Generating => {
                if let Some(rx) = &mut app.generation_result_rx {
                    if let Ok(result) = rx.try_recv() {
                        app.generation_result_rx = None;
                        match result {
                            Ok(cmd) => {
                                app.generated_command = Some(cmd.clone());
                                app.command_draft = cmd.clone();
                                app.command_cursor = char_count(&app.command_draft);
                                app.logs.push(format!("Generated command: {}", cmd));
                                app.dry_run_output = None;
                                app.output_scroll = 0;
                                app.state = AppState::PendingDryRun;
                            }
                            Err(e) => {
                                app.state = AppState::Error(format!("Generation error: {}", e));
                            }
                        }
                    }
                }
            }
            AppState::DryRunning => {
                if let Some(rx) = &mut app.dry_run_result_rx {
                    if let Ok(result) = rx.try_recv() {
                        app.dry_run_result_rx = None;
                        match result {
                            Ok(output) => {
                                app.logs
                                    .push("Preview data captured successfully".to_string());
                                app.dry_run_output = Some(output);
                                app.output_scroll = 0;
                                app.state = AppState::AwaitingConfirmation;
                            }
                            Err(e) => {
                                app.logs.push(format!("Preview failed: {}", e));
                                app.state = AppState::Error(format!("Dry run failed: {}", e));
                            }
                        }
                    }
                }
            }
            AppState::Executing => {
                // Check for progress updates
                if let Some(rx) = &mut app.progress_rx {
                    while let Ok(prog) = rx.try_recv() {
                        app.progress = Some(prog);
                    }
                }

                // Check for completion
                let mut finished = false;
                if let Some(rx) = &mut app.execution_result_rx {
                    if let Ok(result) = rx.try_recv() {
                        finished = true;
                        match result {
                            Ok(output) => {
                                app.state = AppState::Finished(output);
                                app.logs
                                    .push("Execution completed successfully.".to_string());
                                let _ = app.update_context().await;
                            }
                            Err(e) => {
                                app.state = AppState::Error(format!("Execution failed: {}", e));
                            }
                        }
                    }
                }

                if finished {
                    app.progress_rx = None;
                    app.execution_result_rx = None;
                    app.progress = None;
                }
            }
            _ => {}
        }

        if app.pending_open_settings {
            app.pending_open_settings = false;
            let busy = matches!(
                app.state,
                AppState::Routing
                    | AppState::Generating
                    | AppState::Executing
                    | AppState::DryRunning
                    | AppState::PendingRouting
                    | AppState::PendingGeneration
                    | AppState::PendingDryRun
            );
            if busy {
                app.logs
                    .push("Cannot open settings while a task is running.".to_string());
            } else {
                match run_settings_panel(terminal, app.config.clone()).await {
                    Ok(new_config) => {
                        app.apply_config(new_config);
                        app.logs.push("Settings updated.".to_string());
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if !msg.to_lowercase().contains("aborted") {
                            app.logs.push(format!("Settings update failed: {}", msg));
                        }
                    }
                }
            }
        }

        // Keep output scrolling bounds in sync with current terminal size/content.
        update_output_scroll_bounds(terminal, app)?;

        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let editing = app.focus == FocusArea::Proposal
                        && matches!(app.state, AppState::Input | AppState::EditingCommand);

                    // Global output scrolling keys (work in most states).
                    if !editing {
                        match key.code {
                            KeyCode::Up => {
                                app.output_scroll = app.output_scroll.saturating_sub(1);
                                continue;
                            }
                            KeyCode::Down => {
                                app.output_scroll = app.output_scroll.saturating_add(1);
                                continue;
                            }
                            KeyCode::PageUp => {
                                app.output_scroll = app.output_scroll.saturating_sub(10);
                                continue;
                            }
                            KeyCode::PageDown => {
                                app.output_scroll = app.output_scroll.saturating_add(10);
                                continue;
                            }
                            KeyCode::Home => {
                                app.output_scroll = 0;
                                continue;
                            }
                            KeyCode::End => {
                                app.output_scroll = app.output_max_scroll;
                                continue;
                            }
                            _ => {}
                        }
                    }

                    // Focus switching / button navigation.
                    match key.code {
                        KeyCode::Tab => {
                            match app.state {
                                AppState::Input | AppState::EditingCommand => {
                                    app.focus = match app.focus {
                                        FocusArea::Proposal => FocusArea::FooterButtons,
                                        FocusArea::FooterButtons => FocusArea::Proposal,
                                    };
                                }
                                _ => {
                                    app.focus = FocusArea::FooterButtons;
                                    if !app.footer_buttons.is_empty() {
                                        app.footer_focus =
                                            (app.footer_focus + 1) % app.footer_buttons.len();
                                    }
                                }
                            }
                            continue;
                        }
                        KeyCode::Left => {
                            if app.focus == FocusArea::FooterButtons
                                && !app.footer_buttons.is_empty()
                            {
                                if app.footer_focus == 0 {
                                    app.footer_focus = app.footer_buttons.len() - 1;
                                } else {
                                    app.footer_focus -= 1;
                                }
                                continue;
                            }
                        }
                        KeyCode::Right => {
                            if app.focus == FocusArea::FooterButtons
                                && !app.footer_buttons.is_empty()
                            {
                                app.footer_focus =
                                    (app.footer_focus + 1) % app.footer_buttons.len();
                                continue;
                            }
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            if app.focus == FocusArea::FooterButtons {
                                if let Some(btn) = app.footer_buttons.get(app.footer_focus) {
                                    let should_quit =
                                        perform_footer_action(app, btn.action).await?;
                                    if should_quit {
                                        return Ok(());
                                    }
                                }
                                continue;
                            }
                        }
                        _ => {}
                    }

                    match app.state {
                        AppState::Input => match key.code {
                            KeyCode::Char('t')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::ToggleDebug).await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            KeyCode::Char('u')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::ClearInput).await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            KeyCode::Char('d')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::Submit).await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            KeyCode::Enter
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::Submit).await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            KeyCode::Enter => {
                                if app.focus == FocusArea::Proposal {
                                    insert_char_at_cursor(
                                        &mut app.input,
                                        &mut app.input_cursor,
                                        '\n',
                                    );
                                }
                            }
                            KeyCode::Left => {
                                if app.focus == FocusArea::Proposal && app.input_cursor > 0 {
                                    app.input_cursor -= 1;
                                }
                            }
                            KeyCode::Right => {
                                if app.focus == FocusArea::Proposal
                                    && app.input_cursor < char_count(&app.input)
                                {
                                    app.input_cursor += 1;
                                }
                            }
                            KeyCode::Up => {
                                if app.focus == FocusArea::Proposal {
                                    move_cursor_up(&app.input, &mut app.input_cursor);
                                }
                            }
                            KeyCode::Down => {
                                if app.focus == FocusArea::Proposal {
                                    move_cursor_down(&app.input, &mut app.input_cursor);
                                }
                            }
                            KeyCode::Home => {
                                if app.focus == FocusArea::Proposal {
                                    move_cursor_line_start(&app.input, &mut app.input_cursor);
                                }
                            }
                            KeyCode::End => {
                                if app.focus == FocusArea::Proposal {
                                    move_cursor_line_end(&app.input, &mut app.input_cursor);
                                }
                            }
                            KeyCode::Char(c) => {
                                if app.focus == FocusArea::Proposal {
                                    insert_char_at_cursor(&mut app.input, &mut app.input_cursor, c);
                                    app.notice = None;
                                    app.clarify = None;
                                }
                            }
                            KeyCode::Backspace => {
                                if app.focus == FocusArea::Proposal {
                                    delete_char_before_cursor(
                                        &mut app.input,
                                        &mut app.input_cursor,
                                    );
                                    app.notice = None;
                                    app.clarify = None;
                                }
                            }
                            KeyCode::Delete => {
                                if app.focus == FocusArea::Proposal {
                                    delete_char_at_cursor(&mut app.input, &mut app.input_cursor);
                                }
                            }
                            KeyCode::Esc => return Ok(()),
                            _ => {}
                        },
                        AppState::AwaitingConfirmation => match key.code {
                            KeyCode::Char('y') | KeyCode::Enter => {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::Execute).await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            KeyCode::Char('m') => {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::EditCommand).await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            KeyCode::Char('e') => {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::EditInput).await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            KeyCode::Char('r') => {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::Regenerate).await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            KeyCode::Char('n') | KeyCode::Esc => {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::BackToInput).await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            _ => {}
                        },
                        AppState::EditingCommand => match key.code {
                            KeyCode::Char('d')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::PreviewEditedCommand)
                                        .await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            KeyCode::Enter
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::PreviewEditedCommand)
                                        .await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            KeyCode::Char('u')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                if app.focus == FocusArea::Proposal {
                                    app.command_draft.clear();
                                    app.command_cursor = 0;
                                }
                            }
                            KeyCode::Left => {
                                if app.focus == FocusArea::Proposal && app.command_cursor > 0 {
                                    app.command_cursor -= 1;
                                }
                            }
                            KeyCode::Right => {
                                if app.focus == FocusArea::Proposal
                                    && app.command_cursor < char_count(&app.command_draft)
                                {
                                    app.command_cursor += 1;
                                }
                            }
                            KeyCode::Up => {
                                if app.focus == FocusArea::Proposal {
                                    move_cursor_up(&app.command_draft, &mut app.command_cursor);
                                }
                            }
                            KeyCode::Down => {
                                if app.focus == FocusArea::Proposal {
                                    move_cursor_down(&app.command_draft, &mut app.command_cursor);
                                }
                            }
                            KeyCode::Home => {
                                if app.focus == FocusArea::Proposal {
                                    move_cursor_line_start(
                                        &app.command_draft,
                                        &mut app.command_cursor,
                                    );
                                }
                            }
                            KeyCode::End => {
                                if app.focus == FocusArea::Proposal {
                                    move_cursor_line_end(
                                        &app.command_draft,
                                        &mut app.command_cursor,
                                    );
                                }
                            }
                            KeyCode::Char(c) => {
                                if app.focus == FocusArea::Proposal {
                                    insert_char_at_cursor(
                                        &mut app.command_draft,
                                        &mut app.command_cursor,
                                        c,
                                    );
                                }
                            }
                            KeyCode::Backspace => {
                                if app.focus == FocusArea::Proposal {
                                    delete_char_before_cursor(
                                        &mut app.command_draft,
                                        &mut app.command_cursor,
                                    );
                                }
                            }
                            KeyCode::Delete => {
                                if app.focus == FocusArea::Proposal {
                                    delete_char_at_cursor(
                                        &mut app.command_draft,
                                        &mut app.command_cursor,
                                    );
                                }
                            }
                            KeyCode::Esc => {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::CancelEditCommand)
                                        .await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            _ => {}
                        },
                        AppState::Finished(_) | AppState::Error(_) => match key.code {
                            KeyCode::Char('r') => {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::Retry).await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ') => {
                                let should_quit =
                                    perform_footer_action(app, FooterAction::ResetToInput).await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            _ => {}
                        },
                        AppState::Routing
                        | AppState::Generating
                        | AppState::Executing
                        | AppState::DryRunning
                        | AppState::PendingRouting
                        | AppState::PendingGeneration
                        | AppState::PendingDryRun
                        | AppState::Clarifying => {
                            // Non-interactive states
                        }
                    }
                }
                Event::Paste(text) => {
                    let editing_proposal = app.focus == FocusArea::Proposal
                        && matches!(app.state, AppState::Input | AppState::EditingCommand);
                    if editing_proposal {
                        match app.state {
                            AppState::Input => {
                                for ch in text.chars() {
                                    insert_char_at_cursor(
                                        &mut app.input,
                                        &mut app.input_cursor,
                                        ch,
                                    );
                                }
                                app.notice = None;
                                app.clarify = None;
                            }
                            AppState::EditingCommand => {
                                for ch in text.chars() {
                                    insert_char_at_cursor(
                                        &mut app.command_draft,
                                        &mut app.command_cursor,
                                        ch,
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        app.output_scroll = app.output_scroll.saturating_sub(3);
                    }
                    MouseEventKind::ScrollDown => {
                        app.output_scroll = app.output_scroll.saturating_add(3);
                    }
                    MouseEventKind::Drag(MouseButton::Left)
                    | MouseEventKind::Down(MouseButton::Left) => {
                        // If the scrollbar is visible, allow clicking/dragging it to jump.
                        if let Some(sb) = app.output_scrollbar_rect {
                            if app.output_max_scroll > 0
                                && mouse.column >= sb.x
                                && mouse.column < sb.x + sb.width
                                && mouse.row >= sb.y
                                && mouse.row < sb.y + sb.height
                            {
                                let track_h = sb.height.max(1);
                                let rel = mouse.row.saturating_sub(sb.y).min(track_h - 1);
                                let denom = track_h.saturating_sub(1).max(1) as u32;
                                let new_scroll =
                                    ((rel as u32) * (app.output_max_scroll as u32) / denom) as u16;
                                app.output_scroll = new_scroll.min(app.output_max_scroll);
                                continue;
                            }
                        }

                        // Otherwise, treat it as a click on the button bar (if any) or focus change.
                        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                            if let Some(rect) = app.settings_button_rect {
                                if point_in_rect(rect, mouse.column, mouse.row) {
                                    let should_quit =
                                        perform_footer_action(app, FooterAction::Settings).await?;
                                    if should_quit {
                                        return Ok(());
                                    }
                                    continue;
                                }
                            }

                            // Click footer buttons if mouse is within the rect.
                            let mut clicked_button = false;
                            for (idx, btn) in app.footer_buttons.iter().enumerate() {
                                if mouse.column >= btn.rect.x
                                    && mouse.column < btn.rect.x + btn.rect.width
                                    && mouse.row >= btn.rect.y
                                    && mouse.row < btn.rect.y + btn.rect.height
                                {
                                    app.focus = FocusArea::FooterButtons;
                                    app.footer_focus = idx;
                                    let should_quit =
                                        perform_footer_action(app, btn.action).await?;
                                    if should_quit {
                                        return Ok(());
                                    }
                                    clicked_button = true;
                                    break;
                                }
                            }

                            // Convenience: click anywhere else to focus the proposal editor.
                            if !clicked_button
                                && matches!(app.state, AppState::Input | AppState::EditingCommand)
                            {
                                app.focus = FocusArea::Proposal;
                                if let Some(area) = app.proposal_rect {
                                    if point_in_rect(area, mouse.column, mouse.row) {
                                        match app.state {
                                            AppState::Input => {
                                                set_cursor_from_click(
                                                    &app.input,
                                                    &mut app.input_cursor,
                                                    area,
                                                    mouse.column,
                                                    mouse.row,
                                                );
                                            }
                                            AppState::EditingCommand => {
                                                set_cursor_from_click(
                                                    &app.command_draft,
                                                    &mut app.command_cursor,
                                                    area,
                                                    mouse.column,
                                                    mouse.row,
                                                );
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

async fn perform_footer_action(app: &mut App, action: FooterAction) -> Result<bool> {
    match action {
        FooterAction::Quit => return Ok(true),
        FooterAction::Settings => {
            app.pending_open_settings = true;
        }
        FooterAction::ToggleDebug => {
            app.show_debug = !app.show_debug;
            app.logs.push(format!(
                "Debug Mode: {}",
                if app.show_debug { "ON" } else { "OFF" }
            ));
        }
        FooterAction::Retry => {
            // Retry from the last user input, restarting the routing -> generation -> preview flow.
            if app.input.trim().is_empty() {
                app.state = AppState::Input;
                app.focus = FocusArea::Proposal;
                app.input_cursor = char_count(&app.input);
            } else {
                app.generated_command = None;
                app.command_draft.clear();
                app.command_cursor = 0;
                app.dry_run_output = None;
                app.output_scroll = 0;
                app.selected_plugin = None;
                app.routing_result_rx = None;
                app.generation_result_rx = None;
                app.dry_run_result_rx = None;
                app.progress_rx = None;
                app.execution_result_rx = None;
                app.progress = None;
                app.generation_cache_policy = CachePolicy::Normal;
                app.focus = FocusArea::FooterButtons;
                app.footer_focus = 0;
                app.state = AppState::PendingRouting;
            }
        }
        FooterAction::ClearInput => {
            app.input.clear();
            app.input_cursor = 0;
            app.notice = None;
            app.clarify = None;
            app.focus = FocusArea::Proposal;
        }
        FooterAction::Submit => {
            if !app.input.trim().is_empty() {
                app.logs
                    .push(format!("Input submitted ({} chars)", app.input.len()));
                app.generated_command = None;
                app.command_draft.clear();
                app.command_cursor = 0;
                app.dry_run_output = None;
                app.output_scroll = 0;
                app.selected_plugin = None;
                app.notice = None;
                app.clarify = None;
                app.generation_cache_policy = CachePolicy::Normal;
                app.focus = FocusArea::FooterButtons;
                app.footer_focus = 0;
                app.state = AppState::PendingRouting;
            }
        }
        FooterAction::Execute => {
            app.focus = FocusArea::FooterButtons;
            app.footer_focus = 0;
            app.execute_command().await?;
        }
        FooterAction::BackToInput => {
            app.state = AppState::Input;
            app.generated_command = None;
            app.command_draft.clear();
            app.command_cursor = 0;
            app.dry_run_output = None;
            app.output_scroll = 0;
            app.selected_plugin = None;
            app.notice = None;
            app.clarify = None;
            app.generation_cache_policy = CachePolicy::Normal;
            app.input_cursor = char_count(&app.input);
            app.focus = FocusArea::Proposal;
            app.footer_focus = 0;
        }
        FooterAction::EditCommand => {
            if let Some(cmd) = &app.generated_command {
                app.command_draft = cmd.clone();
            }
            app.command_cursor = char_count(&app.command_draft);
            app.state = AppState::EditingCommand;
            app.focus = FocusArea::Proposal;
            app.footer_focus = 0;
        }
        FooterAction::EditInput => {
            app.state = AppState::Input;
            app.generated_command = None;
            app.command_draft.clear();
            app.command_cursor = 0;
            app.dry_run_output = None;
            app.output_scroll = 0;
            app.selected_plugin = None;
            app.notice = None;
            app.clarify = None;
            app.generation_cache_policy = CachePolicy::Normal;
            app.input_cursor = char_count(&app.input);
            app.focus = FocusArea::Proposal;
            app.footer_focus = 0;
        }
        FooterAction::Regenerate => {
            app.generated_command = None;
            app.command_draft.clear();
            app.command_cursor = 0;
            app.dry_run_output = None;
            app.output_scroll = 0;
            app.notice = None;
            app.clarify = None;
            app.generation_cache_policy = CachePolicy::Bypass;
            app.focus = FocusArea::FooterButtons;
            app.footer_focus = 0;
            app.state = AppState::PendingGeneration;
        }
        FooterAction::PreviewEditedCommand => {
            let new_cmd = app.command_draft.trim().to_string();
            if !new_cmd.is_empty() {
                app.generated_command = Some(new_cmd.clone());
                app.logs.push(format!("Command edited: {}", new_cmd));
                app.command_cursor = char_count(&app.command_draft);
                app.dry_run_output = None;
                app.output_scroll = 0;
                app.focus = FocusArea::FooterButtons;
                app.footer_focus = 0;
                app.state = AppState::PendingDryRun;
            }
        }
        FooterAction::CancelEditCommand => {
            if let Some(cmd) = &app.generated_command {
                app.command_draft = cmd.clone();
            }
            app.command_cursor = char_count(&app.command_draft);
            app.state = AppState::AwaitingConfirmation;
            app.focus = FocusArea::FooterButtons;
            app.footer_focus = 0;
        }
        FooterAction::ResetToInput => {
            app.state = AppState::Input;
            app.generated_command = None;
            app.command_draft.clear();
            app.command_cursor = 0;
            app.dry_run_output = None;
            app.output_scroll = 0;
            app.generation_cache_policy = CachePolicy::Normal;
            app.selected_plugin = None;
            app.notice = None;
            app.clarify = None;
            app.input_cursor = char_count(&app.input);
            app.focus = FocusArea::Proposal;
            app.footer_focus = 0;
        }
        FooterAction::ClarifySelect(idx) => {
            if let Some(payload) = &app.clarify {
                if let Some(opt) = payload.options.get(idx) {
                    app.input = opt.resolved_intent.clone();
                    app.input_cursor = char_count(&app.input);
                    app.logs.push(format!("Clarify selected: {}", opt.label));
                    app.generated_command = None;
                    app.command_draft.clear();
                    app.command_cursor = 0;
                    app.dry_run_output = None;
                    app.output_scroll = 0;
                    app.selected_plugin = None;
                    app.notice = None;
                    app.clarify = None;
                    app.focus = FocusArea::FooterButtons;
                    app.footer_focus = 0;
                    app.state = AppState::PendingRouting;
                }
            }
        }
    }

    Ok(false)
}

fn ui(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let compact_width = area.width < 100;
    let very_narrow_width = area.width < 80;
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(main_layout_constraints(area))
        .split(area);

    let block_style = app.theme.base_style;
    let border_style = app.theme.border_style;

    // --- SECTION 1: TITLE (HEADER) ---
    let header_text = if very_narrow_width {
        Line::from(vec![
            Span::styled(" DEXTER ", app.theme.header_title_style),
            Span::styled(" // AI CLI ", app.theme.header_subtitle_style),
        ])
    } else {
        Line::from(vec![
            Span::styled(" D E X T E R ", app.theme.header_title_style),
            Span::styled(
                " // AI COMMAND INTERFACE v0.1 ",
                app.theme.header_subtitle_style,
            ),
        ])
    };

    let header = Paragraph::new(header_text).style(block_style).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(if very_narrow_width {
                " STATUS: ONLINE "
            } else {
                " SYSTEM STATUS: ONLINE "
            }),
    );
    f.render_widget(header, main_layout[0]);

    // --- SECTION 2: PROPOSAL (OR INPUT/INTENT) ---
    let (proposal_title, proposal_content) = match app.state {
        AppState::Input => {
            let cursor_visible = app.focus == FocusArea::Proposal && (app.tick_count / 8) % 2 == 0;
            let mut lines = vec![Line::from("")];
            lines.extend(render_multiline_prompt(
                &app.input,
                Span::styled(" > ", app.theme.input_prompt_style),
                Span::styled("   ", app.theme.input_prompt_style),
                app.theme.input_text_style,
                Some(app.theme.input_cursor_style),
                cursor_visible,
                Some(app.input_cursor),
            ));
            (" USER INPUT ", lines)
        }
        AppState::EditingCommand => {
            let mut lines = vec![Line::from("")];
            let cmd_cursor = app
                .theme
                .proposal_cmd_style
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::RAPID_BLINK);
            let cursor_visible = app.focus == FocusArea::Proposal && (app.tick_count / 8) % 2 == 0;
            lines.extend(render_multiline_prompt(
                &app.command_draft,
                Span::styled(" > ", app.theme.header_subtitle_style),
                Span::styled("   ", app.theme.header_subtitle_style),
                app.theme.proposal_cmd_style,
                Some(cmd_cursor),
                cursor_visible,
                Some(app.command_cursor),
            ));
            (" EDIT COMMAND ", lines)
        }
        AppState::Routing
        | AppState::Generating
        | AppState::PendingRouting
        | AppState::PendingGeneration
        | AppState::Clarifying => (" USER INTENT ", {
            let mut lines = vec![Line::from("")];
            lines.extend(render_multiline_prompt(
                &app.input,
                Span::styled(" > ", app.theme.header_subtitle_style),
                Span::styled("   ", app.theme.header_subtitle_style),
                app.theme.header_subtitle_style,
                None,
                false,
                None,
            ));
            lines
        }),
        _ => {
            if let Some(cmd) = &app.generated_command {
                (" PROPOSAL ", {
                    let mut lines = vec![Line::from("")];
                    lines.extend(render_multiline_prompt(
                        cmd,
                        Span::styled(" > ", app.theme.header_subtitle_style),
                        Span::styled("   ", app.theme.header_subtitle_style),
                        app.theme.proposal_cmd_style,
                        None,
                        false,
                        None,
                    ));
                    lines
                })
            } else {
                (" USER INTENT (FAILED) ", {
                    let mut lines = vec![Line::from("")];
                    lines.extend(render_multiline_prompt(
                        &app.input,
                        Span::styled(" > ", app.theme.error_style),
                        Span::styled("   ", app.theme.error_style),
                        app.theme.error_style,
                        None,
                        false,
                        None,
                    ));
                    lines
                })
            }
        }
    };

    let proposal_block = Paragraph::new(proposal_content)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(Span::styled(proposal_title, app.theme.header_title_style)),
        );
    f.render_widget(proposal_block, main_layout[1]);
    app.proposal_rect = Some(Rect {
        x: main_layout[1].x + 1,
        y: main_layout[1].y + 1,
        width: main_layout[1].width.saturating_sub(2),
        height: main_layout[1].height.saturating_sub(2),
    });

    // --- SECTION 3: BUTTON BAR (INTERACTIVE) ---
    render_button_bar(f, app, main_layout[2]);

    // --- SECTION 4: OUTPUT (PREVIEW / LOGS / STATUS) + OPTIONAL SCROLLBAR ---
    let output_title = output_title(app);
    let output_block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(output_title, app.theme.header_title_style));
    f.render_widget(&output_block, main_layout[3]);

    let inner = output_block.inner(main_layout[3]);
    let show_scrollbar = app.output_max_scroll > 0 && inner.width > 1 && inner.height > 0;
    app.output_scrollbar_rect = if show_scrollbar {
        Some(Rect {
            x: inner.x + inner.width - 1,
            y: inner.y,
            width: 1,
            height: inner.height,
        })
    } else {
        None
    };

    let mut text_area = inner;
    if show_scrollbar {
        text_area.width = text_area.width.saturating_sub(1);
    }

    let output_content = build_output_lines(app);
    let output_line_count = output_content.len() as u16;
    let output_para = Paragraph::new(output_content)
        .style(block_style)
        .wrap(Wrap { trim: false })
        .scroll((app.output_scroll, 0));
    f.render_widget(output_para, text_area);

    if show_scrollbar {
        let content_len = output_line_count.max(1) as usize;
        let mut scrollbar_state =
            ScrollbarState::new(content_len).position(app.output_scroll as usize);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(app.theme.border_style)
            .track_style(app.theme.base_style);
        f.render_stateful_widget(scrollbar, inner, &mut scrollbar_state);
    }

    // --- SECTION 5: FOOTER (MODE/MODEL/PROVIDER) ---
    let state_name = format!("{:?}", app.state).to_uppercase();
    let provider_name = get_provider_name(&app.config);
    let footer_block = Block::default()
        .borders(Borders::TOP)
        .border_style(border_style);
    f.render_widget(&footer_block, main_layout[4]);
    let footer_inner = footer_block.inner(main_layout[4]);
    app.settings_button_rect = None;
    let settings_label = if very_narrow_width {
        " [SET] "
    } else {
        " [SETTINGS] "
    };
    let settings_width = settings_label.len() as u16;

    if compact_width && footer_inner.height >= 2 {
        let footer_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(footer_inner);

        let top = if footer_rows.is_empty() {
            footer_inner
        } else {
            footer_rows[0]
        };
        let bottom = if footer_rows.len() > 1 {
            footer_rows[1]
        } else {
            footer_inner
        };

        if top.width > settings_width {
            let top_split = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1), Constraint::Length(settings_width)])
                .split(top);
            let line1 = Line::from(vec![
                Span::styled(" MODE: ", app.theme.footer_text_style),
                Span::styled(&state_name, app.theme.footer_highlight_style),
                Span::styled("  PROVIDER: ", app.theme.footer_text_style),
                Span::styled(&provider_name, app.theme.footer_highlight_style),
            ]);
            let info = Paragraph::new(vec![line1]).style(block_style);
            f.render_widget(info, top_split[0]);

            let settings_button = Paragraph::new(settings_label).style(app.theme.footer_key_style);
            f.render_widget(settings_button, top_split[1]);
            app.settings_button_rect = Some(top_split[1]);
        } else {
            let settings_button = Paragraph::new(settings_label).style(app.theme.footer_key_style);
            f.render_widget(settings_button, top);
            app.settings_button_rect = Some(top);
        }

        let line2 = Line::from(vec![
            Span::styled(" MODEL: ", app.theme.footer_text_style),
            Span::styled(
                &app.config.models.executor_model,
                app.theme.footer_highlight_style,
            ),
        ]);
        let model_info = Paragraph::new(vec![line2]).style(block_style);
        f.render_widget(model_info, bottom);
    } else if footer_inner.width > settings_width {
        let footer_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(settings_width)])
            .split(footer_inner);

        let line1 = Line::from(vec![
            Span::styled(" MODE: ", app.theme.footer_text_style),
            Span::styled(&state_name, app.theme.footer_highlight_style),
            Span::styled("  MODEL: ", app.theme.footer_text_style),
            Span::styled(
                &app.config.models.executor_model,
                app.theme.footer_highlight_style,
            ),
            Span::styled("  PROVIDER: ", app.theme.footer_text_style),
            Span::styled(&provider_name, app.theme.footer_highlight_style),
        ]);
        let info = Paragraph::new(vec![line1]).style(block_style);
        f.render_widget(info, footer_layout[0]);

        let settings_button = Paragraph::new(settings_label).style(app.theme.footer_key_style);
        f.render_widget(settings_button, footer_layout[1]);
        app.settings_button_rect = Some(footer_layout[1]);
    } else {
        let line1 = Line::from(vec![
            Span::styled(" MODE: ", app.theme.footer_text_style),
            Span::styled(&state_name, app.theme.footer_highlight_style),
            Span::styled("  MODEL: ", app.theme.footer_text_style),
            Span::styled(
                &app.config.models.executor_model,
                app.theme.footer_highlight_style,
            ),
            Span::styled("  PROVIDER: ", app.theme.footer_text_style),
            Span::styled(&provider_name, app.theme.footer_highlight_style),
        ]);
        let info = Paragraph::new(vec![line1]).style(block_style);
        f.render_widget(info, footer_inner);
    }
}

fn main_layout_constraints(area: Rect) -> [Constraint; 5] {
    let short_height = area.height < 24;
    let compact_width = area.width < 100;
    let proposal_height = if short_height { 4 } else { 5 };
    let footer_height = if compact_width { 3 } else { 2 };
    [
        Constraint::Length(3),               // 1. Title/Header
        Constraint::Length(proposal_height), // 2. Proposal
        Constraint::Length(1),               // 3. Buttons
        Constraint::Min(1),                  // 4. Output
        Constraint::Length(footer_height),   // 5. Footer
    ]
}

fn compact_button_label(label: &str) -> String {
    match label {
        "SUBMIT" => "GO".to_string(),
        "CLEAR" => "CLR".to_string(),
        "DEBUG:ON" => "DBG:ON".to_string(),
        "DEBUG:OFF" => "DBG:OFF".to_string(),
        "EXECUTE" => "RUN".to_string(),
        "EDIT CMD" => "EDIT".to_string(),
        "EDIT INPUT" => "INPUT".to_string(),
        "PREVIEW" => "PREV".to_string(),
        "REGENERATE" => "REGEN".to_string(),
        other => {
            if other.chars().count() > 8 {
                other.chars().take(8).collect()
            } else {
                other.to_string()
            }
        }
    }
}

fn button_row_width(buttons: &[(FooterAction, String)]) -> u16 {
    let mut width = 0u16;
    for (idx, (_, label)) in buttons.iter().enumerate() {
        let token_width = label.chars().count() as u16 + 4; // " [label] "
        width = width.saturating_add(token_width);
        if idx + 1 < buttons.len() {
            width = width.saturating_add(1); // layout spacing
        }
    }
    width
}

fn render_button_bar(f: &mut Frame, app: &mut App, area: Rect) {
    // Reuse existing button rendering (with mouse hitboxes), but position the bar
    // between the input and output panes per the new UI layout.
    let specs = footer_buttons_for_state(app);
    if specs.is_empty() || area.height == 0 || area.width == 0 {
        app.footer_buttons.clear();
        app.footer_focus = 0;
        if app.focus == FocusArea::FooterButtons {
            app.focus = FocusArea::Proposal;
        }
        return;
    }

    if app.footer_focus >= specs.len() {
        app.footer_focus = 0;
    }

    let compact = area.width < 88;
    let mut candidates: Vec<(FooterAction, String)> = specs
        .into_iter()
        .map(|(action, label)| {
            let rendered = if compact {
                compact_button_label(&label)
            } else {
                label
            };
            (action, rendered)
        })
        .collect();
    while !candidates.is_empty() && button_row_width(&candidates) > area.width {
        candidates.pop();
    }
    if candidates.is_empty() {
        app.footer_buttons.clear();
        app.footer_focus = 0;
        if app.focus == FocusArea::FooterButtons {
            app.focus = FocusArea::Proposal;
        }
        return;
    }
    if app.footer_focus >= candidates.len() {
        app.footer_focus = 0;
    }

    let mut display_texts: Vec<String> = Vec::with_capacity(candidates.len());
    let mut actions: Vec<FooterAction> = Vec::with_capacity(candidates.len());
    let mut constraints: Vec<Constraint> = Vec::with_capacity(candidates.len());
    for (action, label) in candidates {
        let t = format!(" [{}] ", label);
        constraints.push(Constraint::Length(t.len() as u16));
        display_texts.push(t);
        actions.push(action);
    }

    let button_rects = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .spacing(1)
        .split(area);

    app.footer_buttons.clear();
    for (i, rect) in button_rects.iter().enumerate() {
        let mut style = app.theme.footer_key_style;
        if app.focus == FocusArea::FooterButtons && app.footer_focus == i {
            style = style
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD);
        }

        let text = display_texts.get(i).cloned().unwrap_or_default();
        let para = Paragraph::new(text.clone()).style(style);
        f.render_widget(para, *rect);
        let action = *actions.get(i).unwrap_or(&FooterAction::Quit);
        app.footer_buttons.push(FooterButton {
            rect: *rect,
            action,
        });
    }
}

fn get_provider_name(config: &Config) -> String {
    if let Some(primary_route) = config.models.executor_routes.first() {
        return primary_route.provider.display_name().to_string();
    }

    let providers = config.configured_providers();
    if let Some(provider) = providers
        .iter()
        .find(|p| p.models.iter().any(|m| m == &config.models.executor_model))
    {
        provider.display_name()
    } else if let Some(provider) = providers.first() {
        provider.display_name()
    } else {
        "Unknown".to_string()
    }
}

fn footer_buttons_for_state(app: &App) -> Vec<(FooterAction, String)> {
    match &app.state {
        AppState::Input => vec![
            (FooterAction::Submit, "SUBMIT".to_string()),
            (FooterAction::ClearInput, "CLEAR".to_string()),
            (
                FooterAction::ToggleDebug,
                if app.show_debug {
                    "DEBUG:ON".to_string()
                } else {
                    "DEBUG:OFF".to_string()
                },
            ),
            (FooterAction::Quit, "QUIT".to_string()),
        ],
        AppState::AwaitingConfirmation => vec![
            (FooterAction::Execute, "EXECUTE".to_string()),
            (FooterAction::BackToInput, "BACK".to_string()),
            (FooterAction::EditCommand, "EDIT CMD".to_string()),
            (FooterAction::EditInput, "EDIT INPUT".to_string()),
            (FooterAction::Regenerate, "REGEN".to_string()),
            (FooterAction::Quit, "QUIT".to_string()),
        ],
        AppState::EditingCommand => vec![
            (FooterAction::PreviewEditedCommand, "PREVIEW".to_string()),
            (FooterAction::CancelEditCommand, "BACK".to_string()),
            (FooterAction::Quit, "QUIT".to_string()),
        ],
        AppState::Finished(_) | AppState::Error(_) => vec![
            (FooterAction::Retry, "RETRY".to_string()),
            (FooterAction::ResetToInput, "BACK".to_string()),
            (FooterAction::Quit, "QUIT".to_string()),
        ],
        AppState::Clarifying => {
            let mut buttons = Vec::new();
            if let Some(payload) = &app.clarify {
                for (i, opt) in payload.options.iter().enumerate() {
                    buttons.push((FooterAction::ClarifySelect(i), opt.label.clone()));
                }
            }
            buttons.push((FooterAction::BackToInput, "BACK".to_string()));
            buttons.push((FooterAction::Quit, "QUIT".to_string()));
            buttons
        }
        _ => vec![(FooterAction::Quit, "QUIT".to_string())],
    }
}

// --- HELPER RENDERERS ---

fn render_multiline_prompt<'a>(
    text: &'a str,
    first_prefix: Span<'a>,
    continuation_prefix: Span<'a>,
    text_style: Style,
    cursor_style: Option<Style>,
    cursor_visible: bool,
    cursor_pos: Option<usize>,
) -> Vec<Line<'a>> {
    let mut out = Vec::new();
    let mut parts: Vec<&str> = text.split('\n').collect();
    if parts.is_empty() {
        parts.push("");
    }
    let mut remaining = cursor_pos.unwrap_or(0);
    let mut cursor_pending = cursor_pos.is_some();

    for (idx, line) in parts.iter().enumerate() {
        let prefix = if idx == 0 {
            first_prefix.clone()
        } else {
            continuation_prefix.clone()
        };

        let line_len = line.chars().count();
        if cursor_pending && remaining <= line_len {
            let (before, current, after) = split_line_at_char(line, remaining);
            let mut spans = vec![prefix];

            if !before.is_empty() {
                spans.push(Span::styled(before, text_style));
            }

            match current {
                Some(ch) => {
                    let ch_str = ch.to_string();
                    if cursor_visible {
                        if let Some(cursor_style) = cursor_style {
                            spans.push(Span::styled(ch_str, cursor_style));
                        } else {
                            spans.push(Span::styled(ch_str, text_style));
                        }
                    } else {
                        spans.push(Span::styled(ch_str, text_style));
                    }
                }
                None => {
                    if cursor_visible {
                        if let Some(cursor_style) = cursor_style {
                            spans.push(Span::styled(" ", cursor_style));
                        }
                    }
                }
            }

            if !after.is_empty() {
                spans.push(Span::styled(after, text_style));
            }
            out.push(Line::from(spans));
            cursor_pending = false;
        } else {
            out.push(Line::from(vec![prefix, Span::styled(*line, text_style)]));
            if cursor_pending {
                remaining = remaining.saturating_sub(line_len + 1);
            }
        }
    }
    out
}

fn split_line_at_char(line: &str, idx: usize) -> (String, Option<char>, String) {
    let mut before = String::new();
    let mut current = None;
    let mut after = String::new();

    for (i, ch) in line.chars().enumerate() {
        if i < idx {
            before.push(ch);
        } else if i == idx {
            current = Some(ch);
        } else {
            after.push(ch);
        }
    }

    (before, current, after)
}

fn char_count(text: &str) -> usize {
    text.chars().count()
}

fn byte_index(text: &str, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    if let Some((idx, _)) = text.char_indices().nth(char_idx) {
        return idx;
    }
    text.len()
}

fn insert_char_at_cursor(text: &mut String, cursor: &mut usize, ch: char) {
    let idx = byte_index(text, *cursor);
    text.insert(idx, ch);
    *cursor += 1;
}

fn delete_char_before_cursor(text: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let start = byte_index(text, *cursor - 1);
    let end = byte_index(text, *cursor);
    if start < end {
        text.replace_range(start..end, "");
        *cursor -= 1;
    }
}

fn delete_char_at_cursor(text: &mut String, cursor: &mut usize) {
    let len = char_count(text);
    if *cursor >= len {
        return;
    }
    let start = byte_index(text, *cursor);
    let end = byte_index(text, *cursor + 1);
    if start < end {
        text.replace_range(start..end, "");
    }
}

fn line_lengths(text: &str) -> Vec<usize> {
    let mut lines: Vec<usize> = text.split('\n').map(|line| line.chars().count()).collect();
    if lines.is_empty() {
        lines.push(0);
    }
    lines
}

fn cursor_line_col(line_lens: &[usize], cursor: usize) -> (usize, usize) {
    let mut remaining = cursor;
    for (i, len) in line_lens.iter().enumerate() {
        if remaining <= *len {
            return (i, remaining);
        }
        remaining = remaining.saturating_sub(len + 1);
    }
    let last = line_lens.len().saturating_sub(1);
    let last_len = *line_lens.get(last).unwrap_or(&0);
    (last, last_len)
}

fn cursor_from_line_col(line_lens: &[usize], line_idx: usize, col: usize) -> usize {
    let mut idx = 0usize;
    for i in 0..line_idx {
        idx = idx.saturating_add(line_lens.get(i).copied().unwrap_or(0) + 1);
    }
    let line_len = line_lens.get(line_idx).copied().unwrap_or(0);
    idx.saturating_add(col.min(line_len))
}

fn move_cursor_up(text: &str, cursor: &mut usize) {
    let line_lens = line_lengths(text);
    let (line, col) = cursor_line_col(&line_lens, *cursor);
    if line == 0 {
        *cursor = cursor_from_line_col(&line_lens, 0, col);
        return;
    }
    *cursor = cursor_from_line_col(&line_lens, line - 1, col);
}

fn move_cursor_down(text: &str, cursor: &mut usize) {
    let line_lens = line_lengths(text);
    let (line, col) = cursor_line_col(&line_lens, *cursor);
    if line + 1 >= line_lens.len() {
        *cursor = cursor_from_line_col(&line_lens, line, col);
        return;
    }
    *cursor = cursor_from_line_col(&line_lens, line + 1, col);
}

fn move_cursor_line_start(text: &str, cursor: &mut usize) {
    let line_lens = line_lengths(text);
    let (line, _) = cursor_line_col(&line_lens, *cursor);
    *cursor = cursor_from_line_col(&line_lens, line, 0);
}

fn move_cursor_line_end(text: &str, cursor: &mut usize) {
    let line_lens = line_lengths(text);
    let (line, _) = cursor_line_col(&line_lens, *cursor);
    let line_len = line_lens.get(line).copied().unwrap_or(0);
    *cursor = cursor_from_line_col(&line_lens, line, line_len);
}

fn point_in_rect(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn set_cursor_from_click(text: &str, cursor: &mut usize, area: Rect, col: u16, row: u16) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let row_in_area = row.saturating_sub(area.y) as usize;
    let col_in_area = col.saturating_sub(area.x) as usize;

    // Account for the leading blank line in the proposal pane.
    let text_row = row_in_area.saturating_sub(1);
    let lines: Vec<&str> = text.split('\n').collect();
    let line_idx = text_row.min(lines.len().saturating_sub(1));
    let line = lines.get(line_idx).copied().unwrap_or("");

    let prefix_len = 3usize; // " > " or "   "
    let col_in_text = col_in_area.saturating_sub(prefix_len);
    let target_col = col_in_text.min(line.chars().count());

    let line_lens = line_lengths(text);
    *cursor = cursor_from_line_col(&line_lens, line_idx, target_col);
}

fn build_output_view<'a>(app: &'a App) -> (&'static str, Vec<Line<'a>>) {
    (output_title(app), build_output_lines(app))
}

fn output_title(app: &App) -> &'static str {
    if app.show_debug {
        return " DEBUG_SYSTEM_INTERNAL ";
    }

    match &app.state {
        AppState::Input => " SYSTEM STATUS & LOGS ",
        AppState::Routing
        | AppState::Generating
        | AppState::Executing
        | AppState::DryRunning
        | AppState::PendingRouting
        | AppState::PendingGeneration
        | AppState::PendingDryRun => " PROCESSING ",
        AppState::Clarifying => " CLARIFICATION ",
        AppState::AwaitingConfirmation => " PREVIEW / CONFIRMATION ",
        AppState::EditingCommand => " EDIT COMMAND ",
        AppState::Finished(_) => " EXECUTION RESULTS ",
        AppState::Error(_) => " SYSTEM FAILURE ",
    }
}

fn build_output_lines<'a>(app: &'a App) -> Vec<Line<'a>> {
    if app.show_debug {
        return render_debug(app, &app.theme);
    }

    match &app.state {
        AppState::Input => render_input_view(app, &app.theme),
        AppState::Routing
        | AppState::Generating
        | AppState::Executing
        | AppState::DryRunning
        | AppState::PendingRouting
        | AppState::PendingGeneration
        | AppState::PendingDryRun => render_processing_view(app, &app.theme),
        AppState::Clarifying => render_clarify_view(app, &app.theme),
        AppState::AwaitingConfirmation => render_preview_view(app, &app.theme),
        AppState::EditingCommand => render_edit_command_view(app, &app.theme),
        AppState::Finished(out) => {
            render_finished_view(out, app.selected_plugin.as_deref(), &app.theme)
        }
        AppState::Error(e) => render_error_view(e, &app.theme),
    }
}

fn render_debug<'a>(app: &'a App, theme: &Theme) -> Vec<Line<'a>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(" DEBUG_MODE: ", theme.header_subtitle_style),
            Span::styled("ACTIVE", theme.success_style.add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled(" CWD: ", theme.header_subtitle_style),
            Span::styled(
                format!("{}", std::env::current_dir().unwrap_or_default().display()),
                theme.header_title_style,
            ),
        ]),
        Line::from(""),
    ];

    if let Some(ctx) = &app.current_context {
        for (i, f) in ctx.files.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:2}. ", i + 1), theme.header_subtitle_style),
                Span::styled(f, theme.header_title_style),
            ]));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "  (No Context Scanned)",
            theme.error_style,
        )));
    }
    lines
}

fn render_input_view<'a>(app: &'a App, theme: &Theme) -> Vec<Line<'a>> {
    let mut text = vec![];
    if let Some(notice) = &app.notice {
        text.push(Line::from(""));
        for line in notice.lines() {
            text.push(Line::from(Span::styled(line, theme.error_style)));
        }
        text.push(Line::from(""));
    }
    text.extend(vec![
        Line::from(""),
        Line::from(Span::styled(
            "Ready for instructions. Type your command above.",
            theme.header_subtitle_style,
        )),
        Line::from(""),
    ]);

    if let Some(ctx) = &app.current_context {
        let files_str = if ctx.files.is_empty() {
            " (Empty)".to_string()
        } else if ctx.files.len() > 10 {
            format!("{} files detected (Scan complete)", ctx.files.len())
        } else {
            ctx.files.join(", ")
        };

        text.push(Line::from(vec![
            Span::styled(" CWD_CONTEXT: ", theme.header_subtitle_style),
            Span::styled(
                format!("{}", std::env::current_dir().unwrap_or_default().display()),
                theme.header_subtitle_style,
            ),
        ]));
        text.push(Line::from(vec![
            Span::styled(" FILES: ", theme.header_subtitle_style),
            Span::styled(files_str, theme.header_subtitle_style),
        ]));
        text.push(Line::from(""));
    }

    if !app.logs.is_empty() {
        text.push(Line::from(Span::styled(
            "--- SYSTEM LOGS ---",
            theme.header_subtitle_style,
        )));
        for log in app.logs.iter().rev().take(5) {
            text.push(Line::from(Span::styled(
                format!(":: {}", log),
                theme.header_subtitle_style,
            )));
        }
    }
    text
}

fn render_clarify_view<'a>(app: &'a App, theme: &Theme) -> Vec<Line<'a>> {
    let mut lines = vec![Line::from("")];
    if let Some(payload) = &app.clarify {
        lines.push(Line::from(Span::styled(
            "This could mean more than one action.",
            theme.header_subtitle_style,
        )));
        lines.push(Line::from(Span::styled(
            &payload.question,
            theme.header_subtitle_style,
        )));
        lines.push(Line::from(Span::styled(
            "Choose one option below to continue.",
            theme.header_subtitle_style,
        )));
        lines.push(Line::from(""));

        for opt in payload.options.iter() {
            lines.push(Line::from(vec![
                Span::styled("OPTION: ", theme.header_subtitle_style),
                Span::styled(&opt.label, theme.header_title_style),
            ]));
            lines.push(Line::from(Span::styled(
                &opt.detail,
                theme.header_subtitle_style,
            )));
            lines.push(Line::from(""));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "Clarification required, but no options are available.",
            theme.error_style,
        )));
    }
    lines
}

fn render_processing_view<'a>(app: &'a App, theme: &Theme) -> Vec<Line<'a>> {
    let spinner = ["|", "/", "-", "\\"];
    let idx = (app.tick_count as usize / 2) % spinner.len();
    let char = spinner[idx];

    let action = match app.state {
        AppState::Routing | AppState::PendingRouting => "CALCULATING ROUTE",
        AppState::Generating | AppState::PendingGeneration => "SYNTHESIZING COMMAND",
        AppState::Executing => "APPLYING CHANGES",
        AppState::DryRunning | AppState::PendingDryRun => "FETCHING PREVIEW",
        _ => "PROCESSING",
    };

    let belt_w = app.output_text_width.max(10) as usize;
    let belt = token_conveyor_belt_line(app.tick_count, belt_w);

    let quip = if let Some(prog) = &app.progress {
        let msg = prog.message.trim();
        if !msg.is_empty() {
            msg.to_string()
        } else {
            processing_quip(app.tick_count, belt.bounce_count)
        }
    } else if belt.dropped {
        // Guaranteed to line up: we compute drop + message in the same frame.
        match (app.tick_count / 10) % 3 {
            0 => "One token escaped.".to_string(),
            1 => "A token fell off the belt. We'll pretend it's fine.".to_string(),
            _ => "Token down. Morale up.".to_string(),
        }
    } else {
        processing_quip(app.tick_count, belt.bounce_count)
    };

    // Reserve a dedicated line under the belt for the dropped token so the quip never shifts.
    let drop_line = if let Some(col) = belt.dropped_col {
        // Clamp to avoid wrapping on very narrow terminals.
        let col = col.min(belt_w.saturating_sub(1));
        format!("{:width$}o", "", width = col)
    } else {
        String::new()
    };

    vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(format!(" {} ", char), theme.processing_spinner_style),
            Span::styled(format!("{}...", action), theme.processing_text_style),
        ]),
        Line::from(Span::styled(belt.line, theme.header_subtitle_style)),
        Line::from(Span::styled(drop_line, theme.header_subtitle_style)),
        Line::from(Span::styled(quip, theme.header_subtitle_style)),
        Line::from(""),
    ]
}

struct BeltRender {
    line: String,
    dropped_col: Option<usize>,
    dropped: bool,
    bounce_count: u64,
}

fn token_conveyor_belt_line(tick: u64, width: usize) -> BeltRender {
    // ASCII-only "token conveyor belt". Designed to fill the whole line so it doesn't wrap.
    // Example: "(|) [o---o----o---o---] (/)"
    let w = width.max(10);

    // If terminal is too narrow, keep it compact and skip the rollers.
    if w < 16 {
        let inner = w.saturating_sub(2).max(1);
        let mut cells: Vec<char> = vec!['-'; inner];
        let t = (tick / 2) as usize;
        let n_tokens = 3usize.min(inner.max(1));
        for i in 0..n_tokens {
            let pos = (t + i * 3) % inner;
            cells[pos] = 'o';
        }
        let mut s = String::with_capacity(w);
        s.push('[');
        for c in cells {
            s.push(c);
        }
        s.push(']');
        return BeltRender {
            line: s,
            dropped_col: None,
            dropped: false,
            bounce_count: 0,
        };
    }

    let roller_frames = ['|', '/', '-', '\\'];
    let left = roller_frames[((tick / 2) as usize) % roller_frames.len()];
    let right = roller_frames[(((tick / 2) as usize) + 2) % roller_frames.len()];
    let left_roller = format!("({})", left);
    let right_roller = format!("({})", right);

    let overhead = left_roller.len() + 1 + 1 + right_roller.len(); // "L <belt> R"
    let belt_len = w.saturating_sub(overhead).max(4);
    let inner = belt_len.saturating_sub(2).max(1);

    let t = (tick / 2) as usize;
    let mut cells: Vec<char> = vec!['-'; inner];
    let n_tokens = 5usize.min(inner.max(1));
    for i in 0..n_tokens {
        let pos = (t + i * 4) % inner;
        cells[pos] = 'o';
    }

    // Occasionally, a token "falls off" the belt for a short moment.
    //
    // Keep it visible long enough to notice: a few hundred ms, not a few frames.
    let drop_active = (tick % 173) < 12 && inner > 1;
    let mut dropped_col: Option<usize> = None;
    if drop_active {
        // Drop an actual token we know exists so the belt visibly changes.
        // Tokens are at (t + i*4) % inner; pick i=2.
        let drop_pos = (t + 8) % inner;
        cells[drop_pos] = '-';
        // Column where the dropped token should appear (roughly aligned under the belt).
        // Format: "<L> <[belt] > <R>"
        let belt_start = left_roller.len() + 1;
        dropped_col = Some(belt_start + 1 + drop_pos);
    }

    // "Cart" token that bounces end-to-end; use it to set a rhythmic quip cadence.
    let period = inner.saturating_sub(1).max(1);
    let bounce_count = (t / period) as u64;
    let phase = t % (period * 2);
    let cart_pos = if phase <= period {
        phase
    } else {
        (period * 2) - phase
    };
    if cart_pos < inner {
        cells[cart_pos] = 'O';
    }

    let mut belt = String::with_capacity(belt_len);
    belt.push('[');
    for c in cells {
        belt.push(c);
    }
    belt.push(']');

    let line = format!("{} {} {}", left_roller, belt, right_roller);
    BeltRender {
        line,
        dropped_col,
        dropped: drop_active,
        bounce_count,
    }
}

fn processing_quip(tick: u64, bounce_count: u64) -> String {
    // Prefer cadence over chatter: rotate copy only when the cart hits an end.
    // That makes it feel intentional rather than "every N ms".
    let idx = (bounce_count as usize) % 6;
    match idx {
        0 => "Applying artisanal, free-range heuristics.".to_string(),
        1 => "Consulting the manual (it is blank).".to_string(),
        2 => "Counting tokens by hand. Again.".to_string(),
        3 => "Gently discouraging hallucinations.".to_string(),
        4 => "Assembling a command with 100% confidence (±100%).".to_string(),
        _ => {
            // Slow blink on the ellipsis for a tiny bit of extra rhythm.
            let dots = match (tick / 10) % 4 {
                0 => ".",
                1 => "..",
                2 => "...",
                _ => "....",
            };
            format!("Staying calm{} (mostly).", dots)
        }
    }
}

fn render_preview_view<'a>(app: &'a App, theme: &Theme) -> Vec<Line<'a>> {
    let mut lines = vec![Line::from("")];
    if let Some(preview) = &app.dry_run_output {
        lines.extend(render_preview_content(preview, theme));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("CONFIRM EXECUTION? [", theme.input_prompt_style),
        Span::styled("Y", theme.input_prompt_style),
        Span::styled("/", theme.input_prompt_style),
        Span::styled("N", theme.input_prompt_style),
        Span::styled("]", theme.input_prompt_style),
    ]));

    lines.push(Line::from(vec![
        Span::styled("KEYS: ", theme.header_subtitle_style),
        Span::styled(
            "M=Edit Cmd  E=Edit Input  R=Regenerate  Up/Down=Scroll",
            theme.header_subtitle_style,
        ),
    ]));

    lines
}

fn render_edit_command_view<'a>(app: &'a App, theme: &Theme) -> Vec<Line<'a>> {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Edit the command in the PROPOSAL pane.",
            theme.header_subtitle_style,
        )),
        Line::from(Span::styled(
            "Ctrl+Enter/Ctrl+D: Re-run preview | Esc: Back",
            theme.header_subtitle_style,
        )),
        Line::from(""),
    ];

    if let Some(preview) = &app.dry_run_output {
        lines.extend(render_preview_content(preview, theme));
    } else {
        lines.push(Line::from(Span::styled(
            "(No preview available yet.)",
            theme.header_subtitle_style,
        )));
    }

    lines
}

fn render_preview_content<'a>(preview: &'a PreviewContent, theme: &Theme) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    match preview {
        PreviewContent::Text(t) => {
            for line in t.lines() {
                lines.push(Line::from(Span::styled(line, theme.processing_text_style)));
            }
        }
        PreviewContent::DiffList(diffs) => {
            if diffs.is_empty() {
                lines.push(Line::from(Span::styled(
                    "No changes detected.",
                    theme.header_subtitle_style,
                )));
            } else {
                for (i, diff) in diffs.iter().enumerate() {
                    let mut header_spans = vec![Span::styled(
                        format!("FILE [{:02}]: ", i + 1),
                        theme.diff_header_style,
                    )];

                    if let Some(status) = &diff.status {
                        let status_style = theme.proposal_cmd_style;
                        header_spans.push(Span::styled(
                            format!(" [{}] ", status.to_uppercase()),
                            status_style,
                        ));
                    }

                    lines.push(Line::from(header_spans));
                    lines.push(Line::from(vec![
                        Span::styled("  OLD: ", theme.diff_removed_style),
                        Span::styled(&diff.original, theme.diff_removed_style),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("  NEW: ", theme.diff_added_style),
                        Span::styled(&diff.new, theme.diff_added_style),
                    ]));
                    lines.push(Line::from(""));
                }
            }
        }
    }

    lines
}

fn render_finished_view<'a>(
    output: &'a str,
    plugin_name: Option<&'a str>,
    theme: &Theme,
) -> Vec<Line<'a>> {
    let mut lines = vec![
        Line::from(Span::styled(
            "EXECUTION COMPLETE.",
            theme.success_style.add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Target System Output:",
            theme.header_subtitle_style,
        )),
    ];

    if plugin_name == Some("f2") {
        for line in output.lines() {
            if line.contains(" -> ") {
                let parts: Vec<&str> = line.split(" -> ").collect();
                if parts.len() == 2 {
                    lines.push(Line::from(vec![
                        Span::styled(parts[0], theme.processing_text_style),
                        Span::styled(" -> ", theme.header_subtitle_style),
                        Span::styled(parts[1], theme.success_style),
                    ]));
                } else {
                    lines.push(Line::from(Span::styled(line, theme.header_subtitle_style)));
                }
            } else {
                lines.push(Line::from(Span::styled(line, theme.header_subtitle_style)));
            }
        }
    } else {
        for line in output.lines() {
            lines.push(Line::from(Span::styled(line, theme.header_subtitle_style)));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "[PRESS ENTER TO RESET]",
        theme.header_subtitle_style,
    )));
    lines
}

fn render_error_view<'a>(err: &'a str, theme: &Theme) -> Vec<Line<'a>> {
    let mut lines = vec![
        Line::from(Span::styled(
            "!!! SYSTEM FAILURE !!!",
            theme.error_style.add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for line in err.lines() {
        lines.push(Line::from(Span::styled(line, theme.error_style)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "[PRESS ENTER TO ACKNOWLEDGE]",
        theme.header_subtitle_style,
    )));
    lines
}

fn update_output_scroll_bounds(
    terminal: &Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    let size = terminal.size()?;
    let area = Rect {
        x: 0,
        y: 0,
        width: size.width,
        height: size.height,
    };

    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(main_layout_constraints(area))
        .split(area);

    let output_viewport_height = main_layout[3].height.saturating_sub(2); // subtract borders
    let output_inner_width = main_layout[3].width.saturating_sub(2); // subtract borders
                                                                     // Leave 1 column slack so the animation doesn't wrap when the scrollbar appears.
    app.output_text_width = output_inner_width.saturating_sub(1);
    let (_, lines) = build_output_view(app);
    let line_count = lines.len() as u16;

    let max_scroll = line_count.saturating_sub(output_viewport_height);
    app.output_max_scroll = max_scroll;
    app.output_scroll = app.output_scroll.min(max_scroll);
    Ok(())
}

// --- Setup Wizard ---

#[derive(Debug, Clone, PartialEq)]
enum SetupState {
    Welcome,
    ProviderSelection,
    ProviderConfig,
    FetchingProviderModels,
    ProviderModelSelection,
    ModelOrderSelection,
    ThemeSelection,
    Confirm,
    Saving,
    Error(String),
}

#[derive(Debug, Clone)]
struct SetupProviderEntry {
    kind: ProviderKind,
    enabled: bool,
    api_key: String,
    base_url: String,
    auth: ProviderAuth,
    available_models: Vec<String>,
    active_models: Vec<String>,
    runtime_ready: Option<bool>, // For local providers like Ollama
}

impl SetupProviderEntry {
    fn name(&self) -> &'static str {
        self.kind.display_name()
    }

    fn requires_api_key(&self) -> bool {
        !matches!(self.auth, ProviderAuth::None)
    }

    fn has_key(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    fn to_provider_config(&self) -> ProviderConfig {
        let api_key = if self.api_key.trim().is_empty() {
            None
        } else {
            Some(self.api_key.trim().to_string())
        };
        ProviderConfig {
            kind: self.kind,
            name: Some(self.name().to_string()),
            api_key,
            base_url: self.base_url.trim().to_string(),
            auth: self.auth,
            enabled: self.enabled,
            models: dedup_models(self.active_models.clone()),
        }
        .normalized()
    }

    fn to_provider_config_for_fetch(&self) -> ProviderConfig {
        let mut cfg = self.to_provider_config();
        cfg.enabled = true;
        cfg
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelRouteDraft {
    provider_idx: usize,
    model: String,
}

impl ModelRouteDraft {
    fn key(&self) -> String {
        format!("{}::{}", self.provider_idx, self.model)
    }
}

struct SetupApp {
    state: SetupState,
    providers: Vec<SetupProviderEntry>,
    selected_provider_idx: usize,
    config_provider_idx: Option<usize>,
    guided_provider_order: Vec<usize>,
    guided_provider_pos: usize,
    provider_model_cursor: usize,
    model_order: Vec<ModelRouteDraft>,
    model_order_cursor: usize,
    available_themes: Vec<(&'static str, &'static str)>,
    selected_theme_idx: usize,
    config: Config,
    theme: Theme,
}

impl SetupApp {
    fn new(config: Config, show_welcome: bool) -> Self {
        let mut providers = build_provider_entries(&config);
        if !providers.iter().any(|p| p.enabled) {
            if let Some(gemini) = providers
                .iter_mut()
                .find(|p| matches!(p.kind, ProviderKind::Gemini))
            {
                gemini.enabled = true;
                if gemini.active_models.is_empty() {
                    gemini.active_models = vec!["gemini-2.5-flash-lite".to_string()];
                }
            }
        }

        let mut app = Self {
            state: if show_welcome {
                SetupState::Welcome
            } else {
                SetupState::ProviderSelection
            },
            providers,
            selected_provider_idx: 0,
            config_provider_idx: None,
            guided_provider_order: Vec::new(),
            guided_provider_pos: 0,
            provider_model_cursor: 0,
            model_order: Vec::new(),
            model_order_cursor: 0,
            available_themes: vec![
                ("auto", "🔄 Auto (Follow system appearance)"),
                ("dark", "🌑 Dark (Blue on charcoal)"),
                ("retro", "🌙 Retro (Classic amber CRT aesthetic)"),
                ("light", "☀️ Light (Clean blue/white for light terminals)"),
            ],
            selected_theme_idx: 0,
            theme: Theme::from_config(&config.theme),
            config,
        };

        if let Some(idx) = app
            .available_themes
            .iter()
            .position(|(id, _)| *id == app.config.theme)
        {
            app.selected_theme_idx = idx;
        }

        let mut seeded = false;
        if !app.config.models.executor_routes.is_empty() {
            for route in &app.config.models.executor_routes {
                if let Some((provider_idx, _)) = app
                    .providers
                    .iter()
                    .enumerate()
                    .find(|(_, p)| p.kind == route.provider)
                {
                    if route.model.trim().is_empty() {
                        continue;
                    }
                    app.model_order.push(ModelRouteDraft {
                        provider_idx,
                        model: route.model.clone(),
                    });
                    seeded = true;
                }
            }
        }

        if !seeded {
            app.update_model_order_from_active();
        }

        app
    }

    async fn refresh_runtime_statuses(&mut self) {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_millis(900))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };

        for provider in &mut self.providers {
            if provider.kind != ProviderKind::Ollama {
                continue;
            }
            let base = provider.base_url.trim().trim_end_matches('/');
            let root = base.strip_suffix("/v1").unwrap_or(base);
            let probe_url = format!("{}/api/tags", root);
            let ready = client
                .get(&probe_url)
                .send()
                .await
                .map(|res| res.status().is_success())
                .unwrap_or(false);
            provider.runtime_ready = Some(ready);
        }
    }

    fn provider_selection_len(&self) -> usize {
        self.providers.len()
    }

    fn current_guided_provider_idx(&self) -> Option<usize> {
        self.guided_provider_order
            .get(self.guided_provider_pos)
            .copied()
    }

    fn start_guided_flow(&mut self) -> Result<()> {
        let mut order: Vec<usize> = self
            .providers
            .iter()
            .enumerate()
            .filter_map(|(idx, p)| if p.enabled { Some(idx) } else { None })
            .collect();

        if order.is_empty() {
            return Err(anyhow!("Please enable at least one provider."));
        }

        if let Some(pos) = order
            .iter()
            .position(|idx| *idx == self.selected_provider_idx)
        {
            order.rotate_left(pos);
        }

        self.guided_provider_order = order;
        self.guided_provider_pos = 0;
        self.config_provider_idx = self.current_guided_provider_idx();
        self.provider_model_cursor = 0;
        self.state = SetupState::ProviderConfig;
        Ok(())
    }

    fn reset_guided_flow(&mut self) {
        self.guided_provider_order.clear();
        self.guided_provider_pos = 0;
        self.config_provider_idx = None;
        self.provider_model_cursor = 0;
    }

    fn advance_provider_config(&mut self) {
        if self.guided_provider_pos + 1 < self.guided_provider_order.len() {
            self.guided_provider_pos += 1;
            self.config_provider_idx = self.current_guided_provider_idx();
            self.state = SetupState::ProviderConfig;
        } else {
            self.guided_provider_pos = 0;
            self.config_provider_idx = self.current_guided_provider_idx();
            self.provider_model_cursor = 0;
            self.state = SetupState::FetchingProviderModels;
        }
    }

    fn advance_provider_models(&mut self) {
        if self.guided_provider_pos + 1 < self.guided_provider_order.len() {
            self.guided_provider_pos += 1;
            self.config_provider_idx = self.current_guided_provider_idx();
            self.provider_model_cursor = 0;
            self.state = SetupState::FetchingProviderModels;
        } else if self.ensure_at_least_one_route() {
            self.state = SetupState::ModelOrderSelection;
        } else {
            self.state = SetupState::Error(
                "No active models. Select at least one model for an enabled provider.".to_string(),
            );
        }
    }

    fn enabled_provider_names(&self) -> Vec<String> {
        self.providers
            .iter()
            .filter(|p| p.enabled)
            .map(|p| p.name().to_string())
            .collect()
    }

    fn disabled_provider_names(&self) -> Vec<String> {
        self.providers
            .iter()
            .filter(|p| !p.enabled)
            .map(|p| p.name().to_string())
            .collect()
    }

    fn toggle_provider_enabled(&mut self, provider_idx: usize) {
        if let Some(provider) = self.providers.get_mut(provider_idx) {
            provider.enabled = !provider.enabled;
            if provider.enabled
                && provider.active_models.is_empty()
                && !provider.available_models.is_empty()
            {
                provider
                    .active_models
                    .push(provider.available_models[0].clone());
            }
        }
        self.update_model_order_from_active();
    }

    fn update_model_order_from_active(&mut self) -> bool {
        let mut valid = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for (provider_idx, provider) in self.providers.iter().enumerate() {
            if !provider.enabled {
                continue;
            }
            for model in &provider.active_models {
                let model = model.trim();
                if model.is_empty() {
                    continue;
                }
                let draft = ModelRouteDraft {
                    provider_idx,
                    model: model.to_string(),
                };
                if seen.insert(draft.key()) {
                    valid.push(draft);
                }
            }
        }

        if self.model_order.is_empty() {
            self.model_order = valid;
        } else {
            let valid_keys: std::collections::HashSet<String> =
                valid.iter().map(ModelRouteDraft::key).collect();
            let mut merged = Vec::new();
            let mut merged_keys = std::collections::HashSet::new();

            for old in &self.model_order {
                let key = old.key();
                if valid_keys.contains(&key) && merged_keys.insert(key) {
                    merged.push(old.clone());
                }
            }

            for v in valid {
                let key = v.key();
                if merged_keys.insert(key) {
                    merged.push(v);
                }
            }

            self.model_order = merged;
        }

        if self.model_order.is_empty() {
            self.model_order_cursor = 0;
            return false;
        }

        if self.model_order_cursor >= self.model_order.len() {
            self.model_order_cursor = self.model_order.len().saturating_sub(1);
        }

        true
    }

    async fn fetch_models_for_current_provider(&mut self) -> Result<()> {
        let provider_idx = self
            .config_provider_idx
            .ok_or_else(|| anyhow!("No provider selected for model fetch"))?;

        let provider_cfg = self.providers[provider_idx].to_provider_config_for_fetch();
        let primary = self.providers[provider_idx]
            .active_models
            .first()
            .cloned()
            .or_else(|| {
                self.providers[provider_idx]
                    .available_models
                    .first()
                    .cloned()
            })
            .or_else(|| provider_cfg.models.first().cloned())
            .unwrap_or_else(|| "gemini-2.5-flash-lite".to_string());

        let client = dexter_core::LlmClient::with_routes(
            vec![provider_cfg],
            Vec::new(),
            primary,
            Vec::new(),
        );

        let discovered_models = client.list_models().await;
        let discovered_models = match discovered_models {
            Ok(models) => {
                if self.providers[provider_idx].kind == ProviderKind::Ollama {
                    self.providers[provider_idx].runtime_ready = Some(true);
                }
                models
            }
            Err(_) => {
                if self.providers[provider_idx].kind == ProviderKind::Ollama {
                    self.providers[provider_idx].runtime_ready = Some(false);
                }
                self.providers[provider_idx].kind.default_models()
            }
        };

        let mut available_models = dedup_models(discovered_models);
        if available_models.is_empty() {
            available_models = self.providers[provider_idx].kind.default_models();
        }

        let available_set: std::collections::HashSet<String> =
            available_models.iter().cloned().collect();
        let mut active_models = self.providers[provider_idx]
            .active_models
            .iter()
            .filter(|m| available_set.contains(*m))
            .cloned()
            .collect::<Vec<_>>();

        if active_models.is_empty() && !available_models.is_empty() {
            active_models.push(available_models[0].clone());
        }

        self.providers[provider_idx].available_models = available_models;
        self.providers[provider_idx].active_models = dedup_models(active_models);
        self.provider_model_cursor = 0;
        Ok(())
    }

    fn toggle_model_selection(&mut self) {
        let Some(provider_idx) = self.config_provider_idx else {
            return;
        };
        let Some(provider) = self.providers.get_mut(provider_idx) else {
            return;
        };

        let model_count = provider.available_models.len();
        if model_count == 0 {
            return;
        }

        if self.provider_model_cursor < model_count {
            let model = provider.available_models[self.provider_model_cursor].clone();
            if let Some(pos) = provider.active_models.iter().position(|m| m == &model) {
                provider.active_models.remove(pos);
            } else {
                provider.active_models.push(model);
            }
            provider.active_models = dedup_models(provider.active_models.clone());
        } else {
            let all_selected = provider.active_models.len() == model_count;
            if all_selected {
                provider.active_models.clear();
            } else {
                provider.active_models = provider.available_models.clone();
            }
            provider.active_models = dedup_models(provider.active_models.clone());
        }

        self.update_model_order_from_active();
    }

    fn move_model_order_up(&mut self) {
        if self.model_order_cursor > 0 && self.model_order_cursor < self.model_order.len() {
            self.model_order
                .swap(self.model_order_cursor, self.model_order_cursor - 1);
            self.model_order_cursor -= 1;
        }
    }

    fn move_model_order_down(&mut self) {
        if self.model_order_cursor + 1 < self.model_order.len() {
            self.model_order
                .swap(self.model_order_cursor, self.model_order_cursor + 1);
            self.model_order_cursor += 1;
        }
    }

    fn ensure_at_least_one_route(&mut self) -> bool {
        self.update_model_order_from_active() && !self.model_order.is_empty()
    }

    async fn save_config(&mut self) -> Result<()> {
        self.ensure_at_least_one_route();

        let providers_cfg: Vec<ProviderConfig> = self
            .providers
            .iter()
            .map(|p| p.to_provider_config())
            .collect();
        self.config.providers = providers_cfg.clone();

        // Keep legacy fields for backward compatibility.
        self.config.api_keys.gemini = providers_cfg
            .iter()
            .find(|p| matches!(p.kind, ProviderKind::Gemini))
            .and_then(|p| p.api_key.clone());
        self.config.api_keys.deepseek = providers_cfg
            .iter()
            .find(|p| matches!(p.kind, ProviderKind::Deepseek))
            .and_then(|p| p.api_key.clone());
        self.config.api_keys.base_url = None;

        let routes: Vec<ModelRoute> = self
            .model_order
            .iter()
            .filter_map(|r| {
                self.providers.get(r.provider_idx).map(|p| ModelRoute {
                    provider: p.kind,
                    model: r.model.clone(),
                })
            })
            .collect();

        self.config.models.router_routes = routes.clone();
        self.config.models.executor_routes = routes.clone();

        if let Some(primary) = routes.first() {
            self.config.models.router_model = primary.model.clone();
            self.config.models.executor_model = primary.model.clone();
        }

        let fallback_models =
            dedup_models(routes.iter().skip(1).map(|r| r.model.clone()).collect());
        self.config.models.router_fallback_models = fallback_models.clone();
        self.config.models.executor_fallback_models = fallback_models;

        let theme_id = self.available_themes[self.selected_theme_idx].0;
        self.config.theme = theme_id.to_string();

        self.config.save().await?;
        Ok(())
    }
}

async fn run_setup_wizard(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    config: Config,
) -> Result<Config> {
    run_setup_flow(terminal, config, true).await
}

async fn run_settings_panel(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    config: Config,
) -> Result<Config> {
    run_setup_flow(terminal, config, false).await
}

async fn run_setup_flow(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    config: Config,
    show_welcome: bool,
) -> Result<Config> {
    let mut app = SetupApp::new(config, show_welcome);
    app.refresh_runtime_statuses().await;

    loop {
        terminal.draw(|f| setup_ui(f, &app))?;

        if app.state == SetupState::FetchingProviderModels {
            if let Err(e) = app.fetch_models_for_current_provider().await {
                app.state = SetupState::Error(format!("Model discovery failed: {}", e));
            } else {
                app.state = SetupState::ProviderModelSelection;
            }
            continue;
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match app.state {
                        SetupState::Welcome => match key.code {
                            KeyCode::Enter => app.state = SetupState::ProviderSelection,
                            KeyCode::Esc => return Err(anyhow!("Setup aborted by user")),
                            _ => {}
                        },
                        SetupState::ProviderSelection => match key.code {
                            KeyCode::Up | KeyCode::Left => {
                                if app.selected_provider_idx > 0 {
                                    app.selected_provider_idx -= 1;
                                }
                            }
                            KeyCode::Down | KeyCode::Right => {
                                if app.selected_provider_idx
                                    < app.provider_selection_len().saturating_sub(1)
                                {
                                    app.selected_provider_idx += 1;
                                }
                            }
                            KeyCode::Char(' ') => {
                                app.toggle_provider_enabled(app.selected_provider_idx);
                            }
                            KeyCode::Enter => {
                                if let Err(e) = app.start_guided_flow() {
                                    app.state = SetupState::Error(e.to_string());
                                }
                            }
                            KeyCode::Esc => return Err(anyhow!("Setup aborted")),
                            _ => {}
                        },
                        SetupState::ProviderConfig => {
                            let Some(provider_idx) = app.config_provider_idx else {
                                app.state = SetupState::ProviderSelection;
                                continue;
                            };
                            let requires_key = app.providers[provider_idx].requires_api_key();
                            match key.code {
                                KeyCode::Enter => {
                                    if requires_key
                                        && app.providers[provider_idx].api_key.trim().is_empty()
                                    {
                                        app.state = SetupState::Error(format!(
                                            "{} requires an API key.",
                                            app.providers[provider_idx].name()
                                        ));
                                    } else {
                                        app.advance_provider_config();
                                    }
                                }
                                KeyCode::Char(c) => {
                                    if requires_key {
                                        app.providers[provider_idx].api_key.push(c);
                                    }
                                }
                                KeyCode::Backspace => {
                                    if requires_key {
                                        app.providers[provider_idx].api_key.pop();
                                    }
                                }
                                KeyCode::Esc => {
                                    app.reset_guided_flow();
                                    app.state = SetupState::ProviderSelection;
                                }
                                _ => {}
                            }
                        }
                        SetupState::FetchingProviderModels => {}
                        SetupState::ProviderModelSelection => {
                            let Some(provider_idx) = app.config_provider_idx else {
                                app.state = SetupState::ProviderSelection;
                                continue;
                            };
                            let model_count = app.providers[provider_idx].available_models.len();
                            let max_idx = model_count; // last row is Select All
                            match key.code {
                                KeyCode::Up | KeyCode::Left => {
                                    if app.provider_model_cursor > 0 {
                                        app.provider_model_cursor -= 1;
                                    }
                                }
                                KeyCode::Down | KeyCode::Right => {
                                    if app.provider_model_cursor < max_idx {
                                        app.provider_model_cursor += 1;
                                    }
                                }
                                KeyCode::Char(' ') => app.toggle_model_selection(),
                                KeyCode::Enter => {
                                    if app.providers[provider_idx].enabled
                                        && app.providers[provider_idx].active_models.is_empty()
                                        && !app.providers[provider_idx].available_models.is_empty()
                                    {
                                        let first_model =
                                            app.providers[provider_idx].available_models[0].clone();
                                        app.providers[provider_idx].active_models.push(first_model);
                                    }
                                    app.update_model_order_from_active();
                                    app.advance_provider_models();
                                }
                                KeyCode::Esc => app.state = SetupState::ProviderConfig,
                                _ => {}
                            }
                        }
                        SetupState::ModelOrderSelection => match key.code {
                            KeyCode::Up | KeyCode::Left => {
                                if app.model_order_cursor > 0 {
                                    app.model_order_cursor -= 1;
                                }
                            }
                            KeyCode::Down | KeyCode::Right => {
                                if app.model_order_cursor < app.model_order.len().saturating_sub(1)
                                {
                                    app.model_order_cursor += 1;
                                }
                            }
                            KeyCode::Char('u') | KeyCode::Char('k') => app.move_model_order_up(),
                            KeyCode::Char('d') | KeyCode::Char('j') => app.move_model_order_down(),
                            KeyCode::Enter => {
                                if app.model_order.is_empty() {
                                    app.state = SetupState::Error(
                                        "No active models in routing order.".to_string(),
                                    );
                                } else {
                                    app.state = SetupState::Saving;
                                    app.save_config().await?;
                                    return Ok(app.config);
                                }
                            }
                            KeyCode::Esc => {
                                app.reset_guided_flow();
                                app.state = SetupState::ProviderSelection;
                            }
                            _ => {}
                        },
                        SetupState::ThemeSelection => match key.code {
                            KeyCode::Up | KeyCode::Left => {
                                if app.selected_theme_idx > 0 {
                                    app.selected_theme_idx -= 1;
                                    let theme_id = app.available_themes[app.selected_theme_idx].0;
                                    app.theme = Theme::from_config(theme_id);
                                }
                            }
                            KeyCode::Down | KeyCode::Right => {
                                if app.selected_theme_idx
                                    < app.available_themes.len().saturating_sub(1)
                                {
                                    app.selected_theme_idx += 1;
                                    let theme_id = app.available_themes[app.selected_theme_idx].0;
                                    app.theme = Theme::from_config(theme_id);
                                }
                            }
                            KeyCode::Enter => app.state = SetupState::Confirm,
                            KeyCode::Esc => app.state = SetupState::ModelOrderSelection,
                            _ => {}
                        },
                        SetupState::Confirm => match key.code {
                            KeyCode::Enter | KeyCode::Char('y') => {
                                app.state = SetupState::Saving;
                                app.save_config().await?;
                                return Ok(app.config);
                            }
                            KeyCode::Esc | KeyCode::Char('n') => {
                                app.state = SetupState::ThemeSelection
                            }
                            _ => {}
                        },
                        SetupState::Error(_) => {
                            if key.code == KeyCode::Enter || key.code == KeyCode::Esc {
                                app.state = SetupState::ProviderSelection;
                            }
                        }
                        SetupState::Saving => {}
                    }
                }
            }
        }
    }
}

fn setup_ui(f: &mut Frame, app: &SetupApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(f.area());

    let header = Paragraph::new(Span::styled(
        " D E X T E R  //  INITIALIZATION ",
        app.theme.header_title_style,
    ))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(app.theme.border_style),
    );
    f.render_widget(header, chunks[0]);

    if app.state == SetupState::ProviderSelection {
        render_setup_provider_table(f, app, chunks[1]);
        return;
    }
    if app.state == SetupState::ProviderModelSelection {
        render_setup_models_table(f, app, chunks[1]);
        return;
    }
    if app.state == SetupState::ModelOrderSelection {
        render_setup_model_order_table(f, app, chunks[1]);
        return;
    }

    let content_text = match &app.state {
        SetupState::Welcome => vec![
            Line::from(""),
            Line::from(Span::styled(
                "WELCOME TO DEXTER",
                app.theme.header_title_style,
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Configure Providers, Models, and Fallback Priority.",
                app.theme.header_subtitle_style,
            )),
            Line::from(""),
            Line::from(Span::styled(
                "[PRESS ENTER TO BEGIN]",
                app.theme.input_cursor_style,
            )),
        ],
        SetupState::ProviderSelection => vec![],
        SetupState::ProviderConfig => {
            if let Some(provider_idx) = app.config_provider_idx {
                let provider = &app.providers[provider_idx];
                let total = app.guided_provider_order.len().max(1);
                let current = app.guided_provider_pos + 1;
                vec![
                    Line::from(Span::styled(
                        format!("STEP 2: PROVIDERS CONFIG ({}/{})", current, total),
                        app.theme.header_title_style,
                    )),
                    Line::from(""),
                    Line::from(format!("Provider: {}", provider.name())),
                    Line::from(format!("Runtime Enabled: {}", provider.enabled)),
                    Line::from(format!("Base URL: {}", provider.base_url)),
                    Line::from(""),
                    Line::from(if provider.requires_api_key() {
                        "API Key (editable):"
                    } else {
                        "API Key not required for this provider"
                    }),
                    Line::from(vec![
                        Span::styled("> ", app.theme.input_prompt_style),
                        Span::styled(
                            if provider.requires_api_key() {
                                if provider.api_key.is_empty() {
                                    "_"
                                } else {
                                    &provider.api_key
                                }
                            } else {
                                "N/A"
                            },
                            app.theme.input_text_style,
                        ),
                    ]),
                    Line::from(""),
                    Line::from(Span::styled(
                        "ENTER: Save & Next Provider  ESC: Back to Step 1",
                        app.theme.header_subtitle_style,
                    )),
                ]
            } else {
                vec![
                    Line::from(Span::styled(
                        "STEP 2: PROVIDER CONFIG",
                        app.theme.header_title_style,
                    )),
                    Line::from("No provider selected."),
                ]
            }
        }
        SetupState::FetchingProviderModels => {
            let provider_name = app
                .config_provider_idx
                .and_then(|idx| app.providers.get(idx))
                .map(|p| p.name().to_string())
                .unwrap_or_else(|| "Provider".to_string());
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    "STEP 3: FETCHING PROVIDER MODELS",
                    app.theme.header_title_style,
                )),
                Line::from(""),
                Line::from(format!("Provider: {}", provider_name)),
                Line::from("Connecting and discovering latest models..."),
                Line::from(""),
                Line::from(Span::styled("[PLEASE WAIT]", app.theme.input_cursor_style)),
            ]
        }
        SetupState::ProviderModelSelection => {
            if let Some(provider_idx) = app.config_provider_idx {
                let provider = &app.providers[provider_idx];
                let total = app.guided_provider_order.len().max(1);
                let current = app.guided_provider_pos + 1;
                let mut lines = vec![
                    Line::from(Span::styled(
                        format!("STEP 3: MODELS TOGGLE ({}/{})", current, total),
                        app.theme.header_title_style,
                    )),
                    Line::from(""),
                    Line::from(format!("Provider: {}", provider.name())),
                    Line::from("Select one or more models to activate for fallback."),
                    Line::from(""),
                ];

                for (i, model) in provider.available_models.iter().enumerate() {
                    let is_cursor = i == app.provider_model_cursor;
                    let checked = provider.active_models.iter().any(|m| m == model);
                    let style = if is_cursor {
                        app.theme.proposal_cmd_style
                    } else {
                        app.theme.header_subtitle_style
                    };
                    lines.push(Line::from(Span::styled(
                        format!(
                            "{}[{}] {}",
                            if is_cursor { "> " } else { "  " },
                            if checked { "x" } else { " " },
                            model
                        ),
                        style,
                    )));
                }

                let all_selected = !provider.available_models.is_empty()
                    && provider.active_models.len() == provider.available_models.len();
                let select_all_cursor =
                    app.provider_model_cursor == provider.available_models.len();
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(
                        "{}[{}] Select All",
                        if select_all_cursor { "> " } else { "  " },
                        if all_selected { "x" } else { " " }
                    ),
                    if select_all_cursor {
                        app.theme.proposal_cmd_style
                    } else {
                        app.theme.header_subtitle_style
                    },
                )));

                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "SPACE: Toggle model / Select All  ENTER: Save & Next",
                    app.theme.header_subtitle_style,
                )));
                lines
            } else {
                vec![
                    Line::from(Span::styled(
                        "STEP 4: SELECT ACTIVE MODELS",
                        app.theme.header_title_style,
                    )),
                    Line::from("No provider selected."),
                ]
            }
        }
        SetupState::ModelOrderSelection => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "STEP 4: ALL MODELS CONFIRMATION / FALLBACK ORDER",
                    app.theme.header_title_style,
                )),
                Line::from(""),
                Line::from("Primary model = top item. Fallbacks follow in order."),
                Line::from("Use U/K to move up, D/J to move down."),
                Line::from(""),
            ];

            if app.model_order.is_empty() {
                lines.push(Line::from(Span::styled(
                    "No active models selected yet.",
                    Style::default().fg(Color::Red),
                )));
            } else {
                for (i, route) in app.model_order.iter().enumerate() {
                    let is_cursor = i == app.model_order_cursor;
                    let style = if is_cursor {
                        app.theme.proposal_cmd_style
                    } else {
                        app.theme.header_subtitle_style
                    };
                    lines.push(Line::from(Span::styled(
                        format!(
                            "{}{}. {}",
                            if is_cursor { "> " } else { "  " },
                            i + 1,
                            model_route_display(route, &app.providers)
                        ),
                        style,
                    )));
                }
            }

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "ENTER: Save & Leave  ESC: Back to Step 1",
                app.theme.header_subtitle_style,
            )));
            lines
        }
        SetupState::ThemeSelection => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "STEP 6: SELECT THEME",
                    app.theme.header_title_style,
                )),
                Line::from(""),
                Line::from("Choose your preferred color scheme:"),
                Line::from(""),
            ];

            for (i, (_, display_name)) in app.available_themes.iter().enumerate() {
                let style = if i == app.selected_theme_idx {
                    app.theme.proposal_cmd_style
                } else {
                    app.theme.header_subtitle_style
                };
                let prefix = if i == app.selected_theme_idx {
                    "> "
                } else {
                    "  "
                };
                lines.push(Line::from(Span::styled(
                    format!("{}{}", prefix, display_name),
                    style,
                )));
            }

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "(Use Arrow Keys to Select, ENTER to Confirm)",
                app.theme.header_subtitle_style,
            )));
            lines
        }
        SetupState::Confirm => {
            let primary = app
                .model_order
                .first()
                .map(|r| model_route_display(r, &app.providers))
                .unwrap_or_else(|| "Unknown".to_string());
            let fallback = if app.model_order.len() > 1 {
                app.model_order
                    .iter()
                    .skip(1)
                    .map(|r| model_route_display(r, &app.providers))
                    .collect::<Vec<_>>()
                    .join(" -> ")
            } else {
                "None".to_string()
            };

            vec![
                Line::from(Span::styled(
                    "CONFIRM SETTINGS",
                    app.theme.header_title_style,
                )),
                Line::from(""),
                Line::from(format!(
                    "Enabled Providers: {}",
                    app.enabled_provider_names().join(", ")
                )),
                Line::from(format!(
                    "Disabled Providers: {}",
                    app.disabled_provider_names().join(", ")
                )),
                Line::from(format!("Primary Model: {}", primary)),
                Line::from(format!("Fallback Order: {}", fallback)),
                Line::from(format!(
                    "Theme: {}",
                    app.available_themes[app.selected_theme_idx].1
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Save and Apply? [Y/n]",
                    app.theme.input_prompt_style,
                )),
            ]
        }
        SetupState::Saving => vec![
            Line::from(""),
            Line::from(Span::styled(
                "SAVING CONFIGURATION...",
                app.theme.input_cursor_style,
            )),
        ],
        SetupState::Error(e) => vec![
            Line::from(Span::styled("ERROR", Style::default().fg(Color::Red))),
            Line::from(e.clone()),
            Line::from(""),
            Line::from(Span::styled(
                "Press ENTER/ESC to return to provider list.",
                app.theme.header_subtitle_style,
            )),
        ],
    };

    let content = Paragraph::new(content_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.theme.border_style)
                .title(" SETUP WIZARD "),
        )
        .style(app.theme.base_style)
        .wrap(Wrap { trim: true });
    f.render_widget(content, chunks[1]);
}

fn render_setup_provider_table(f: &mut Frame, app: &SetupApp, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border_style)
        .title(" SETUP WIZARD ");
    f.render_widget(&block, area);
    let inner = block.inner(area);
    let compact = inner.width < 98;
    let very_narrow = inner.width < 78;

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if compact { 8 } else { 7 }), // title + help
            Constraint::Length(1),
            Constraint::Length((app.providers.len() as u16).saturating_add(2)), // header + rows
            Constraint::Min(1),
        ])
        .split(inner);

    let intro = vec![
        Line::from(Span::styled(
            if very_narrow {
                "STEP 1: PROVIDERS"
            } else {
                "STEP 1: PROVIDERS (ENTER TO CONFIGURE SELECTED PROVIDER)"
            },
            app.theme.header_title_style,
        )),
        Line::from(""),
        Line::from("SPACE: Toggle provider enabled/disabled for runtime fallback"),
        Line::from(if very_narrow {
            "ENTER: Continue setup"
        } else {
            "ENTER: Continue to providers config"
        }),
        Line::from("OFF providers keep model selections saved but not active at runtime."),
    ];
    let intro_para = Paragraph::new(intro)
        .style(app.theme.header_subtitle_style)
        .wrap(Wrap { trim: true });
    f.render_widget(intro_para, layout[0]);

    let header = Row::new(vec![
        Cell::from("  "),
        Cell::from(if very_narrow { "ON/OFF" } else { "TOGGLE" }),
        Cell::from("PROVIDER"),
        Cell::from("SETUP"),
        Cell::from(if very_narrow {
            "ACTIVE"
        } else {
            "ACTIVE MODELS"
        }),
    ])
    .style(app.theme.footer_text_style.add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app
        .providers
        .iter()
        .enumerate()
        .map(|(idx, provider)| {
            let is_cursor = idx == app.selected_provider_idx;
            let toggle = if provider.enabled {
                "◉ ON"
            } else {
                "○ OFF"
            };
            let setup_state = if provider.kind == ProviderKind::Ollama {
                if provider.runtime_ready.unwrap_or(false) {
                    "SET"
                } else {
                    "NOT SET"
                }
            } else if provider.requires_api_key() {
                if provider.has_key() {
                    "SET"
                } else {
                    "NOT SET"
                }
            } else {
                "SET"
            };
            let active_runtime = if provider.enabled {
                provider.active_models.len().to_string()
            } else {
                "0".to_string()
            };

            Row::new(vec![
                Cell::from(if is_cursor { "> " } else { "  " }),
                Cell::from(toggle),
                Cell::from(provider.name().to_string()),
                Cell::from(setup_state),
                Cell::from(active_runtime),
            ])
            .style(if is_cursor {
                app.theme.proposal_cmd_style
            } else {
                app.theme.header_subtitle_style
            })
        })
        .collect();

    let provider_name_width = app
        .providers
        .iter()
        .map(|provider| provider.name().chars().count() as u16)
        .max()
        .unwrap_or(10);

    let table_widths = if very_narrow {
        vec![
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Min(provider_name_width.max(8)),
            Constraint::Length(8),
            Constraint::Length(6),
        ]
    } else if compact {
        vec![
            Constraint::Length(2),
            Constraint::Length(8),
            Constraint::Min(provider_name_width.max(10)),
            Constraint::Length(10),
            Constraint::Length(8),
        ]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Length(8),
            Constraint::Min(provider_name_width.max(12)),
            Constraint::Length(10),
            Constraint::Length(14),
        ]
    };
    let table = Table::new(rows, table_widths)
        .header(header)
        .column_spacing(1)
        .style(app.theme.base_style);

    f.render_widget(table, layout[2]);
}

fn render_setup_models_table(f: &mut Frame, app: &SetupApp, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border_style)
        .title(" SETUP WIZARD ");
    f.render_widget(&block, area);
    let inner = block.inner(area);

    let Some(provider_idx) = app.config_provider_idx else {
        let fallback = Paragraph::new("STEP 3: MODELS TOGGLE\n\nNo provider selected.")
            .style(app.theme.header_subtitle_style);
        f.render_widget(fallback, inner);
        return;
    };
    let provider = &app.providers[provider_idx];
    let compact = inner.width < 98;
    let very_narrow = inner.width < 78;
    let total = app.guided_provider_order.len().max(1);
    let current = app.guided_provider_pos + 1;
    let table_height = (provider.available_models.len() as u16).saturating_add(3);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if compact { 7 } else { 6 }),
            Constraint::Length(1),
            Constraint::Length(table_height),
            Constraint::Min(1),
        ])
        .split(inner);

    let intro = vec![
        Line::from(Span::styled(
            format!("STEP 3: MODELS TOGGLE ({}/{})", current, total),
            app.theme.header_title_style,
        )),
        Line::from(""),
        Line::from(format!("Provider: {}", provider.name())),
        Line::from(if very_narrow {
            "Select models for runtime fallback."
        } else {
            "Select one or more models to activate for fallback."
        }),
    ];
    let intro_para = Paragraph::new(intro)
        .style(app.theme.header_subtitle_style)
        .wrap(Wrap { trim: true });
    f.render_widget(intro_para, layout[0]);

    let header = Row::new(vec![
        Cell::from("  "),
        Cell::from(if very_narrow { "ON/OFF" } else { "TOGGLE" }),
        Cell::from("MODEL"),
    ])
    .style(app.theme.footer_text_style.add_modifier(Modifier::BOLD));

    let mut rows: Vec<Row> = provider
        .available_models
        .iter()
        .enumerate()
        .map(|(idx, model)| {
            let is_cursor = idx == app.provider_model_cursor;
            let selected = provider.active_models.iter().any(|m| m == model);
            let toggle = if selected { "◉ ON" } else { "○ OFF" };
            Row::new(vec![
                Cell::from(if is_cursor { "> " } else { "  " }),
                Cell::from(toggle),
                Cell::from(model.to_string()),
            ])
            .style(if is_cursor {
                app.theme.proposal_cmd_style
            } else {
                app.theme.header_subtitle_style
            })
        })
        .collect();

    let all_selected = !provider.available_models.is_empty()
        && provider.active_models.len() == provider.available_models.len();
    let select_all_cursor = app.provider_model_cursor == provider.available_models.len();
    rows.push(
        Row::new(vec![
            Cell::from(if select_all_cursor { "> " } else { "  " }),
            Cell::from(if all_selected { "◉ ON" } else { "○ OFF" }),
            Cell::from("Select All"),
        ])
        .style(if select_all_cursor {
            app.theme.proposal_cmd_style
        } else {
            app.theme.header_subtitle_style
        }),
    );

    let table_widths = if very_narrow {
        vec![
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Min(8),
        ]
    } else if compact {
        vec![
            Constraint::Length(2),
            Constraint::Length(8),
            Constraint::Min(12),
        ]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Length(8),
            Constraint::Min(20),
        ]
    };
    let table = Table::new(rows, table_widths)
        .header(header)
        .column_spacing(1)
        .style(app.theme.base_style);
    f.render_widget(table, layout[2]);

    let help = Paragraph::new(if very_narrow {
        "SPACE: Toggle / Select All   ENTER: Save & Next"
    } else {
        "SPACE: Toggle model / Select All   ENTER: Save & Next"
    })
    .style(app.theme.header_subtitle_style)
    .wrap(Wrap { trim: true });
    f.render_widget(help, layout[3]);
}

fn render_setup_model_order_table(f: &mut Frame, app: &SetupApp, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border_style)
        .title(" SETUP WIZARD ");
    f.render_widget(&block, area);
    let inner = block.inner(area);
    let compact = inner.width < 98;
    let very_narrow = inner.width < 78;

    let table_height = if app.model_order.is_empty() {
        3
    } else {
        (app.model_order.len() as u16).saturating_add(2)
    };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if compact { 8 } else { 7 }),
            Constraint::Length(1),
            Constraint::Length(table_height),
            Constraint::Min(1),
        ])
        .split(inner);

    let intro = vec![
        Line::from(Span::styled(
            "STEP 4: ALL MODELS CONFIRMATION / FALLBACK ORDER",
            app.theme.header_title_style,
        )),
        Line::from(""),
        Line::from("Primary model = top item. Fallbacks follow in order."),
        Line::from(if very_narrow {
            "U/K: move up   D/J: move down"
        } else {
            "Use U/K to move up, D/J to move down."
        }),
    ];
    let intro_para = Paragraph::new(intro)
        .style(app.theme.header_subtitle_style)
        .wrap(Wrap { trim: true });
    f.render_widget(intro_para, layout[0]);

    let header = Row::new(vec![
        Cell::from("  "),
        Cell::from("ORDER"),
        Cell::from("MODEL"),
        Cell::from(if very_narrow { "PROV" } else { "PROVIDER" }),
    ])
    .style(app.theme.footer_text_style.add_modifier(Modifier::BOLD));

    let rows = if app.model_order.is_empty() {
        vec![Row::new(vec![
            Cell::from("  "),
            Cell::from("--"),
            Cell::from("No active models selected yet."),
            Cell::from("--"),
        ])
        .style(Style::default().fg(Color::Red))]
    } else {
        app.model_order
            .iter()
            .enumerate()
            .map(|(idx, route)| {
                let is_cursor = idx == app.model_order_cursor;
                let provider = app
                    .providers
                    .get(route.provider_idx)
                    .map(|p| p.name().to_string())
                    .unwrap_or_else(|| "Unknown".to_string());
                Row::new(vec![
                    Cell::from(if is_cursor { "> " } else { "  " }),
                    Cell::from(format!("[{}]", idx + 1)),
                    Cell::from(route.model.clone()),
                    Cell::from(provider),
                ])
                .style(if is_cursor {
                    app.theme.proposal_cmd_style
                } else {
                    app.theme.header_subtitle_style
                })
            })
            .collect::<Vec<_>>()
    };

    let table_widths = if very_narrow {
        vec![
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Min(10),
            Constraint::Length(8),
        ]
    } else if compact {
        vec![
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Min(16),
            Constraint::Length(10),
        ]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Min(24),
            Constraint::Length(12),
        ]
    };
    let table = Table::new(rows, table_widths)
        .header(header)
        .column_spacing(1)
        .style(app.theme.base_style);
    f.render_widget(table, layout[2]);

    let help = Paragraph::new("ENTER: Save & Leave   ESC: Back to Step 1")
        .style(app.theme.header_subtitle_style)
        .wrap(Wrap { trim: true });
    f.render_widget(help, layout[3]);
}

fn build_provider_entries(config: &Config) -> Vec<SetupProviderEntry> {
    let existing = config.effective_providers();
    let supported = [
        ProviderKind::OpenAI,
        ProviderKind::Anthropic,
        ProviderKind::OpenRouter,
        ProviderKind::Moonshot,
        ProviderKind::Gemini,
        ProviderKind::Deepseek,
        ProviderKind::Groq,
        ProviderKind::Baseten,
        ProviderKind::Ollama,
        ProviderKind::OpenAICompatible,
        ProviderKind::AnthropicCompatible,
    ];

    let mut entries = supported
        .iter()
        .map(|kind| {
            let mut base = ProviderConfig::builtin(*kind, None).normalized();
            let mut enabled = false;
            if let Some(existing_provider) = existing.iter().find(|p| p.kind == *kind) {
                base.api_key = existing_provider.api_key.clone();
                base.base_url = existing_provider.base_url.clone();
                base.auth = existing_provider.auth;
                base.models = dedup_models(existing_provider.models.clone());
                enabled = existing_provider.enabled;
            }

            let mut available_models = if base.models.is_empty() {
                (*kind).default_models()
            } else {
                base.models.clone()
            };
            let active_models = base.models.clone();

            if available_models.is_empty() {
                available_models = (*kind).default_models();
            }

            SetupProviderEntry {
                kind: *kind,
                enabled,
                api_key: base.api_key.unwrap_or_default(),
                base_url: base.base_url,
                auth: base.auth,
                available_models: dedup_models(available_models),
                active_models: dedup_models(active_models),
                runtime_ready: None,
            }
        })
        .collect::<Vec<_>>();

    // Route config has highest priority for active models.
    for route in &config.models.executor_routes {
        if route.model.trim().is_empty() {
            continue;
        }
        if let Some(entry) = entries.iter_mut().find(|e| e.kind == route.provider) {
            if !entry.available_models.iter().any(|m| m == &route.model) {
                entry.available_models.push(route.model.clone());
            }
            if !entry.active_models.iter().any(|m| m == &route.model) {
                entry.active_models.push(route.model.clone());
            }
            entry.enabled = true;
        }
    }

    for entry in &mut entries {
        entry.available_models = dedup_models(entry.available_models.clone());
        entry.active_models = dedup_models(entry.active_models.clone());
        if entry.enabled && entry.active_models.is_empty() && !entry.available_models.is_empty() {
            entry.active_models.push(entry.available_models[0].clone());
        }
    }

    entries
}

fn dedup_models(models: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for m in models {
        let m = m.trim();
        if m.is_empty() {
            continue;
        }
        if !out.iter().any(|x: &String| x == m) {
            out.push(m.to_string());
        }
    }
    out
}

fn model_route_display(route: &ModelRouteDraft, providers: &[SetupProviderEntry]) -> String {
    let provider_name = providers
        .get(route.provider_idx)
        .map(|p| p.name().to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    format!("{} ({})", route.model, provider_name)
}
