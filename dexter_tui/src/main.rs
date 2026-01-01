use anyhow::{Result, anyhow};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use dexter_core::{Config, ContextScanner, Executor, LlmClient, Router};
use dexter_plugins::{F2Plugin, FFmpegPlugin, Plugin, PreviewContent};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io::{stdout, Stdout};
use std::time::Duration;

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
    plugins: Vec<Box<dyn Plugin>>,
    selected_plugin: Option<String>,
    generated_command: Option<String>,
    logs: Vec<String>,
    tick_count: u64,
    current_context: Option<dexter_core::context::FileContext>,
    dry_run_output: Option<PreviewContent>,
    show_debug: bool,
}

impl App {
    fn new(config: Config) -> Self {
        // Prioritize specific API key, fallback to others if we implemented that logic.
        // For now, assuming Gemini as primary based on original code, but we should probably check others.
        // Simplified logic: Check Gemini -> OpenAI -> DeepSeek.
        let api_key = config.api_keys.gemini
            .or(config.api_keys.openai)
            .or(config.api_keys.deepseek)
            .unwrap_or_default();

        let base_url = config.api_keys.base_url.unwrap_or_else(|| 
            "https://generativelanguage.googleapis.com/v1beta/openai/".to_string()
        );
        
        let router_client = LlmClient::new(
            api_key.clone(), 
            base_url.clone(), 
            config.models.router_model
        );

        let executor_client = LlmClient::new(
            api_key, 
            base_url, 
            config.models.executor_model
        );
        
        Self {
            state: AppState::Input,
            input: String::new(),
            router: Router::new(router_client),
            executor: Executor::new(executor_client),
            plugins: vec![
                Box::new(F2Plugin),
                Box::new(FFmpegPlugin),
            ],
            selected_plugin: None,
            generated_command: None,
            logs: vec!["Dexter initialized. Ready for your command.".to_string()],
            tick_count: 0,
            current_context: None,
            dry_run_output: None,
            show_debug: false,
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
        
        match self.router.route(&self.input, &context, &self.plugins).await {
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
        let plugin = self.plugins.iter().find(|p| p.name() == *plugin_name).unwrap();
        let context = self.current_context.as_ref().unwrap();

        match self.executor.generate_command(&self.input, &context, plugin.as_ref()).await {
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
        let plugin = self.plugins.iter().find(|p| p.name() == *plugin_name)
            .ok_or_else(|| anyhow!("Plugin not found"))?;

        self.logs.push(format!("Executing preview: {}", cmd));
        match plugin.dry_run(cmd, Some(self.executor.llm_client())).await {
            Ok(output) => {
                self.logs.push("Preview data captured successfully".to_string());
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
            self.logs.push(format!("Executing [{}]: {}", plugin_name, cmd));
            
            // Record to history
            if let Err(e) = self.executor.record_history(&plugin_name, cmd).await {
                self.logs.push(format!("History log failed: {}", e));
            }
            
            let plugin = self.plugins.iter().find(|p| p.name() == plugin_name).unwrap();
            
            // Adjust command for actual execution if needed (e.g., adding -x for f2)
            let final_cmd = if plugin_name == "f2" && !cmd.contains(" -x") && !cmd.contains(" -X") {
                format!("{} -x", cmd)
            } else {
                cmd.clone()
            };

            match plugin.execute(&final_cmd).await {
                Ok(output) => {
                    self.state = AppState::Finished(output);
                    // Refresh context after execution
                    let _ = self.update_context().await;
                }
                Err(e) => {
                    self.state = AppState::Error(format!("Execution failed: {}", e));
                }
            }
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
                execute!(
                    terminal.backend_mut(),
                    LeaveAlternateScreen
                )?;
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
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err);
    }

    Ok(())
}

async fn run_app(terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
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
            _ => {}
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match app.state {
                        AppState::Input => match key.code {
                            KeyCode::Char('d') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                                app.show_debug = !app.show_debug;
                                app.logs.push(format!("Debug Mode: {}", if app.show_debug { "ON" } else { "OFF" }));
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
                        AppState::Routing | AppState::Generating | AppState::Executing | AppState::DryRunning |
                        AppState::PendingRouting | AppState::PendingGeneration | AppState::PendingDryRun => {
                            // Non-interactive states
                        }
                    }
                }
            }
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    // Retro Palette
    const AMBER: Color = Color::Rgb(255, 176, 0);       // Solid Classic Amber
    const AMBER_DIM: Color = Color::Rgb(150, 110, 0);   // Balanced Dim Amber
    const RED_ALERT: Color = Color::Rgb(255, 40, 40);   // Alert Red
    const CRT_BG: Color = Color::Black;                 // Solid Black

    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // 1. Title/Header
            Constraint::Length(5), // 2. Proposal
            Constraint::Min(1),    // 3. Output
            Constraint::Length(4), // 4. Footer
        ])
        .split(f.area());

    let block_style = Style::default().fg(AMBER).bg(CRT_BG);
    let border_style = Style::default().fg(AMBER_DIM);

    // --- SECTION 1: TITLE (HEADER) ---
    let header_text = Line::from(vec![
        Span::styled(" D E X T E R ", Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
        Span::styled(" // AI COMMAND INTERFACE v0.1 ", Style::default().fg(AMBER_DIM)),
    ]);
    
    let header = Paragraph::new(header_text)
        .style(block_style)
        .block(Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(" SYSTEM STATUS: ONLINE "));
    f.render_widget(header, main_layout[0]);

    // --- SECTION 2: PROPOSAL (OR INPUT/INTENT) ---
    let (proposal_title, proposal_content) = match app.state {
        AppState::Input => (
            " USER INPUT ",
            vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(" > ", Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
                    Span::styled(&app.input, Style::default().fg(Color::White)),
                    Span::styled("_", Style::default().fg(AMBER).add_modifier(Modifier::RAPID_BLINK)),
                ]),
            ]
        ),
        AppState::Routing | AppState::Generating | AppState::PendingRouting | AppState::PendingGeneration => (
            " USER INTENT ",
            vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(" > ", Style::default().fg(AMBER_DIM)),
                    Span::styled(&app.input, Style::default().fg(AMBER_DIM)),
                ]),
            ]
        ),
        _ => {
            if let Some(cmd) = &app.generated_command {
                (
                    " PROPOSAL ",
                    vec![
                        Line::from(""),
                        Line::from(vec![
                            Span::styled(" > ", Style::default().fg(AMBER_DIM)),
                            Span::styled(cmd, Style::default().bg(AMBER).fg(CRT_BG).add_modifier(Modifier::BOLD)),
                        ]),
                    ]
                )
            } else {
                (
                    " USER INTENT (FAILED) ",
                    vec![
                        Line::from(""),
                        Line::from(vec![
                            Span::styled(" > ", Style::default().fg(RED_ALERT)),
                            Span::styled(&app.input, Style::default().fg(RED_ALERT)),
                        ]),
                    ]
                )
            }
        }
    };

    let proposal_block = Paragraph::new(proposal_content)
        .block(Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(proposal_title, Style::default().fg(AMBER).add_modifier(Modifier::BOLD))));
    f.render_widget(proposal_block, main_layout[1]);

    // --- SECTION 3: OUTPUT (PREVIEW / LOGS / STATUS) ---
    let (output_title, output_content) = if app.show_debug {
        (" DEBUG_SYSTEM_INTERNAL ", render_debug(app, AMBER, AMBER_DIM, RED_ALERT))
    } else {
        match &app.state {
            AppState::Input => (" SYSTEM STATUS & LOGS ", render_input_view(app, AMBER, AMBER_DIM)),
            AppState::Routing | AppState::Generating | AppState::Executing | AppState::DryRunning |
            AppState::PendingRouting | AppState::PendingGeneration | AppState::PendingDryRun => 
                (" PROCESSING ", render_processing_view(app, AMBER, AMBER_DIM)),
            AppState::AwaitingConfirmation => (" PREVIEW / CONFIRMATION ", render_preview_view(app, AMBER, AMBER_DIM)),
            AppState::Finished(out) => (" EXECUTION RESULTS ", render_finished_view(out, app.selected_plugin.as_deref(), AMBER, AMBER_DIM)),
            AppState::Error(e) => (" SYSTEM FAILURE ", render_error_view(e, AMBER_DIM, RED_ALERT)),
        }
    };

    let output_block = Paragraph::new(output_content)
        .wrap(Wrap { trim: false })
        .block(Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(output_title, Style::default().fg(AMBER).add_modifier(Modifier::BOLD))));
    f.render_widget(output_block, main_layout[2]);

    // --- SECTION 4: FOOTER ---
    let state_name = format!("{:?}", app.state).to_uppercase();
    let footer_text = vec![
        Line::from(vec![
            Span::styled(" MODE: ", Style::default().fg(AMBER_DIM)),
            Span::styled(state_name, Style::default().fg(AMBER)),
            Span::styled(" | LATENCY: 24ms | MEM: 12MB", Style::default().fg(AMBER_DIM)),
        ]),
        Line::from(Span::styled(" Press [ESC] to Abort/Quit ", Style::default().fg(Color::Black).bg(AMBER))),
    ];
    
    let footer = Paragraph::new(footer_text)
        .style(block_style)
        .block(Block::default().borders(Borders::TOP).border_style(border_style));
    f.render_widget(footer, main_layout[3]);
}

