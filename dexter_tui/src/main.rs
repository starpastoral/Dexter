use anyhow::{anyhow, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use dexter_core::{Config, ContextScanner, Executor, LlmClient, Router};
use dexter_plugins::{F2Plugin, FFmpegPlugin, Plugin, PreviewContent, YtDlpPlugin};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
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
    Executing,
    Finished(String),
    Error(String),
    // New states for non-blocking execution flow
    PendingRouting,
    PendingGeneration,
    PendingDryRun,
    DryRunning,
}

struct App {
    state: AppState,
    input: String,
    router: Router,
    executor: Executor,
    plugins: Vec<Arc<dyn Plugin>>,
    selected_plugin: Option<String>,
    generated_command: Option<String>,
    logs: Vec<String>,
    tick_count: u64,
    current_context: Option<dexter_core::context::FileContext>,
    dry_run_output: Option<PreviewContent>,
    show_debug: bool,
    config: Config,
    theme: Theme,

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
            logs: vec!["Dexter initialized. Ready for your command.".to_string()],
            tick_count: 0,
            current_context: None,
            dry_run_output: None,
            show_debug: false,
            config,
            theme,
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
                self.logs.push(format!("Generated command: {}", cmd));
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

        self.logs.push(format!("Executing preview: {}", cmd));
        match plugin.dry_run(cmd, Some(self.executor.llm_client())).await {
            Ok(output) => {
                self.logs
                    .push("Preview data captured successfully".to_string());
                self.dry_run_output = Some(output);
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

            self.state = AppState::Executing;
            self.logs
                .push(format!("Executing [{}]: {}", plugin_name, cmd));
            // Record to history
            if let Err(e) = self.executor.record_history(&plugin_name, cmd).await {
                self.logs.push(format!("History log failed: {}", e));
            }

            let plugin = self
                .plugins
                .iter()
                .find(|p| p.name() == plugin_name)
                .unwrap()
                .clone();

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
        terminal.draw(|f| ui(f, app))?;
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

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match app.state {
                        AppState::Input => match key.code {
                            KeyCode::Char('d')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                app.show_debug = !app.show_debug;
                                app.logs.push(format!(
                                    "Debug Mode: {}",
                                    if app.show_debug { "ON" } else { "OFF" }
                                ));
                            }
                            KeyCode::Enter => {
                                if !app.input.is_empty() {
                                    app.logs.push(format!("Input: '{}'", app.input));
                                    app.state = AppState::PendingRouting;
                                }
                            }
                            KeyCode::Char(c) => {
                                app.input.push(c);
                            }
                            KeyCode::Backspace => {
                                app.input.pop();
                            }
                            KeyCode::Esc => return Ok(()),
                            _ => {}
                        },
                        AppState::AwaitingConfirmation => match key.code {
                            KeyCode::Char('y') | KeyCode::Enter => {
                                app.execute_command().await?;
                            }
                            KeyCode::Char('n') | KeyCode::Esc => {
                                app.state = AppState::Input;
                                app.input.clear();
                                app.generated_command = None;
                            }
                            _ => {}
                        },
                        AppState::Finished(_) | AppState::Error(_) => match key.code {
                            KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ') => {
                                app.state = AppState::Input;
                                app.input.clear();
                                app.generated_command = None;
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
            }
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // 1. Title/Header
            Constraint::Length(5), // 2. Proposal
            Constraint::Min(1),    // 3. Output
            Constraint::Length(4), // 4. Footer
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
        AppState::Input => (
            " USER INPUT ",
            vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(" > ", app.theme.input_prompt_style),
                    Span::styled(&app.input, app.theme.input_text_style),
                    Span::styled("_", app.theme.input_cursor_style),
                ]),
            ],
        ),
        AppState::Routing
        | AppState::Generating
        | AppState::PendingRouting
        | AppState::PendingGeneration => (
            " USER INTENT ",
            vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(" > ", app.theme.header_subtitle_style),
                    Span::styled(&app.input, app.theme.header_subtitle_style),
                ]),
            ],
        ),
        _ => {
            if let Some(cmd) = &app.generated_command {
                (
                    " PROPOSAL ",
                    vec![
                        Line::from(""),
                        Line::from(vec![
                            Span::styled(" > ", app.theme.header_subtitle_style),
                            Span::styled(cmd, app.theme.proposal_cmd_style),
                        ]),
                    ],
                )
            } else {
                (
                    " USER INTENT (FAILED) ",
                    vec![
                        Line::from(""),
                        Line::from(vec![
                            Span::styled(" > ", app.theme.error_style),
                            Span::styled(&app.input, app.theme.error_style),
                        ]),
                    ],
                )
            }
        }
    };

    let proposal_block = Paragraph::new(proposal_content).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(proposal_title, app.theme.header_title_style)),
    );
    f.render_widget(proposal_block, main_layout[1]);

    // --- SECTION 3: OUTPUT (PREVIEW / LOGS / STATUS) ---
    let (output_title, output_content) = if app.show_debug {
        (" DEBUG_SYSTEM_INTERNAL ", render_debug(app, &app.theme))
    } else {
        match &app.state {
            AppState::Input => (" SYSTEM STATUS & LOGS ", render_input_view(app, &app.theme)),
            AppState::Routing
            | AppState::Generating
            | AppState::Executing
            | AppState::DryRunning
            | AppState::PendingRouting
            | AppState::PendingGeneration
            | AppState::PendingDryRun => (" PROCESSING ", render_processing_view(app, &app.theme)),
            AppState::AwaitingConfirmation => (
                " PREVIEW / CONFIRMATION ",
                render_preview_view(app, &app.theme),
            ),
            AppState::Finished(out) => (
                " EXECUTION RESULTS ",
                render_finished_view(out, app.selected_plugin.as_deref(), &app.theme),
            ),
            AppState::Error(e) => (" SYSTEM FAILURE ", render_error_view(e, &app.theme)),
        }
    };

    let output_block = Paragraph::new(output_content)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(Span::styled(output_title, app.theme.header_title_style)),
        );
    f.render_widget(output_block, main_layout[2]);

    // --- SECTION 4: FOOTER ---
    let state_name = format!("{:?}", app.state).to_uppercase();
    let footer_width = main_layout[3].width.saturating_sub(2) as usize; // Sub borders

    // Line 1: Mode (Left) + Models (Right)
    let left_text_1 = format!(" MODE: {}", state_name);
    let right_text_1 = format!("MODEL: {} ", app.config.models.executor_model);
    let padding_1 = footer_width.saturating_sub(left_text_1.len() + right_text_1.len());

    let line1 = Line::from(vec![
        Span::styled(" MODE: ", app.theme.footer_text_style),
        Span::styled(state_name, app.theme.footer_highlight_style),
        Span::styled(" ".repeat(padding_1), Style::default()),
        Span::styled("MODEL: ", app.theme.footer_text_style),
        Span::styled(
            &app.config.models.executor_model,
            app.theme.footer_highlight_style,
        ),
        Span::styled(" ", Style::default()),
    ]);

    // Line 2: Quit (Left) + Provider (Right)
    let left_text_2 = " Press [ESC] to Abort/Quit ";
    let provider_name = get_provider_name(&app.config);
    let right_text_2 = format!(" PROVIDER: {} ", provider_name);
    let padding_2 = footer_width.saturating_sub(left_text_2.len() + right_text_2.len());

    let line2 = Line::from(vec![
        Span::styled(left_text_2, app.theme.footer_key_style),
        Span::styled(" ".repeat(padding_2), Style::default()),
        Span::styled(" PROVIDER: ", app.theme.footer_text_style),
        Span::styled(provider_name, app.theme.footer_highlight_style),
        Span::styled(" ", Style::default()),
    ]);

    let footer_text = vec![line1, line2];

    let footer = Paragraph::new(footer_text).style(block_style).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(border_style),
    );
    f.render_widget(footer, main_layout[3]);
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

