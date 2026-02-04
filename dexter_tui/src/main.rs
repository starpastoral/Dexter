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
use dexter_core::{Config, ContextScanner, Executor, LlmClient, Router, SafetyGuard};
use dexter_plugins::{F2Plugin, FFmpegPlugin, Plugin, PreviewContent, YtDlpPlugin};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Gauge, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
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
}

#[derive(Clone, Debug)]
struct FooterButton {
    rect: Rect,
    action: FooterAction,
}

struct App {
    state: AppState,
    input: String,
    router: Router,
    executor: Executor,
    plugins: Vec<Arc<dyn Plugin>>,
    selected_plugin: Option<String>,
    generated_command: Option<String>,
    command_draft: String,
    logs: Vec<String>,
    tick_count: u64,
    current_context: Option<dexter_core::context::FileContext>,
    dry_run_output: Option<PreviewContent>,
    show_debug: bool,
    config: Config,
    theme: Theme,

    // Focus + interactive footer buttons
    focus: FocusArea,
    footer_buttons: Vec<FooterButton>,
    footer_focus: usize,

    // Output scrolling
    output_scroll: u16,
    output_max_scroll: u16,
    output_content_length: u16,
    output_viewport_height: u16,
    output_scrollbar_rect: Option<Rect>,

    // Async execution handling
    progress_rx: Option<mpsc::Receiver<dexter_plugins::Progress>>,
    execution_result_rx: Option<oneshot::Receiver<Result<String>>>,
    progress: Option<dexter_plugins::Progress>,
}

impl App {
    fn new(config: Config) -> Self {
        // Prioritize specific API key, fallback to others if we implemented that logic.
        // For now, assuming Gemini as primary based on original code, but we should probably check others.
        let api_key = config
            .api_keys
            .gemini
            .clone()
            .or(config.api_keys.deepseek.clone())
            .unwrap_or_default();

        let base_url = config.api_keys.base_url.clone().unwrap_or_else(|| {
            "https://generativelanguage.googleapis.com/v1beta/openai/".to_string()
        });

        let router_client = LlmClient::new(
            api_key.clone(),
            base_url.clone(),
            config.models.router_model.clone(),
        );

        let executor_client =
            LlmClient::new(api_key, base_url, config.models.executor_model.clone());

        let theme = Theme::from_config(&config.theme);

        Self {
            state: AppState::Input,
            input: String::new(),
            router: Router::new(router_client),
            executor: Executor::new(executor_client),
            plugins: vec![
                Arc::new(F2Plugin) as Arc<dyn Plugin>,
                Arc::new(FFmpegPlugin) as Arc<dyn Plugin>,
                Arc::new(YtDlpPlugin) as Arc<dyn Plugin>,
            ],
            selected_plugin: None,
            generated_command: None,
            command_draft: String::new(),
            logs: vec!["Dexter initialized. Ready for your command.".to_string()],
            tick_count: 0,
            current_context: None,
            dry_run_output: None,
            show_debug: false,
            config,
            theme,
            focus: FocusArea::Proposal,
            footer_buttons: Vec::new(),
            footer_focus: 0,
            output_scroll: 0,
            output_max_scroll: 0,
            output_content_length: 0,
            output_viewport_height: 0,
            output_scrollbar_rect: None,
            progress_rx: None,
            execution_result_rx: None,
            progress: None,
        }
    }

    async fn update_context(&mut self) -> Result<()> {
        let context = ContextScanner::scan_cwd().await?;
        self.current_context = Some(context);
        Ok(())
    }

    async fn run_routing(&mut self) -> Result<()> {
        // Always refresh context before routing to ensure latest file list
        self.update_context().await?;
        let context = self.current_context.as_ref().unwrap();

        match self
            .router
            .route(&self.input, &context, &self.plugins)
            .await
        {
            Ok(plugin_name) => {
                self.selected_plugin = Some(plugin_name.clone());
                self.logs.push(format!("Routed to plugin: {}", plugin_name));
                self.state = AppState::PendingGeneration;
            }
            Err(e) => {
                self.state = AppState::Error(format!("Routing error: {}", e));
            }
        }
        Ok(())
    }

