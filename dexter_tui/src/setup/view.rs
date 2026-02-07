use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap},
    Frame,
};

use crate::setup::state::{model_route_display, SetupApp, SetupState};

pub fn mask_api_key(raw: &str) -> String {
    let value = raw.trim();
    let len = value.chars().count();
    if len == 0 {
        return String::new();
    }
    if len <= 4 {
        return "*".repeat(len);
    }

    let suffix: String = value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{}{}", "*".repeat(len - 4), suffix)
}

pub fn setup_ui(f: &mut Frame, app: &SetupApp) {
    // Fill the full frame so theme background also applies to top/bottom gutters.
    f.render_widget(Block::default().style(app.theme.base_style), f.area());

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
    )
    .style(app.theme.base_style);
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
    if app.state == SetupState::Confirm {
        render_setup_confirm_table(f, app, chunks[1]);
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
                let key_display = if provider.requires_api_key() {
                    if provider.api_key.is_empty() {
                        "_".to_string()
                    } else {
                        mask_api_key(&provider.api_key)
                    }
                } else {
                    "N/A".to_string()
                };
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
                        Span::styled(key_display, app.theme.input_text_style),
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
                        "STEP 3: SELECT ACTIVE MODELS",
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
                "ENTER: Next Step (Theme)  ESC: Back to Step 1",
                app.theme.header_subtitle_style,
            )));
            lines
        }
        SetupState::ThemeSelection => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "STEP 5: SELECT THEME",
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
                let marker = if i == app.selected_theme_idx {
                    "◆"
                } else {
                    "◇"
                };
                lines.push(Line::from(Span::styled(
                    format!("  {} {}", marker, display_name),
                    style,
                )));
            }

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Use Arrow Keys to Select, ENTER to Confirm",
                app.theme.header_subtitle_style,
            )));
            lines
        }
        SetupState::Confirm => vec![],
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
            Constraint::Length(if compact { 8 } else { 7 }),
            Constraint::Length(1),
            Constraint::Length((app.providers.len() as u16).saturating_add(2)),
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
            let setup_state = if provider.requires_api_key() {
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

    let help = Paragraph::new("ENTER: Next Step (Theme)   ESC: Back to Step 1")
        .style(app.theme.header_subtitle_style)
        .wrap(Wrap { trim: true });
    f.render_widget(help, layout[3]);
}

fn render_setup_confirm_table(f: &mut Frame, app: &SetupApp, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border_style)
        .title(" SETUP WIZARD ");
    f.render_widget(&block, area);
    let inner = block.inner(area);
    let compact = inner.width < 108;
    let very_narrow = inner.width < 86;

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if compact { 5 } else { 4 }),
            Constraint::Length(1),
            Constraint::Length(5),
            Constraint::Length(if very_narrow { 8 } else { 5 }),
            Constraint::Min(6),
            Constraint::Length(3),
        ])
        .split(inner);

    let intro = vec![
        Line::from(Span::styled(
            "STEP 6: CONFIRM SETTINGS",
            app.theme.header_title_style,
        )),
        Line::from(""),
        Line::from("Review the final setup before saving."),
    ];
    let intro_para = Paragraph::new(intro)
        .style(app.theme.header_subtitle_style)
        .wrap(Wrap { trim: true });
    f.render_widget(intro_para, layout[0]);

    let primary = app
        .model_order
        .first()
        .map(|r| model_route_display(r, &app.providers))
        .unwrap_or_else(|| "Unknown".to_string());
    let summary_header = Row::new(vec![Cell::from("FIELD"), Cell::from("VALUE")])
        .style(app.theme.footer_text_style.add_modifier(Modifier::BOLD));
    let summary_rows = vec![
        Row::new(vec![
            Cell::from("THEME"),
            Cell::from(app.available_themes[app.selected_theme_idx].1),
        ])
        .style(app.theme.header_subtitle_style),
        Row::new(vec![Cell::from("PRIMARY"), Cell::from(primary)])
            .style(app.theme.header_subtitle_style),
        Row::new(vec![
            Cell::from("ROUTES"),
            Cell::from(app.model_order.len().to_string()),
        ])
        .style(app.theme.header_subtitle_style),
    ];
    let summary_table = Table::new(summary_rows, [Constraint::Length(10), Constraint::Min(20)])
        .header(summary_header)
        .column_spacing(1)
        .style(app.theme.base_style);
    f.render_widget(summary_table, layout[2]);

    let enabled = app.enabled_provider_names();
    let mut enabled_lines = vec![Line::from(Span::styled(
        "ENABLED PROVIDERS",
        app.theme.footer_text_style.add_modifier(Modifier::BOLD),
    ))];
    if enabled.is_empty() {
        enabled_lines.push(Line::from("  ◇ (NONE)"));
    } else {
        for name in enabled {
            enabled_lines.push(Line::from(format!("  ◆ {}", name)));
        }
    }
    let enabled_para = Paragraph::new(enabled_lines)
        .style(app.theme.header_subtitle_style)
        .wrap(Wrap { trim: true });
    f.render_widget(enabled_para, layout[3]);

    let fallback_header = Row::new(vec![
        Cell::from("ROLE"),
        Cell::from("MODEL"),
        Cell::from(if very_narrow { "PROV" } else { "PROVIDER" }),
    ])
    .style(app.theme.footer_text_style.add_modifier(Modifier::BOLD));
    let fallback_rows = if app.model_order.is_empty() {
        vec![Row::new(vec![
            Cell::from("--"),
            Cell::from("(none)"),
            Cell::from("--"),
        ])
        .style(app.theme.header_subtitle_style)]
    } else {
        app.model_order
            .iter()
            .enumerate()
            .map(|(idx, route)| {
                let provider = app
                    .providers
                    .get(route.provider_idx)
                    .map(|p| p.name().to_string())
                    .unwrap_or_else(|| "Unknown".to_string());
                let role = if idx == 0 {
                    "PRIMARY".to_string()
                } else {
                    format!("F{}", idx)
                };

                Row::new(vec![
                    Cell::from(role),
                    Cell::from(route.model.clone()),
                    Cell::from(provider),
                ])
                .style(if idx == 0 {
                    app.theme.proposal_cmd_style
                } else {
                    app.theme.header_subtitle_style
                })
            })
            .collect::<Vec<_>>()
    };
    let fallback_widths = if very_narrow {
        vec![
            Constraint::Length(8),
            Constraint::Min(10),
            Constraint::Length(8),
        ]
    } else if compact {
        vec![
            Constraint::Length(8),
            Constraint::Min(18),
            Constraint::Length(12),
        ]
    } else {
        vec![
            Constraint::Length(8),
            Constraint::Min(24),
            Constraint::Length(16),
        ]
    };
    let fallback_table = Table::new(fallback_rows, fallback_widths)
        .header(fallback_header)
        .column_spacing(1)
        .style(app.theme.base_style);
    f.render_widget(fallback_table, layout[4]);

    let prompt = Paragraph::new(vec![
        Line::from(Span::styled(
            "Save and Apply? [Y/N]",
            app.theme.input_prompt_style,
        )),
        Line::from(Span::styled(
            "ENTER/Y: Save and apply   ESC/N: Back to Theme",
            app.theme.header_subtitle_style,
        )),
    ])
    .wrap(Wrap { trim: true });
    f.render_widget(prompt, layout[5]);
}
