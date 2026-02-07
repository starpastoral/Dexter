use anyhow::{anyhow, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::Stdout;
use std::time::Duration;

use dexter_core::Config;

use crate::setup::state::{SetupApp, SetupState};
use crate::setup::view::setup_ui;

pub async fn run_setup_wizard(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    config: Config,
) -> Result<Config> {
    run_setup_flow(terminal, config, true).await
}

pub async fn run_settings_panel(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    config: Config,
) -> Result<Config> {
    run_setup_flow(terminal, config, false).await
}

pub async fn run_setup_flow(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    config: Config,
    show_welcome: bool,
) -> Result<Config> {
    let mut app = SetupApp::new(config, show_welcome);
    app.refresh_runtime_statuses().await;

    loop {
        if app.dirty {
            terminal.draw(|f| setup_ui(f, &app))?;
            app.dirty = false;
        }

        if app.state == SetupState::FetchingProviderModels {
            if let Err(e) = app.fetch_models_for_current_provider().await {
                app.state = SetupState::Error(format!("Model discovery failed: {}", e));
            } else {
                app.state = SetupState::ProviderModelSelection;
            }
            app.dirty = true;
            continue;
        }

        let poll_ms = 220;
        if event::poll(Duration::from_millis(poll_ms))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.dirty = true;
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
                                app.dirty = true;
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
                                app.dirty = true;
                                continue;
                            };
                            let model_count = app.providers[provider_idx].available_models.len();
                            let max_idx = model_count;
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
                                app.state =
                                    SetupState::on_model_order_enter(!app.model_order.is_empty());
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
                                    app.theme = crate::theme::Theme::from_config(theme_id);
                                }
                            }
                            KeyCode::Down | KeyCode::Right => {
                                if app.selected_theme_idx
                                    < app.available_themes.len().saturating_sub(1)
                                {
                                    app.selected_theme_idx += 1;
                                    let theme_id = app.available_themes[app.selected_theme_idx].0;
                                    app.theme = crate::theme::Theme::from_config(theme_id);
                                }
                            }
                            KeyCode::Enter => app.state = SetupState::on_theme_enter(),
                            KeyCode::Esc => app.state = SetupState::ModelOrderSelection,
                            _ => {}
                        },
                        SetupState::Confirm => match key.code {
                            KeyCode::Enter | KeyCode::Char('y') => {
                                app.state = SetupState::on_confirm_enter();
                                app.dirty = true;
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