    async fn run_generation(&mut self) -> Result<()> {
        let plugin_name = self.selected_plugin.as_ref().unwrap();
        let plugin = self
            .plugins
            .iter()
            .find(|p| p.name() == *plugin_name)
            .unwrap();
        let context = self.current_context.as_ref().unwrap();

        match self
            .executor
            .generate_command(&self.input, &context, plugin.as_ref())
            .await
        {
            Ok(cmd) => {
                self.generated_command = Some(cmd.clone());
                self.command_draft = cmd.clone();
                self.logs.push(format!("Generated command: {}", cmd));
                self.dry_run_output = None;
                self.output_scroll = 0;
                self.state = AppState::PendingDryRun;
            }
            Err(e) => {
                self.state = AppState::Error(format!("Generation error: {}", e));
            }
        }
        Ok(())
    }

    async fn run_dry_run(&mut self) -> Result<()> {
        let cmd = self.generated_command.as_ref().unwrap();
        let plugin_name = self.selected_plugin.as_ref().unwrap();
        let plugin = self
            .plugins
            .iter()
            .find(|p| p.name() == *plugin_name)
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

        self.logs.push(format!("Executing preview: {}", cmd));
        match plugin.dry_run(cmd, Some(self.executor.llm_client())).await {
            Ok(output) => {
                self.logs
                    .push("Preview data captured successfully".to_string());
                self.dry_run_output = Some(output);
                self.output_scroll = 0;
                self.state = AppState::AwaitingConfirmation;
            }
            Err(e) => {
                self.logs.push(format!("Preview failed: {}", e));
                self.state = AppState::Error(format!("Dry run failed: {}", e));
            }
        }
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

    // Try enabling the kitty keyboard protocol so we can distinguish modifiers (e.g. Ctrl+Enter).
    // This is terminal-dependent; we fall back gracefully when unsupported.
    let mut keyboard_enhancement_enabled = false;
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
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
                app.run_routing().await?;
            }
            AppState::PendingGeneration => {
                app.state = AppState::Generating;
                app.run_generation().await?;
            }
            AppState::PendingDryRun => {
                app.state = AppState::DryRunning;
                app.run_dry_run().await?;
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

        // Keep output scrolling bounds in sync with current terminal size/content.
        update_output_scroll_bounds(terminal, app)?;

        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // Global output scrolling keys (work in most states).
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
                            if app.focus == FocusArea::FooterButtons && !app.footer_buttons.is_empty()
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
                            if app.focus == FocusArea::FooterButtons && !app.footer_buttons.is_empty()
                            {
                                app.footer_focus = (app.footer_focus + 1) % app.footer_buttons.len();
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
                                    app.input.push('\n');
                                }
                            }
                            KeyCode::Char(c) => {
                                if app.focus == FocusArea::Proposal {
                                    app.input.push(c);
                                }
                            }
                            KeyCode::Backspace => {
                                if app.focus == FocusArea::Proposal {
                                    app.input.pop();
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
                                let should_quit = perform_footer_action(
                                    app,
                                    FooterAction::PreviewEditedCommand,
                                )
                                .await?;
                                if should_quit {
                                    return Ok(());
                                }
                            }
                            KeyCode::Enter
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                let should_quit = perform_footer_action(
                                    app,
                                    FooterAction::PreviewEditedCommand,
                                )
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
                                }
                            }
                            KeyCode::Char(c) => {
                                if app.focus == FocusArea::Proposal {
                                    app.command_draft.push(c);
                                }
                            }
                            KeyCode::Backspace => {
                                if app.focus == FocusArea::Proposal {
                                    app.command_draft.pop();
                                }
                            }
                            KeyCode::Esc => {
                                let should_quit = perform_footer_action(
                                    app,
                                    FooterAction::CancelEditCommand,
                                )
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
                        | AppState::PendingDryRun => {
                            // Non-interactive states
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
                                let new_scroll = ((rel as u32) * (app.output_max_scroll as u32)
                                    / denom) as u16;
                                app.output_scroll = new_scroll.min(app.output_max_scroll);
                                continue;
                            }
                        }

                        // Otherwise, treat it as a click on the button bar (if any) or focus change.
                        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
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
            } else {
                app.generated_command = None;
                app.command_draft.clear();
                app.dry_run_output = None;
                app.output_scroll = 0;
                app.selected_plugin = None;
                app.progress_rx = None;
                app.execution_result_rx = None;
                app.progress = None;
                app.focus = FocusArea::FooterButtons;
                app.footer_focus = 0;
                app.state = AppState::PendingRouting;
            }
        }
        FooterAction::ClearInput => {
            app.input.clear();
            app.focus = FocusArea::Proposal;
        }
        FooterAction::Submit => {
            if !app.input.trim().is_empty() {
                app.logs
                    .push(format!("Input submitted ({} chars)", app.input.len()));
                app.generated_command = None;
                app.command_draft.clear();
                app.dry_run_output = None;
                app.output_scroll = 0;
                app.selected_plugin = None;
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
            app.dry_run_output = None;
            app.output_scroll = 0;
            app.selected_plugin = None;
            app.focus = FocusArea::Proposal;
            app.footer_focus = 0;
        }
        FooterAction::EditCommand => {
            if let Some(cmd) = &app.generated_command {
                app.command_draft = cmd.clone();
            }
            app.state = AppState::EditingCommand;
            app.focus = FocusArea::Proposal;
            app.footer_focus = 0;
        }
        FooterAction::EditInput => {
            app.state = AppState::Input;
            app.generated_command = None;
            app.command_draft.clear();
            app.dry_run_output = None;
            app.output_scroll = 0;
            app.selected_plugin = None;
            app.focus = FocusArea::Proposal;
            app.footer_focus = 0;
        }
        FooterAction::Regenerate => {
            app.generated_command = None;
            app.command_draft.clear();
            app.dry_run_output = None;
            app.output_scroll = 0;
            app.focus = FocusArea::FooterButtons;
            app.footer_focus = 0;
            app.state = AppState::PendingGeneration;
        }
        FooterAction::PreviewEditedCommand => {
            let new_cmd = app.command_draft.trim().to_string();
            if !new_cmd.is_empty() {
                app.generated_command = Some(new_cmd.clone());
                app.logs.push(format!("Command edited: {}", new_cmd));
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
            app.state = AppState::AwaitingConfirmation;
            app.focus = FocusArea::FooterButtons;
            app.footer_focus = 0;
        }
        FooterAction::ResetToInput => {
            app.state = AppState::Input;
            app.generated_command = None;
            app.command_draft.clear();
            app.dry_run_output = None;
            app.output_scroll = 0;
            app.selected_plugin = None;
            app.focus = FocusArea::Proposal;
            app.footer_focus = 0;
        }
    }

    Ok(false)
}