// --- HELPER RENDERERS ---

fn render_debug<'a>(app: &'a App, amber: Color, amber_dim: Color, red: Color) -> Vec<Line<'a>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(" DEBUG_MODE: ", Style::default().fg(amber_dim)),
            Span::styled("ACTIVE", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled(" CWD: ", Style::default().fg(amber_dim)),
            Span::styled(format!("{}", std::env::current_dir().unwrap_or_default().display()), Style::default().fg(amber)),
        ]),
        Line::from(""),
    ];

    if let Some(ctx) = &app.current_context {
        for (i, f) in ctx.files.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:2}. ", i + 1), Style::default().fg(amber_dim)),
                Span::styled(f, Style::default().fg(amber)),
            ]));
        }
    } else {
        lines.push(Line::from(Span::styled("  (No Context Scanned)", Style::default().fg(red))));
    }
    lines
}

fn render_input_view<'a>(app: &'a App, _amber: Color, amber_dim: Color) -> Vec<Line<'a>> {
    let mut text = vec![
        Line::from(Span::styled("Ready for instructions. Type your command above.", Style::default().fg(amber_dim))),
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
           Span::styled(" CWD_CONTEXT: ", Style::default().fg(amber_dim)),
           Span::styled(format!("{}", std::env::current_dir().unwrap_or_default().display()), Style::default().fg(amber_dim)),
        ]));
        text.push(Line::from(vec![
           Span::styled(" FILES: ", Style::default().fg(amber_dim)),
           Span::styled(files_str, Style::default().fg(amber_dim)),
        ]));
        text.push(Line::from(""));
    }

    if !app.logs.is_empty() {
         text.push(Line::from(Span::styled("--- SYSTEM LOGS ---", Style::default().fg(amber_dim))));
         for log in app.logs.iter().rev().take(5) {
             text.push(Line::from(Span::styled(format!(":: {}", log), Style::default().fg(amber_dim))));
         }
    }
    text
}

