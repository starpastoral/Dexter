use anyhow::{Result, anyhow};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use dexter_core::{Config, ContextScanner, Executor, LlmClient, Router};
use dexter_plugins::{F2Plugin, FFmpegPlugin, Plugin};
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
    dry_run_output: Option<String>,
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
                self.logs.push(format!("Preview data captured ({} bytes)", output.len()));
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

    // Check for API Keys
    if !config.has_keys() {
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
    // Retro Palette - True Monochrome CRT
    const AMBER: Color = Color::Rgb(255, 176, 0);       // Solid Classic Amber
    const AMBER_DIM: Color = Color::Rgb(150, 110, 0);   // Balanced Dim Amber
    const RED_ALERT: Color = Color::Rgb(255, 40, 40);   // Alert Red
    const CRT_BG: Color = Color::Black;                 // Solid Black

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(4), // Taller footer using blocks
        ])
        .split(f.area());

    // Header
    let block_style = Style::default().fg(AMBER).bg(CRT_BG);
    let border_style = Style::default().fg(AMBER_DIM);

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
    f.render_widget(header, chunks[0]);

    let content_block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(if app.show_debug { " DEBUG_SYSTEM_INTERNAL " } else { " TERMINAL OUTPUT " }, Style::default().fg(AMBER)));

    let content_text = if app.show_debug {
        let mut debug_lines = vec![
            Line::from(vec![
                Span::styled(" DEBUG_MODE: ", Style::default().fg(AMBER_DIM)),
                Span::styled("ACTIVE", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled(" CWD: ", Style::default().fg(AMBER_DIM)),
                Span::styled(format!("{}", std::env::current_dir().unwrap_or_default().display()), Style::default().fg(AMBER)),
            ]),
            Line::from(""),
            Line::from(Span::styled(" RAW_FILE_CONTEXT (HEX INSPECTION):", Style::default().fg(AMBER_DIM))),
        ];

        if let Some(ctx) = &app.current_context {
            for (i, f) in ctx.files.iter().enumerate() {
                let hex: String = f.chars()
                    .map(|c| format!("\\u{{{:04X}}}", c as u32))
                    .collect::<Vec<_>>()
                    .join("");
                debug_lines.push(Line::from(vec![
                    Span::styled(format!("  {:2}. ", i + 1), Style::default().fg(AMBER_DIM)),
                    Span::styled(f, Style::default().fg(AMBER)),
                    Span::styled(format!("  [{}]", hex), Style::default().fg(Color::DarkGray)),
                ]));
            }
        } else {
            debug_lines.push(Line::from(Span::styled("  (No Context Scanned)", Style::default().fg(RED_ALERT))));
        }
        
        debug_lines.push(Line::from(""));
        debug_lines.push(Line::from(Span::styled(" Press Ctrl-D to exit Debug Mode", Style::default().fg(AMBER_DIM))));
        debug_lines
    } else {
        match &app.state {
        AppState::Input => {
            let mut text = vec![
                Line::from(Span::styled("Awaiting parameters...", Style::default().fg(AMBER_DIM))),
                Line::from(""),
            ];

            // Display Current Context
            if let Some(ctx) = &app.current_context {
                text.push(Line::from(vec![
                    Span::styled(" CURRENT_DIRECTORY: ", Style::default().fg(AMBER_DIM)),
                    Span::styled(format!("{}", std::env::current_dir().unwrap_or_default().display()), Style::default().fg(AMBER)),
                ]));
                
                let files_str = if ctx.files.is_empty() {
                    " (Empty)".to_string()
                } else if ctx.files.len() > 5 {
                    format!("{} files (Top 5: {})", ctx.files.len(), ctx.files.iter().take(5).cloned().collect::<Vec<_>>().join(", "))
                } else {
                    ctx.files.join(", ")
                };
                
                text.push(Line::from(vec![
                   Span::styled(" FILES: ", Style::default().fg(AMBER_DIM)),
                   Span::styled(files_str, Style::default().fg(AMBER_DIM)),
                ]));

                if let Some(summary) = &ctx.summary {
                    text.push(Line::from(Span::styled(format!(" Note: {}", summary), Style::default().fg(AMBER_DIM))));
                }
                text.push(Line::from(""));
            }

            text.push(Line::from(vec![
                Span::styled("USER_INPUT > ", Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
                Span::styled(&app.input, Style::default().fg(Color::White)),
                Span::styled("_", Style::default().fg(AMBER).add_modifier(Modifier::RAPID_BLINK)), // Blinking cursor
            ]));
            
            if !app.logs.is_empty() {
                 text.push(Line::from(""));
                 text.push(Line::from(Span::styled("--- SYSTEM LOGS ---", Style::default().fg(AMBER_DIM))));
                 for log in app.logs.iter().rev().take(5) {
                     text.push(Line::from(Span::styled(format!(":: {}", log), Style::default().fg(AMBER_DIM))));
                 }
            }
            text
        }
        AppState::Routing | AppState::Generating | AppState::Executing | AppState::DryRunning |
        AppState::PendingRouting | AppState::PendingGeneration | AppState::PendingDryRun => {
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
                    Span::styled(format!(" {} ", char), Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("{}...", action), Style::default().fg(AMBER)),
                ]),
                Line::from(""),
                Line::from(Span::styled("Consulting neural pathways...", Style::default().fg(AMBER_DIM))),
            ]
        }
        AppState::AwaitingConfirmation => {
            let cmd = app.generated_command.as_ref().unwrap();
            
            // Nested layout for split-box design
            let confirmation_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(6), // Proposal box
                    Constraint::Min(1),    // Preview box
                ])
                .split(f.area()); // This is a bit tricky since we're inside a function that returns lines.
                                 // I'll refactor the ui function to handle multi-block rendering for this state.
            
            // Re-alignment: I will use a different approach. Instead of returning lines, 
            // I'll render the blocks directly here and return an empty vec for the caller.
            
            // 1. Proposal Block
            let proposal_text = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(format!(" {} ", cmd), Style::default().bg(AMBER).fg(CRT_BG)),
                ]),
            ];
            let proposal_block = Paragraph::new(proposal_text)
                .block(Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .title(Span::styled(" PROPOSAL ", Style::default().fg(AMBER).add_modifier(Modifier::BOLD))));
            f.render_widget(proposal_block, confirmation_chunks[0]);

            // 2. Preview Block
            let mut preview_lines = vec![];
            if let Some(preview) = &app.dry_run_output {
                if preview.trim().is_empty() {
                    preview_lines.push(Line::from(Span::styled("No changes or matches detected.", Style::default().fg(AMBER_DIM))));
                } else {
                    let mut file_idx = 1;
                    for line in preview.lines() {
                        let trimmed = line.trim();
                        if trimmed.is_empty() { continue; }
                        
                        // Strip table noise and "Dry run" info
                        if (trimmed.starts_with('*') || trimmed.starts_with('-') || trimmed.starts_with('+')) 
                            && (trimmed.contains("---") || trimmed.contains("***") || trimmed.len() > 10) {
                            continue;
                        }
                        if trimmed.contains("模拟运行") { continue; }

                        if trimmed.contains('|') {
                            let parts: Vec<&str> = trimmed.split('|')
                                .map(|s| s.trim())
                                .filter(|s| !s.is_empty())
                                .collect();
                            
                            if parts.len() >= 2 {
                                let old_name = parts[0];
                                let new_name = parts[1];
                                
                                let old_lower = old_name.to_lowercase();
                                if old_lower.contains("original") || old_lower.contains("filename") ||
                                   old_lower.contains("文件名") || old_lower.contains("原始") {
                                    continue;
                                }
                                
                                preview_lines.push(Line::from(vec![
                                    Span::styled(format!("FILE [{:02}]: ", file_idx), Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
                                ]));
                                preview_lines.push(Line::from(vec![
                                    Span::styled("  OLD: ", Style::default().fg(AMBER_DIM)),
                                    Span::styled(old_name.to_string(), Style::default().fg(AMBER_DIM)),
                                ]));
                                preview_lines.push(Line::from(vec![
                                    Span::styled("  NEW: ", Style::default().fg(AMBER)),
                                    Span::styled(new_name.to_string(), Style::default().fg(AMBER)),
                                ]));
                                preview_lines.push(Line::from("")); 
                                file_idx += 1;
                                continue;
                            }
                        }

                        if trimmed.to_lowercase().contains("success") || trimmed.contains("OK") {
                            preview_lines.push(Line::from(Span::styled(format!("> {}", trimmed), Style::default().fg(AMBER))));
                        }
                    }
                }
            }

            preview_lines.push(Line::from(""));
            preview_lines.push(Line::from(vec![
                Span::styled("CONFIRM EXECUTION? [", Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
                Span::styled("Y", Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
                Span::styled("/", Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
                Span::styled("N", Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
                Span::styled("]", Style::default().fg(AMBER).add_modifier(Modifier::BOLD)),
            ]));

            let preview_block = Paragraph::new(preview_lines)
                .block(Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .title(Span::styled(" PREVIEW ", Style::default().fg(AMBER).add_modifier(Modifier::BOLD))));
            f.render_widget(preview_block, confirmation_chunks[1]);

            // Return empty lines as we rendered directly
            return;
        }
        AppState::Finished(output) => {
            let mut lines = vec![
                Line::from(Span::styled("EXECUTION COMPLETE.", Style::default().fg(Color::Green))),
                Line::from(""),
                Line::from(Span::styled("Target System Output:", Style::default().fg(AMBER_DIM))),
            ];

            // Pretty print f2 output if possible
            if app.selected_plugin.as_deref() == Some("f2") {
                for line in output.lines() {
                    if line.contains(" -> ") {
                        let parts: Vec<&str> = line.split(" -> ").collect();
                        if parts.len() == 2 {
                            lines.push(Line::from(vec![
                                Span::styled(parts[0], Style::default().fg(AMBER)),
                                Span::styled(" -> ", Style::default().fg(AMBER_DIM)),
                                Span::styled(parts[1], Style::default().fg(Color::Green)),
                            ]));
                        } else {
                            lines.push(Line::from(Span::styled(line, Style::default().fg(AMBER_DIM))));
                        }
                    } else {
                        lines.push(Line::from(Span::styled(line, Style::default().fg(AMBER_DIM))));
                    }
                }
            } else {
                lines.push(Line::from(output.clone()));
            }

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("[PRESS ENTER TO RESET]", Style::default().fg(AMBER_DIM))));
            lines
        }
        AppState::Error(e) => vec![
            Line::from(Span::styled("!!! SYSTEM FAILURE !!!", Style::default().fg(RED_ALERT).add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from(Span::styled(e.clone(), Style::default().fg(RED_ALERT))),
            Line::from(""),
            Line::from(Span::styled("[PRESS ENTER TO ACKNOWLEDGE]", Style::default().fg(AMBER_DIM))),
        ],
    }
    };

    let content = Paragraph::new(content_text)
        .block(content_block)
        .style(Style::default().fg(AMBER))
        .wrap(Wrap { trim: true });
    f.render_widget(content, chunks[1]);

    // Footer
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
    f.render_widget(footer, chunks[2]);
}

// --- Setup Wizard ---

#[derive(Debug, Clone, PartialEq)]
enum SetupState {
    Welcome,
    GeminiKey,
    DeepSeekKey,
    ModelSelection,
    Confirm,
    Saving,
    #[allow(dead_code)]
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

    fn populate_models(&mut self) {
        self.available_models.clear();
        if !self.gemini_key.is_empty() {
            self.available_models.push("gemini-2.0-flash-exp (Google)".to_string());
            self.available_models.push("gemini-1.5-pro (Google)".to_string());
        }
        if !self.deepseek_key.is_empty() {
            self.available_models.push("deepseek-chat (DeepSeek)".to_string());
            self.available_models.push("deepseek-coder (DeepSeek)".to_string());
        }
        // Fallback default if nothing entered (though unlikely to work without keys)
        if self.available_models.is_empty() {
            self.available_models.push("gemini-2.0-flash-exp (Default)".to_string());
        }
        self.selected_model_idx = 0;
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
                                    app.populate_models();
                                    app.state = SetupState::ModelSelection;
                                }
                                KeyCode::Char(c) => app.deepseek_key.push(c),
                                KeyCode::Backspace => { app.deepseek_key.pop(); },
                                KeyCode::Esc => app.state = SetupState::GeminiKey, // Go back
                                _ => {}
                            }
                        }
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