fn ui(f: &mut Frame, app: &mut App) {
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // 1. Title/Header
            Constraint::Length(5), // 2. Proposal
            Constraint::Length(1), // 3. Buttons
            Constraint::Min(1),    // 4. Output
            Constraint::Length(2), // 5. Footer (MODE/MODEL/PROVIDER)
        ])
        .split(f.area());

    let block_style = app.theme.base_style;
    let border_style = app.theme.border_style;

    // --- SECTION 1: TITLE (HEADER) ---
    let header_text = Line::from(vec![
        Span::styled(" D E X T E R ", app.theme.header_title_style),
        Span::styled(
            " // AI COMMAND INTERFACE v0.1 ",
            app.theme.header_subtitle_style,
        ),
    ]);

    let header = Paragraph::new(header_text).style(block_style).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(" SYSTEM STATUS: ONLINE "),
    );
    f.render_widget(header, main_layout[0]);

    // --- SECTION 2: PROPOSAL (OR INPUT/INTENT) ---
    let (proposal_title, proposal_content) = match app.state {
        AppState::Input => {
            let mut lines = vec![Line::from("")];
            lines.extend(render_multiline_prompt(
                &app.input,
                Span::styled(" > ", app.theme.input_prompt_style),
                Span::styled("   ", app.theme.input_prompt_style),
                app.theme.input_text_style,
                Some(app.theme.input_cursor_style),
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
            lines.extend(render_multiline_prompt(
                &app.command_draft,
                Span::styled(" > ", app.theme.header_subtitle_style),
                Span::styled("   ", app.theme.header_subtitle_style),
                app.theme.proposal_cmd_style,
                Some(cmd_cursor),
            ));
            (" EDIT COMMAND ", lines)
        }
        AppState::Routing
        | AppState::Generating
        | AppState::PendingRouting
        | AppState::PendingGeneration => (
            " USER INTENT ",
            {
                let mut lines = vec![Line::from("")];
                lines.extend(render_multiline_prompt(
                    &app.input,
                    Span::styled(" > ", app.theme.header_subtitle_style),
                    Span::styled("   ", app.theme.header_subtitle_style),
                    app.theme.header_subtitle_style,
                    None,
                ));
                lines
            },
        ),
        _ => {
            if let Some(cmd) = &app.generated_command {
                (
                    " PROPOSAL ",
                    {
                        let mut lines = vec![Line::from("")];
                        lines.extend(render_multiline_prompt(
                            cmd,
                            Span::styled(" > ", app.theme.header_subtitle_style),
                            Span::styled("   ", app.theme.header_subtitle_style),
                            app.theme.proposal_cmd_style,
                            None,
                        ));
                        lines
                    },
                )
            } else {
                (
                    " USER INTENT (FAILED) ",
                    {
                        let mut lines = vec![Line::from("")];
                        lines.extend(render_multiline_prompt(
                            &app.input,
                            Span::styled(" > ", app.theme.error_style),
                            Span::styled("   ", app.theme.error_style),
                            app.theme.error_style,
                            None,
                        ));
                        lines
                    },
                )
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

    // Optional progress gauge (native ratatui widget) shown in the output box.
    let show_gauge = should_show_progress_gauge(app) && inner.height > 1;
    let (gauge_rect, text_region) = if show_gauge {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(inner);
        (Some(chunks[0]), chunks[1])
    } else {
        (None, inner)
    };
    app.output_viewport_height = text_region.height;

    if let Some(r) = gauge_rect {
        let (ratio, label, gauge_style) = build_progress_gauge(app);
        let gauge = Gauge::default()
            .ratio(ratio)
            .label(label)
            .style(app.theme.base_style)
            .gauge_style(gauge_style);
        f.render_widget(gauge, r);
    }

    let show_scrollbar =
        app.output_max_scroll > 0 && text_region.width > 1 && text_region.height > 0;
    app.output_scrollbar_rect = if show_scrollbar {
        Some(Rect {
            x: text_region.x + text_region.width - 1,
            y: text_region.y,
            width: 1,
            height: text_region.height,
        })
    } else {
        None
    };

    let mut text_area = text_region;
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
        // Render scrollbar only for the text region (so it doesn't overlap the progress gauge).
        f.render_stateful_widget(scrollbar, text_region, &mut scrollbar_state);
    }

    // --- SECTION 5: FOOTER (MODE/MODEL/PROVIDER) ---
    let state_name = format!("{:?}", app.state).to_uppercase();
    let provider_name = get_provider_name(&app.config);
    let footer_block = Block::default()
        .borders(Borders::TOP)
        .border_style(border_style);
    f.render_widget(&footer_block, main_layout[4]);
    let footer_inner = footer_block.inner(main_layout[4]);

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

    let mut display_texts: Vec<String> = Vec::with_capacity(specs.len());
    let mut actions: Vec<FooterAction> = Vec::with_capacity(specs.len());
    let mut constraints: Vec<Constraint> = Vec::with_capacity(specs.len());
    for (action, label) in specs.into_iter() {
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
    if let Some(ref url) = config.api_keys.base_url {
        if url.contains("googleapis.com") {
            "Gemini".to_string()
        } else if url.contains("deepseek.com") {
            "DeepSeek".to_string()
        } else {
            "Custom Endpoint".to_string()
        }
    } else if config.api_keys.gemini.is_some() {
        "Gemini".to_string()
    } else if config.api_keys.deepseek.is_some() {
        "DeepSeek".to_string()
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
) -> Vec<Line<'a>> {
    let mut out = Vec::new();
    let mut parts: Vec<&str> = text.split('\n').collect();
    if parts.is_empty() {
        parts.push("");
    }

    for (idx, line) in parts.iter().enumerate() {
        let is_last = idx == parts.len().saturating_sub(1);
        let prefix = if idx == 0 {
            first_prefix.clone()
        } else {
            continuation_prefix.clone()
        };

        let mut spans = vec![prefix, Span::styled(*line, text_style)];
        if is_last {
            if let Some(cursor_style) = cursor_style {
                spans.push(Span::styled("_", cursor_style));
            }
        }
        out.push(Line::from(spans));
    }
    out
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
    let mut text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Ready for instructions. Type your command above.",
            theme.header_subtitle_style,
        )),
        Line::from(""),
    ];

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

    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(format!(" {} ", char), theme.processing_spinner_style),
            Span::styled(format!("{}...", action), theme.processing_text_style),
        ]),
        Line::from(""),
    ];

    if let Some(prog) = &app.progress {
        lines.push(Line::from(vec![
            Span::styled(" STATUS: ", theme.header_subtitle_style),
            Span::styled(&prog.message, theme.processing_text_style),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            "Consulting neural pathways...",
            theme.header_subtitle_style,
        )));
    }

    lines
}