fn render_processing_view<'a>(app: &'a App, amber: Color, amber_dim: Color) -> Vec<Line<'a>> {
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
    
    vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(format!(" {} ", char), Style::default().fg(amber).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{}...", action), Style::default().fg(amber)),
        ]),
        Line::from(""),
        Line::from(Span::styled("Consulting neural pathways...", Style::default().fg(amber_dim))),
    ]
}

fn render_preview_view<'a>(app: &'a App, amber: Color, amber_dim: Color) -> Vec<Line<'a>> {
    let mut lines = vec![];
    
    if let Some(preview) = &app.dry_run_output {
        match preview {
            PreviewContent::Text(t) => {
                for line in t.lines() {
                    lines.push(Line::from(Span::styled(line, Style::default().fg(amber))));
                }
            }
            PreviewContent::DiffList(diffs) => {
                if diffs.is_empty() {
                    lines.push(Line::from(Span::styled("No changes detected.", Style::default().fg(amber_dim))));
                } else {
                    for (i, diff) in diffs.iter().enumerate() {
                        lines.push(Line::from(vec![
                            Span::styled(format!("FILE [{:02}]: ", i+1), Style::default().fg(amber).add_modifier(Modifier::BOLD)),
                        ]));
                        lines.push(Line::from(vec![
                            Span::styled("  OLD: ", Style::default().fg(amber_dim)),
                            Span::styled(&diff.original, Style::default().fg(amber_dim)),
                        ]));
                        lines.push(Line::from(vec![
                            Span::styled("  NEW: ", Style::default().fg(amber)),
                            Span::styled(&diff.new, Style::default().fg(amber).add_modifier(Modifier::BOLD)),
                        ]));
                        lines.push(Line::from(""));
                    }
                }
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("CONFIRM EXECUTION? [", Style::default().fg(amber).add_modifier(Modifier::BOLD)),
        Span::styled("Y", Style::default().fg(amber).add_modifier(Modifier::BOLD)),
        Span::styled("/", Style::default().fg(amber).add_modifier(Modifier::BOLD)),
        Span::styled("N", Style::default().fg(amber).add_modifier(Modifier::BOLD)),
        Span::styled("]", Style::default().fg(amber).add_modifier(Modifier::BOLD)),
    ]));
    
    lines
}

