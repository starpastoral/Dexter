use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
    Frame,
};

use dexter_core::Config;
use dexter_plugins::PreviewContent;

use crate::app::editor::split_line_at_char;
use crate::app::state::{App, AppState, FocusArea, FooterAction, FooterButton};
use crate::theme::Theme;

pub fn ui(f: &mut Frame, app: &mut App) {
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
            let cursor_visible =
                app.focus == FocusArea::Proposal && (app.tick_count / 8).is_multiple_of(2);
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
        AppState::History => {
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
            (" USER INPUT ", lines)
        }
        AppState::EditingCommand => {
            let mut lines = vec![Line::from("")];
            let cmd_cursor = app
                .theme
                .proposal_cmd_style
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::RAPID_BLINK);
            let cursor_visible =
                app.focus == FocusArea::Proposal && (app.tick_count / 8).is_multiple_of(2);
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
    render_button_row(f, app, main_layout[2]);

    // --- SECTION 4: OUTPUT (PREVIEW / LOGS / STATUS) + OPTIONAL SCROLLBAR ---
    let output_title = output_title(app);
    let output_block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(output_title, app.theme.header_title_style));
    f.render_widget(&output_block, main_layout[3]);

    let inner = output_block.inner(main_layout[3]);
    let output_viewport_height = main_layout[3].height.saturating_sub(2);
    let output_inner_width = main_layout[3].width.saturating_sub(2);
    app.output_text_width = output_inner_width.saturating_sub(1);

    let (max_scroll, clamped_scroll, scrollbar_rect) = {
        let output_content = build_output_lines(app);
        let output_line_count = output_content.len() as u16;
        let max_scroll = output_line_count.saturating_sub(output_viewport_height);
        let clamped_scroll = app.output_scroll.min(max_scroll);
        let show_scrollbar = max_scroll > 0 && inner.width > 1 && inner.height > 0;
        let scrollbar_rect = if show_scrollbar {
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

        let output_para = Paragraph::new(output_content)
            .style(block_style)
            .wrap(Wrap { trim: false })
            .scroll((clamped_scroll, 0));
        f.render_widget(output_para, text_area);

        if show_scrollbar {
            let content_len = output_line_count.max(1) as usize;
            let mut scrollbar_state =
                ScrollbarState::new(content_len).position(clamped_scroll as usize);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(app.theme.border_style)
                .track_style(app.theme.base_style);
            f.render_stateful_widget(scrollbar, inner, &mut scrollbar_state);
        }

        (max_scroll, clamped_scroll, scrollbar_rect)
    };

    app.output_max_scroll = max_scroll;
    app.output_scroll = clamped_scroll;
    app.output_scrollbar_rect = scrollbar_rect;

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

fn render_button_row(f: &mut Frame, app: &mut App, area: Rect) {
    app.history_button_rect = None;

    if area.width == 0 || area.height == 0 {
        render_button_bar(f, app, area);
        return;
    }

    let compact = area.width < 92;
    let history_label = if compact { " [HIST] " } else { " [HISTORY] " };
    let history_width = history_label.len() as u16;

    if area.width < history_width {
        render_button_bar(f, app, Rect { width: 0, ..area });
        return;
    }

    if area.width == history_width {
        render_button_bar(f, app, Rect { width: 0, ..area });
        let mut history_style = app.theme.footer_key_style;
        if app.state == AppState::History {
            history_style = app.theme.footer_selected_style;
        }
        let history_button = Paragraph::new(history_label).style(history_style);
        f.render_widget(history_button, area);
        app.history_button_rect = Some(area);
        return;
    }

    let left_width = area.width.saturating_sub(history_width + 1);
    let left_area = Rect {
        x: area.x,
        y: area.y,
        width: left_width,
        height: area.height,
    };
    let history_area = Rect {
        x: area.x + left_width + 1,
        y: area.y,
        width: history_width,
        height: area.height,
    };

    render_button_bar(f, app, left_area);

    let mut history_style = app.theme.footer_key_style;
    if app.state == AppState::History {
        history_style = app.theme.footer_selected_style;
    }
    let history_button = Paragraph::new(history_label).style(history_style);
    f.render_widget(history_button, history_area);
    app.history_button_rect = Some(history_area);
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
            style = app.theme.footer_selected_style;
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
        AppState::History => vec![
            (FooterAction::ExecuteHistoryCommand, "RUN".to_string()),
            (
                FooterAction::ToggleHistoryPin,
                if app.history_selected_is_pinned() {
                    "UNPIN".to_string()
                } else {
                    "PIN".to_string()
                },
            ),
            (FooterAction::CloseHistory, "BACK".to_string()),
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

fn output_title(app: &App) -> &'static str {
    if app.show_debug {
        return " DEBUG_SYSTEM_INTERNAL ";
    }

    match &app.state {
        AppState::Input => " SYSTEM STATUS & LOGS ",
        AppState::History => " HISTORY ",
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
        AppState::History => render_history_view(app, &app.theme),
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
        text.push(Line::from(vec![
            Span::styled(" CWD_CONTEXT: ", theme.header_subtitle_style),
            Span::styled(
                format!("{}", std::env::current_dir().unwrap_or_default().display()),
                theme.header_subtitle_style,
            ),
        ]));
        text.push(Line::from(Span::styled(
            " FILES:",
            theme.header_subtitle_style,
        )));
        if ctx.files.is_empty() {
            text.push(Line::from(vec![
                Span::styled("   - ", theme.header_subtitle_style),
                Span::styled("(Empty)", theme.header_subtitle_style),
            ]));
        } else {
            for (idx, file) in ctx.files.iter().take(12).enumerate() {
                text.push(Line::from(vec![
                    Span::styled(format!("   {:02}. ", idx + 1), theme.header_subtitle_style),
                    Span::styled(file, theme.header_subtitle_style),
                ]));
            }
            if ctx.files.len() > 12 {
                text.push(Line::from(vec![
                    Span::styled("   ... ", theme.header_subtitle_style),
                    Span::styled(
                        format!("{} more files", ctx.files.len() - 12),
                        theme.header_subtitle_style,
                    ),
                ]));
            }
        }
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

fn render_history_view<'a>(app: &'a App, theme: &Theme) -> Vec<Line<'a>> {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Command history sorted by pin and execution time.",
            theme.header_subtitle_style,
        )),
        Line::from(Span::styled(
            "Up/Down/PageUp/PageDown/Home/End: Move  X/Run: Execute  P: Pin/Unpin  Esc: Back",
            theme.header_subtitle_style,
        )),
        Line::from(""),
    ];

    if app.history_items.is_empty() {
        lines.push(Line::from(Span::styled(
            "(No command history)",
            theme.header_subtitle_style,
        )));
        return lines;
    }

    let text_width = app.output_text_width.max(24) as usize;
    for (idx, item) in app.history_items.iter().enumerate() {
        let pin_label = if item.pinned_at.is_some() {
            "[PIN]"
        } else {
            "[   ]"
        };
        let row = format!(
            "{} {} [{}] {}",
            pin_label, item.entry.timestamp, item.entry.plugin, item.entry.command
        );
        let clipped = truncate_with_ellipsis(&row, text_width);
        let style = if idx == app.history_selected {
            theme.history_selected_style
        } else {
            theme.header_subtitle_style
        };
        lines.push(Line::from(Span::styled(clipped, style)));
    }

    lines
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

fn truncate_with_ellipsis(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let total = input.chars().count();
    if total <= max_chars {
        return input.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let keep = max_chars - 3;
    let prefix: String = input.chars().take(keep).collect();
    format!("{}...", prefix)
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