fn should_show_progress_gauge(app: &App) -> bool {
    matches!(
        app.state,
        AppState::Routing
            | AppState::Generating
            | AppState::Executing
            | AppState::DryRunning
            | AppState::PendingRouting
            | AppState::PendingGeneration
            | AppState::PendingDryRun
    )
}

fn build_progress_gauge<'a>(app: &'a App) -> (f64, Span<'a>, Style) {
    // Prefer a real percentage if the plugin provides one. Otherwise, show an animated bar.
    let (ratio, label) = match &app.progress {
        Some(p) => {
            if let Some(pct) = p.percentage {
                let r = (pct / 100.0).clamp(0.0, 1.0);
                (r, format!("{:.0}% {}", pct, p.message))
            } else {
                let r = ((app.tick_count % 100) as f64) / 100.0;
                (r, p.message.clone())
            }
        }
        None => {
            let r = ((app.tick_count % 100) as f64) / 100.0;
            let action = match app.state {
                AppState::Routing | AppState::PendingRouting => "Routing",
                AppState::Generating | AppState::PendingGeneration => "Generating",
                AppState::DryRunning | AppState::PendingDryRun => "Previewing",
                AppState::Executing => "Executing",
                _ => "Working",
            };
            (r, format!("{}...", action))
        }
    };

    // Use a strong filled style so it reads well across themes.
    let gauge_style = app.theme.footer_key_style;
    (ratio, Span::styled(label, app.theme.header_subtitle_style), gauge_style)
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
        .constraints([
            Constraint::Length(3), // 1. Title/Header
            Constraint::Length(5), // 2. Proposal
            Constraint::Length(1), // 3. Buttons
            Constraint::Min(1),    // 4. Output
            Constraint::Length(2), // 5. Footer
        ])
        .split(area);

    let output_inner_height = main_layout[3].height.saturating_sub(2); // subtract borders
    let gauge_height = if should_show_progress_gauge(app) && output_inner_height > 1 {
        1
    } else {
        0
    };
    let output_viewport_height = output_inner_height.saturating_sub(gauge_height);
    let (_, lines) = build_output_view(app);
    let line_count = lines.len() as u16;
    app.output_content_length = line_count;
    app.output_viewport_height = output_viewport_height;

    let max_scroll = line_count.saturating_sub(output_viewport_height);
    app.output_max_scroll = max_scroll;
    app.output_scroll = app.output_scroll.min(max_scroll);
    Ok(())
}