fn render_finished_view<'a>(output: &'a str, plugin_name: Option<&'a str>, amber: Color, amber_dim: Color) -> Vec<Line<'a>> {
    let mut lines = vec![
        Line::from(Span::styled("EXECUTION COMPLETE.", Style::default().fg(Color::Green))),
        Line::from(""),
        Line::from(Span::styled("Target System Output:", Style::default().fg(amber_dim))),
    ];

    if plugin_name == Some("f2") {
        for line in output.lines() {
            if line.contains(" -> ") {
                let parts: Vec<&str> = line.split(" -> ").collect();
                if parts.len() == 2 {
                    lines.push(Line::from(vec![
                        Span::styled(parts[0], Style::default().fg(amber)),
                        Span::styled(" -> ", Style::default().fg(amber_dim)),
                        Span::styled(parts[1], Style::default().fg(Color::Green)),
                    ]));
                } else {
                    lines.push(Line::from(Span::styled(line, Style::default().fg(amber_dim))));
                }
            } else {
                lines.push(Line::from(Span::styled(line, Style::default().fg(amber_dim))));
            }
        }
    } else {
        lines.push(Line::from(output.to_string()));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("[PRESS ENTER TO RESET]", Style::default().fg(amber_dim))));
    lines
}

fn render_error_view<'a>(err: &'a str, amber_dim: Color, red: Color) -> Vec<Line<'a>> {
    vec![
        Line::from(Span::styled("!!! SYSTEM FAILURE !!!", Style::default().fg(red).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(Span::styled(err.to_string(), Style::default().fg(red))),
        Line::from(""),
        Line::from(Span::styled("[PRESS ENTER TO ACKNOWLEDGE]", Style::default().fg(amber_dim))),
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
    config: Config,
}

impl SetupApp {
    fn new(config: Config) -> Self {
        Self {
            state: SetupState::Welcome,
            gemini_key: String::new(),
            deepseek_key: String::new(),
            available_models: Vec::new(),
            selected_model_idx: 0,
            config,
        }
    }

    async fn fetch_available_models(&mut self) -> Result<()> {
        let mut all_models = Vec::new();

        // Fetch from Gemini if key provided
        if !self.gemini_key.trim().is_empty() {
             let client = dexter_core::LlmClient::new(
                 self.gemini_key.clone(),
                 "https://generativelanguage.googleapis.com/v1beta".to_string(),
                 "gemini-pro".to_string()
             );
             if let Ok(models) = client.list_models().await {
                 for m in models {
                     all_models.push(format!("{} (Google)", m));
                 }
             }
        }

        // Fetch from DeepSeek if key provided
        if !self.deepseek_key.trim().is_empty() {
            let client = dexter_core::LlmClient::new(
                self.deepseek_key.clone(),
                "https://api.deepseek.com/v1".to_string(),
                "deepseek-chat".to_string()
            );
            if let Ok(models) = client.list_models().await {
                for m in models {
                    all_models.push(format!("{} (DeepSeek)", m));
                }
            }
        }

        if all_models.is_empty() {
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
        let model_id = selection.split_whitespace().next().unwrap_or("gemini-2.0-flash-exp").to_string();

        self.config.models.router_model = model_id.clone();
        self.config.models.executor_model = model_id;

        self.config.save().await?;
        Ok(())
    }
}

async fn run_setup_wizard(terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>, config: Config) -> Result<Config> {
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
                        SetupState::Welcome => {
                            match key.code {
                                KeyCode::Enter => app.state = SetupState::GeminiKey,
                                KeyCode::Esc => return Err(anyhow!("Setup aborted by user")),
                                _ => {}
                            }
                        }
                        SetupState::GeminiKey => {
                            match key.code {
                                KeyCode::Enter => app.state = SetupState::DeepSeekKey,
                                KeyCode::Char(c) => app.gemini_key.push(c),
                                KeyCode::Backspace => { app.gemini_key.pop(); },
                                KeyCode::Esc => return Err(anyhow!("Setup aborted")),
                                _ => {}
                            }
                        }
                        SetupState::DeepSeekKey => {
                            match key.code {
                                KeyCode::Enter => {
                                    app.state = SetupState::FetchingModels;
                                }
                                KeyCode::Char(c) => app.deepseek_key.push(c),
                                KeyCode::Backspace => { app.deepseek_key.pop(); },
                                KeyCode::Esc => app.state = SetupState::GeminiKey,
                                _ => {}
                            }
                        }
                        SetupState::FetchingModels => {} // No input while fetching
                        SetupState::ModelSelection => {
                            match key.code {
                                KeyCode::Up | KeyCode::Left => {
                                     if app.selected_model_idx > 0 {
                                         app.selected_model_idx -= 1;
                                     }
                                }
                                KeyCode::Down | KeyCode::Right => {
                                    if app.selected_model_idx < app.available_models.len().saturating_sub(1) {
                                        app.selected_model_idx += 1;
                                    }
                                }
                                KeyCode::Enter => app.state = SetupState::Confirm,
                                KeyCode::Esc => app.state = SetupState::DeepSeekKey,
                                _ => {}
                            }
                        }
                        SetupState::Confirm => {
                            match key.code {
                                KeyCode::Enter | KeyCode::Char('y') => {
                                    app.state = SetupState::Saving;
                                    app.save_config().await?;
                                    return Ok(app.config);
                                }
                                KeyCode::Esc | KeyCode::Char('n') => {
                                    app.state = SetupState::ModelSelection; // Go back
                                }
                                _ => {}
                            }
                        }
                         SetupState::Error(_) => {
                            if key.code == KeyCode::Enter || key.code == KeyCode::Esc {
                                return Err(anyhow!("Setup failed"));
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
    const AMBER: Color = Color::Rgb(255, 176, 0);
    const AMBER_DIM: Color = Color::Rgb(128, 88, 0);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(f.area());

    // Header
    let header = Paragraph::new(Span::styled(" D E X T E R  //  INITIALIZATION ", Style::default().fg(AMBER).add_modifier(Modifier::BOLD)))
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(AMBER_DIM)));
    f.render_widget(header, chunks[0]);

    // Content
    let content_text = match &app.state {
        SetupState::Welcome => vec![
            Line::from(""),
            Line::from(Span::styled("WELCOME TO DEXTER", Style::default().fg(AMBER).add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from(Span::styled("Dexter requires access to advanced neural networks to function.", Style::default().fg(AMBER_DIM))),
            Line::from("We will guide you through setting up your API keys."),
            Line::from(""),
            Line::from(Span::styled("[PRESS ENTER TO BEGIN]", Style::default().fg(AMBER).add_modifier(Modifier::RAPID_BLINK))),
        ],
        SetupState::GeminiKey => vec![
            Line::from(Span::styled("STEP 1: GEMINI API KEY", Style::default().fg(AMBER))),
            Line::from(""),
            Line::from("Enter your Google Gemini API Key:"),
            Line::from(""),
            Line::from(vec![
                Span::styled("> ", Style::default().fg(AMBER)),
                Span::styled(if app.gemini_key.is_empty() { "_" } else { &app.gemini_key }, Style::default().fg(Color::White)),
            ]),
            Line::from(""),
            Line::from(Span::styled("(Leave empty to skip)", Style::default().fg(AMBER_DIM))),
        ],
        SetupState::DeepSeekKey => vec![
            Line::from(Span::styled("STEP 2: DEEPSEEK API KEY", Style::default().fg(AMBER))),
            Line::from(""),
            Line::from("Enter your DeepSeek API Key:"),
            Line::from(""),
            Line::from(vec![
                Span::styled("> ", Style::default().fg(AMBER)),
                Span::styled(if app.deepseek_key.is_empty() { "_" } else { &app.deepseek_key }, Style::default().fg(Color::White)),
            ]),
            Line::from(""),
            Line::from(Span::styled("(Leave empty to skip if you set Gemini)", Style::default().fg(AMBER_DIM))),
        ],
        SetupState::FetchingModels => vec![
            Line::from(""),
            Line::from(Span::styled("STEP 3: DISCOVERING MODELS", Style::default().fg(AMBER))),
            Line::from(""),
            Line::from("Connecting to provider APIs..."),
            Line::from("Fetching latest model list..."),
            Line::from(""),
            Line::from(Span::styled("[PLEASE WAIT]", Style::default().fg(AMBER).add_modifier(Modifier::RAPID_BLINK))),
        ],
        SetupState::ModelSelection => {
            let mut lines = vec![
                Line::from(Span::styled("STEP 3: SELECT PRIMARY MODEL", Style::default().fg(AMBER))),
                Line::from(""),
                Line::from("Select the AI model to power Dexter:"),
                Line::from(""),
            ];
            
            for (i, model) in app.available_models.iter().enumerate() {
                let style = if i == app.selected_model_idx {
                    Style::default().fg(Color::Black).bg(AMBER)
                } else {
                    Style::default().fg(AMBER_DIM)
                };
                let prefix = if i == app.selected_model_idx { "> " } else { "  " };
                lines.push(Line::from(Span::styled(format!("{}{}", prefix, model), style)));
            }
            
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("(Use Arrow Keys to select, ENTER to confirm)", Style::default().fg(AMBER_DIM))));
            lines
        }
        SetupState::Confirm => vec![
            Line::from(Span::styled("CONFIRM SETTINGS", Style::default().fg(AMBER))),
            Line::from(""),
            Line::from(format!("Gemini Key: {}", if app.gemini_key.is_empty() { "NOT SET" } else { "SET" })),
            Line::from(format!("DeepSeek Key: {}", if app.deepseek_key.is_empty() { "NOT SET" } else { "SET" })),
            Line::from(format!("Selected Model: {}", app.available_models.get(app.selected_model_idx).unwrap_or(&"Unknown".to_string()))),
            Line::from(""),
            Line::from(Span::styled("Save and Initialize? [Y/n]", Style::default().fg(AMBER).add_modifier(Modifier::BOLD))),
        ],
        SetupState::Saving => vec![
            Line::from(""),
            Line::from(Span::styled("SAVING CONFIGURATION...", Style::default().fg(AMBER).add_modifier(Modifier::RAPID_BLINK))),
        ],
        SetupState::Error(e) => vec![
            Line::from(Span::styled("ERROR", Style::default().fg(Color::Red))),
            Line::from(e.clone()),
        ],
    };

    let content = Paragraph::new(content_text)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(AMBER_DIM)).title(" SETUP WIZARD "))
        .style(Style::default().fg(AMBER))
        .wrap(Wrap { trim: true });
    f.render_widget(content, chunks[1]);
}