// --- HELPER RENDERERS ---

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

        if let Some(pct) = prog.percentage {
            // Simple text-based bar if we have percentage
            let width: usize = 40;
            let filled = (pct / 100.0 * width as f64) as usize;
            let bar = format!(
                "[{}{}] {:.1}%",
                "=".repeat(filled),
                "-".repeat(width.saturating_sub(filled)),
                pct
            );
            lines.push(Line::from(Span::styled(bar, theme.processing_text_style)));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "Consulting neural pathways...",
            theme.header_subtitle_style,
        )));
    }

    lines
}

fn render_preview_view<'a>(app: &'a App, theme: &Theme) -> Vec<Line<'a>> {
    let mut lines = vec![Line::from("")]; // NEW: Start with an empty line

    if let Some(preview) = &app.dry_run_output {
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
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("CONFIRM EXECUTION? [", theme.input_prompt_style),
        Span::styled("Y", theme.input_prompt_style),
        Span::styled("/", theme.input_prompt_style),
        Span::styled("N", theme.input_prompt_style),
        Span::styled("]", theme.input_prompt_style),
    ]));

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
        lines.push(Line::from(output.to_string()));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "[PRESS ENTER TO RESET]",
        theme.header_subtitle_style,
    )));
    lines
}

fn render_error_view<'a>(err: &'a str, theme: &Theme) -> Vec<Line<'a>> {
    vec![
        Line::from(Span::styled(
            "!!! SYSTEM FAILURE !!!",
            theme.error_style.add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(err.to_string(), theme.error_style)),
        Line::from(""),
        Line::from(Span::styled(
            "[PRESS ENTER TO ACKNOWLEDGE]",
            theme.header_subtitle_style,
        )),
    ]
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