// --- Setup Wizard ---

#[derive(Debug, Clone, PartialEq)]
enum SetupState {
    Welcome,
    GeminiKey,
    DeepSeekKey,
    FetchingModels,
    ModelSelection,
    ThemeSelection,
    Confirm,
    Saving,
    Error(String),
}

struct SetupApp {
    state: SetupState,
    gemini_key: String,
    deepseek_key: String,
    available_models: Vec<String>,
    selected_model_idx: usize,
    available_themes: Vec<(&'static str, &'static str)>,
    selected_theme_idx: usize,
    config: Config,
    theme: Theme,
}

impl SetupApp {
    fn new(config: Config) -> Self {
        Self {
            state: SetupState::Welcome,
            gemini_key: String::new(),
            deepseek_key: String::new(),
            available_models: Vec::new(),
            selected_model_idx: 0,
            available_themes: vec![
                ("auto", "🔄 Auto (Follow system appearance)"),
                ("retro", "🌙 Retro (Classic amber CRT aesthetic)"),
                ("light", "☀️ Light (Clean blue/white for light terminals)"),
            ],
            selected_theme_idx: 0,
            theme: Theme::from_config(&config.theme),
            config,
        }
    }

    async fn fetch_available_models(&mut self) -> Result<()> {
        let mut all_models = Vec::new();
        let mut errors = Vec::new();

        // Fetch from Gemini if key provided
        if !self.gemini_key.trim().is_empty() {
            let client = dexter_core::LlmClient::new(
                self.gemini_key.clone(),
                "https://generativelanguage.googleapis.com/v1beta".to_string(),
                "gemini-flash".to_string(),
            );
            match client.list_models().await {
                Ok(models) => {
                    for m in models {
                        all_models.push(format!("{} (Google)", m));
                    }
                }
                Err(e) => {
                    errors.push(format!("Gemini Error: {}", e));
                }
            }
        }

        // Fetch from DeepSeek if key provided
        if !self.deepseek_key.trim().is_empty() {
            let client = dexter_core::LlmClient::new(
                self.deepseek_key.clone(),
                "https://api.deepseek.com/v1".to_string(),
                "deepseek-chat".to_string(),
            );
            match client.list_models().await {
                Ok(models) => {
                    for m in models {
                        all_models.push(format!("{} (DeepSeek)", m));
                    }
                }
                Err(e) => {
                    errors.push(format!("DeepSeek Error: {}", e));
                }
            }
        }

        if all_models.is_empty() {
            if !errors.is_empty() {
                // If we have errors and no models, fail loudly so user can see why
                return Err(anyhow::anyhow!(
                    "Model Discovery Failed:\n{}",
                    errors.join("\n")
                ));
            }

            // Only fallback if no keys provided or no errors (just empty lists?)
            all_models.push("gemini-2.5-flash (Fallback)".to_string());
            all_models.push("gemini-2.5-pro (Fallback)".to_string());
        }

        self.available_models = all_models;
        self.selected_model_idx = 0;
        Ok(())
    }

    async fn save_config(&mut self) -> Result<()> {
        if !self.gemini_key.is_empty() {
            self.config.api_keys.gemini = Some(self.gemini_key.clone());
        }
        if !self.deepseek_key.is_empty() {
            self.config.api_keys.deepseek = Some(self.deepseek_key.clone());
        }

        // Parse selected model
        let selection = &self.available_models[self.selected_model_idx];
        let model_id = selection
            .split_whitespace()
            .next()
            .unwrap_or("gemini-2.5-flash-lite")
            .to_string();

        self.config.models.router_model = model_id.clone();
        self.config.models.executor_model = model_id;

        // Save selected theme
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
    let mut app = SetupApp::new(config);

    loop {
        terminal.draw(|f| setup_ui(f, &app))?;

        if app.state == SetupState::FetchingModels {
            if let Err(e) = app.fetch_available_models().await {
                app.state = SetupState::Error(format!("Discovery error: {}", e));
            } else {
                app.state = SetupState::ModelSelection;
            }
            continue;
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match app.state {
                        SetupState::Welcome => match key.code {
                            KeyCode::Enter => app.state = SetupState::GeminiKey,
                            KeyCode::Esc => return Err(anyhow!("Setup aborted by user")),
                            _ => {}
                        },
                        SetupState::GeminiKey => match key.code {
                            KeyCode::Enter => app.state = SetupState::DeepSeekKey,
                            KeyCode::Char(c) => app.gemini_key.push(c),
                            KeyCode::Backspace => {
                                app.gemini_key.pop();
                            }
                            KeyCode::Esc => return Err(anyhow!("Setup aborted")),
                            _ => {}
                        },
                        SetupState::DeepSeekKey => match key.code {
                            KeyCode::Enter => {
                                app.state = SetupState::FetchingModels;
                            }
                            KeyCode::Char(c) => app.deepseek_key.push(c),
                            KeyCode::Backspace => {
                                app.deepseek_key.pop();
                            }
                            KeyCode::Esc => app.state = SetupState::GeminiKey,
                            _ => {}
                        },
                        SetupState::FetchingModels => {} // No input while fetching
                        SetupState::ModelSelection => match key.code {
                            KeyCode::Up | KeyCode::Left => {
                                if app.selected_model_idx > 0 {
                                    app.selected_model_idx -= 1;
                                }
                            }
                            KeyCode::Down | KeyCode::Right => {
                                if app.selected_model_idx
                                    < app.available_models.len().saturating_sub(1)
                                {
                                    app.selected_model_idx += 1;
                                }
                            }
                            KeyCode::Enter => app.state = SetupState::ThemeSelection,
                            KeyCode::Esc => app.state = SetupState::DeepSeekKey,
                            _ => {}
                        },
                        SetupState::ThemeSelection => match key.code {
                            KeyCode::Up | KeyCode::Left => {
                                if app.selected_theme_idx > 0 {
                                    app.selected_theme_idx -= 1;
                                    // Live preview
                                    let theme_id = app.available_themes[app.selected_theme_idx].0;
                                    app.theme = Theme::from_config(theme_id);
                                }
                            }
                            KeyCode::Down | KeyCode::Right => {
                                if app.selected_theme_idx
                                    < app.available_themes.len().saturating_sub(1)
                                {
                                    app.selected_theme_idx += 1;
                                    // Live preview
                                    let theme_id = app.available_themes[app.selected_theme_idx].0;
                                    app.theme = Theme::from_config(theme_id);
                                }
                            }
                            KeyCode::Enter => app.state = SetupState::Confirm,
                            KeyCode::Esc => app.state = SetupState::ModelSelection,
                            _ => {}
                        },
                        SetupState::Confirm => {
                            match key.code {
                                KeyCode::Enter | KeyCode::Char('y') => {
                                    app.state = SetupState::Saving;
                                    app.save_config().await?;
                                    return Ok(app.config);
                                }
                                KeyCode::Esc | KeyCode::Char('n') => {
                                    app.state = SetupState::ThemeSelection; // Go back
                                }
                                _ => {}
                            }
                        }
                        SetupState::Error(_) => {
                            if key.code == KeyCode::Enter || key.code == KeyCode::Esc {
                                // Allow user to go back to key entry instead of crashing
                                app.state = SetupState::GeminiKey;
                            }
                        }
                        _ => {}
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

    // Header
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

    // Content
    let content_text = match &app.state {
        SetupState::Welcome => vec![
            Line::from(""),
            Line::from(Span::styled(
                "WELCOME TO DEXTER",
                app.theme.header_title_style,
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Dexter Requires Access to Advanced Neural Networks to Function.",
                app.theme.header_subtitle_style,
            )),
            Line::from("We Will Guide You Through Setting Up Your API Keys."),
            Line::from(""),
            Line::from(Span::styled(
                "[PRESS ENTER TO BEGIN]",
                app.theme.input_cursor_style,
            )),
        ],
        SetupState::GeminiKey => vec![
            Line::from(Span::styled(
                "STEP 1: GEMINI API KEY",
                app.theme.header_title_style,
            )),
            Line::from(""),
            Line::from("Enter Your Google Gemini API Key:"),
            Line::from(""),
            Line::from(vec![
                Span::styled("> ", app.theme.input_prompt_style),
                Span::styled(
                    if app.gemini_key.is_empty() {
                        "_"
                    } else {
                        &app.gemini_key
                    },
                    app.theme.input_text_style,
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "(Press Enter to Leave Empty If You Only Have DeepSeek)",
                app.theme.header_subtitle_style,
            )),
        ],
        SetupState::DeepSeekKey => vec![
            Line::from(Span::styled(
                "STEP 2: DEEPSEEK API KEY",
                app.theme.header_title_style,
            )),
            Line::from(""),
            Line::from("Enter Your DeepSeek API Key:"),
            Line::from(""),
            Line::from(vec![
                Span::styled("> ", app.theme.input_prompt_style),
                Span::styled(
                    if app.deepseek_key.is_empty() {
                        "_"
                    } else {
                        &app.deepseek_key
                    },
                    app.theme.input_text_style,
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "(Press Enter to Leave Empty to Skip If You Only Have Gemini)",
                app.theme.header_subtitle_style,
            )),
        ],
        SetupState::FetchingModels => vec![
            Line::from(""),
            Line::from(Span::styled(
                "STEP 3: DISCOVERING MODELS",
                app.theme.header_title_style,
            )),
            Line::from(""),
            Line::from("Connecting to Provider APIs..."),
            Line::from("Fetching Latest Model List..."),
            Line::from(""),
            Line::from(Span::styled("[PLEASE WAIT]", app.theme.input_cursor_style)),
        ],
        SetupState::ModelSelection => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "STEP 3: SELECT PRIMARY MODEL",
                    app.theme.header_title_style,
                )),
                Line::from(""),
                Line::from("Select the AI Model to Power Dexter:"),
                Line::from(""),
            ];

            for (i, model) in app.available_models.iter().enumerate() {
                let style = if i == app.selected_model_idx {
                    app.theme.proposal_cmd_style
                } else {
                    app.theme.header_subtitle_style
                };
                let prefix = if i == app.selected_model_idx {
                    "> "
                } else {
                    "  "
                };
                lines.push(Line::from(Span::styled(
                    format!("{}{}", prefix, model),
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
        SetupState::ThemeSelection => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "STEP 4: SELECT THEME",
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
        SetupState::Confirm => vec![
            Line::from(Span::styled(
                "CONFIRM SETTINGS",
                app.theme.header_title_style,
            )),
            Line::from(""),
            Line::from(format!(
                "Gemini Key: {}",
                if app.gemini_key.is_empty() {
                    "NOT SET"
                } else {
                    "SET"
                }
            )),
            Line::from(format!(
                "DeepSeek Key: {}",
                if app.deepseek_key.is_empty() {
                    "NOT SET"
                } else {
                    "SET"
                }
            )),
            Line::from(format!(
                "Selected Model: {}",
                app.available_models
                    .get(app.selected_model_idx)
                    .unwrap_or(&"Unknown".to_string())
            )),
            Line::from(format!(
                "Theme: {}",
                app.available_themes[app.selected_theme_idx].1
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Save and Initialize? [Y/n]",
                app.theme.input_prompt_style,
            )),
        ],
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
