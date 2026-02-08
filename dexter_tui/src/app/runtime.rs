use anyhow::{anyhow, Result};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use dexter_core::{CachePolicy, Executor, RouteOutcome, Router, SafetyGuard};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::Stdout;
use std::time::Duration;
use tokio::sync::oneshot;

use crate::app::actions::perform_footer_action;
use crate::app::editor::{
    char_count, delete_char_at_cursor, delete_char_before_cursor, insert_char_at_cursor,
    move_cursor_down, move_cursor_line_end, move_cursor_line_start, move_cursor_up, point_in_rect,
    set_cursor_from_click,
};
use crate::app::state::{App, AppState, ClarifyPayload, FocusArea, FooterAction};
use crate::setup::runtime::run_settings_panel;
use crate::ui::main_view::ui;
use dexter_plugins::PreviewContent;

pub async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    // Initial context fetch
    let _ = app.update_context().await;

    loop {
        progress_state_and_settings(terminal, app).await?;

        if app.dirty || app.is_processing_state() {
            terminal.draw(|f| ui(f, app))?;
            app.dirty = false;
        }

        let poll_ms = if app.is_processing_state() { 50 } else { 200 };
        if event::poll(Duration::from_millis(poll_ms))? {
            app.dirty = true;
            if handle_runtime_event(app, event::read()?).await? {
                return Ok(());
            }
        }
    }
}

async fn progress_state_and_settings(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    app.tick_count += 1;

    // Non-blocking automatic state transitions.
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
                    app.dirty = true;
                    return Ok(());
                }
            };
            let plugin = match app.plugins.iter().find(|p| p.name() == plugin_name) {
                Some(p) => p.clone(),
                None => {
                    app.state = AppState::Error("Plugin not found".to_string());
                    app.dirty = true;
                    return Ok(());
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
                    .generate_command_with_policy(&input, &context, plugin.as_ref(), cache_policy)
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
                    app.dirty = true;
                    return Ok(());
                }
            };
            let plugin_name = match app.selected_plugin.clone() {
                Some(p) => p,
                None => {
                    app.state = AppState::Error("No plugin selected".to_string());
                    app.dirty = true;
                    return Ok(());
                }
            };
            let plugin = match app.plugins.iter().find(|p| p.name() == plugin_name) {
                Some(p) => p.clone(),
                None => {
                    app.state = AppState::Error("Plugin not found".to_string());
                    app.dirty = true;
                    return Ok(());
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
                                app.push_log(format!("Routed to plugin: {}", plugin));
                                app.generation_cache_policy = CachePolicy::Normal;
                                app.state = AppState::PendingGeneration;
                                app.dirty = true;
                            }
                            RouteOutcome::Unsupported { reason } => {
                                app.notice = Some(format!(
                                    "This request isn’t supported.\n{}\nTry: convert formats or rename files (rename only, no conversion).",
                                    reason
                                ));
                                app.push_log("Routing result: unsupported request".to_string());
                                app.log_block("ROUTING_UNSUPPORTED", &reason);
                                app.state = AppState::Input;
                                app.focus = FocusArea::Proposal;
                                app.footer_focus = 0;
                                app.dirty = true;
                            }
                            RouteOutcome::Clarify {
                                question, options, ..
                            } => {
                                let clarify_text = format_clarify_block(&question, &options);
                                app.clarify = Some(ClarifyPayload { question, options });
                                app.notice = None;
                                app.push_log("Routing requires clarification".to_string());
                                app.log_block("ROUTING_CLARIFY", &clarify_text);
                                app.state = AppState::Clarifying;
                                app.focus = FocusArea::FooterButtons;
                                app.footer_focus = 0;
                                app.dirty = true;
                            }
                        },
                        Err(e) => {
                            app.log_block("ROUTING_ERROR", &e.to_string());
                            app.state = AppState::Error(format!("Routing error: {}", e));
                            app.dirty = true;
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
                            app.push_log(format!("Generated command: {}", cmd));
                            app.log_block("GENERATED_COMMAND", &cmd);
                            app.dry_run_output = None;
                            app.output_scroll = 0;
                            app.state = AppState::PendingDryRun;
                            app.dirty = true;
                        }
                        Err(e) => {
                            app.log_block("GENERATION_ERROR", &e.to_string());
                            app.state = AppState::Error(format!("Generation error: {}", e));
                            app.dirty = true;
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
                            let preview_text = preview_to_log(&output);
                            app.push_log("Preview data captured successfully.".to_string());
                            app.log_block("DRY_RUN_PREVIEW", &preview_text);
                            app.dry_run_output = Some(output);
                            app.output_scroll = 0;
                            app.state = AppState::AwaitingConfirmation;
                            app.dirty = true;
                        }
                        Err(e) => {
                            app.push_log(format!("Preview failed: {}", e));
                            app.log_block("DRY_RUN_ERROR", &e.to_string());
                            app.state = AppState::Error(format!("Dry run failed: {}", e));
                            app.dirty = true;
                        }
                    }
                }
            }
        }
        AppState::Executing => {
            // Check for progress updates.
            if let Some(rx) = &mut app.progress_rx {
                while let Ok(prog) = rx.try_recv() {
                    let progress_line = if let Some(pct) = prog.percentage {
                        format!("{:.1}% {}", pct, prog.message)
                    } else {
                        prog.message.clone()
                    };
                    app.session_logger.event("PROGRESS", &progress_line);
                    app.progress = Some(prog);
                    app.dirty = true;
                }
            }

            // Check for completion.
            let mut finished = false;
            if let Some(rx) = &mut app.execution_result_rx {
                if let Ok(result) = rx.try_recv() {
                    finished = true;
                    match result {
                        Ok(output) => {
                            app.log_block("EXECUTION_OUTPUT", &output);
                            app.state = AppState::Finished(output);
                            app.push_log("Execution completed successfully.".to_string());
                            let _ = app.update_context().await;
                            app.dirty = true;
                        }
                        Err(e) => {
                            app.log_block("EXECUTION_ERROR", &e.to_string());
                            app.state = AppState::Error(format!("Execution failed: {}", e));
                            app.dirty = true;
                        }
                    }
                }
            }

            if finished {
                app.progress_rx = None;
                app.execution_result_rx = None;
                app.progress = None;
                app.dirty = true;
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
            app.push_log("Cannot open settings while a task is running.".to_string());
        } else {
            match run_settings_panel(terminal, app.config.clone()).await {
                Ok(new_config) => {
                    app.apply_config(new_config);
                    app.push_log("Settings updated.".to_string());
                    app.dirty = true;
                }
                Err(e) => {
                    let msg = e.to_string();
                    if !msg.to_lowercase().contains("aborted") {
                        app.push_log(format!("Settings update failed: {}", msg));
                        app.log_block("SETTINGS_ERROR", &msg);
                        app.dirty = true;
                    }
                }
            }
        }
    }

    Ok(())
}

fn preview_to_log(preview: &PreviewContent) -> String {
    match preview {
        PreviewContent::Text(text) => text.clone(),
        PreviewContent::DiffList(diffs) => {
            if diffs.is_empty() {
                return "No changes detected.".to_string();
            }
            let mut out = Vec::new();
            for (idx, diff) in diffs.iter().enumerate() {
                out.push(format!("FILE {:02}", idx + 1));
                if let Some(status) = &diff.status {
                    out.push(format!("status={}", status));
                }
                out.push(format!("old={}", diff.original));
                out.push(format!("new={}", diff.new));
                out.push(String::new());
            }
            out.join("\n")
        }
    }
}

fn format_clarify_block(question: &str, options: &[dexter_core::ClarifyOption]) -> String {
    let mut out = vec![format!("question={}", question)];
    for opt in options {
        out.push(format!("option.id={}", opt.id));
        out.push(format!("option.label={}", opt.label));
        out.push(format!("option.detail={}", opt.detail));
        out.push(format!("option.resolved_intent={}", opt.resolved_intent));
        out.push(String::new());
    }
    out.join("\n")
}

async fn handle_runtime_event(app: &mut App, event: Event) -> Result<bool> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => handle_key_press(app, key).await,
        Event::Paste(text) => {
            handle_paste(app, &text);
            Ok(false)
        }
        Event::Mouse(mouse) => handle_mouse_event(app, mouse).await,
        _ => Ok(false),
    }
}

async fn handle_key_press(app: &mut App, key: KeyEvent) -> Result<bool> {
    let editing = app.focus == FocusArea::Proposal
        && matches!(app.state, AppState::Input | AppState::EditingCommand);

    // Global output scrolling keys (work in most states).
    if !editing {
        match key.code {
            KeyCode::Up => {
                app.output_scroll = app.output_scroll.saturating_sub(1);
                return Ok(false);
            }
            KeyCode::Down => {
                app.output_scroll = app.output_scroll.saturating_add(1);
                return Ok(false);
            }
            KeyCode::PageUp => {
                app.output_scroll = app.output_scroll.saturating_sub(10);
                return Ok(false);
            }
            KeyCode::PageDown => {
                app.output_scroll = app.output_scroll.saturating_add(10);
                return Ok(false);
            }
            KeyCode::Home => {
                app.output_scroll = 0;
                return Ok(false);
            }
            KeyCode::End => {
                app.output_scroll = app.output_max_scroll;
                return Ok(false);
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
                        app.footer_focus = (app.footer_focus + 1) % app.footer_buttons.len();
                    }
                }
            }
            return Ok(false);
        }
        KeyCode::Left => {
            if app.focus == FocusArea::FooterButtons && !app.footer_buttons.is_empty() {
                if app.footer_focus == 0 {
                    app.footer_focus = app.footer_buttons.len() - 1;
                } else {
                    app.footer_focus -= 1;
                }
                return Ok(false);
            }
        }
        KeyCode::Right => {
            if app.focus == FocusArea::FooterButtons && !app.footer_buttons.is_empty() {
                app.footer_focus = (app.footer_focus + 1) % app.footer_buttons.len();
                return Ok(false);
            }
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            if app.focus == FocusArea::FooterButtons {
                if let Some(action) = app.footer_buttons.get(app.footer_focus).map(|b| b.action) {
                    if perform_footer_action(app, action).await? {
                        return Ok(true);
                    }
                }
                return Ok(false);
            }
        }
        _ => {}
    }

    match app.state {
        AppState::Input => match key.code {
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if perform_footer_action(app, FooterAction::ToggleDebug).await? {
                    return Ok(true);
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if perform_footer_action(app, FooterAction::ClearInput).await? {
                    return Ok(true);
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if perform_footer_action(app, FooterAction::Submit).await? {
                    return Ok(true);
                }
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if perform_footer_action(app, FooterAction::Submit).await? {
                    return Ok(true);
                }
            }
            KeyCode::Enter => {
                if app.focus == FocusArea::Proposal {
                    insert_char_at_cursor(&mut app.input, &mut app.input_cursor, '\n');
                }
            }
            KeyCode::Left => {
                if app.focus == FocusArea::Proposal && app.input_cursor > 0 {
                    app.input_cursor -= 1;
                }
            }
            KeyCode::Right => {
                if app.focus == FocusArea::Proposal && app.input_cursor < char_count(&app.input) {
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
                    delete_char_before_cursor(&mut app.input, &mut app.input_cursor);
                    app.notice = None;
                    app.clarify = None;
                }
            }
            KeyCode::Delete => {
                if app.focus == FocusArea::Proposal {
                    delete_char_at_cursor(&mut app.input, &mut app.input_cursor);
                }
            }
            KeyCode::Esc => return Ok(true),
            _ => {}
        },
        AppState::AwaitingConfirmation => match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if perform_footer_action(app, FooterAction::Execute).await? {
                    return Ok(true);
                }
            }
            KeyCode::Char('m') => {
                if perform_footer_action(app, FooterAction::EditCommand).await? {
                    return Ok(true);
                }
            }
            KeyCode::Char('e') => {
                if perform_footer_action(app, FooterAction::EditInput).await? {
                    return Ok(true);
                }
            }
            KeyCode::Char('r') => {
                if perform_footer_action(app, FooterAction::Regenerate).await? {
                    return Ok(true);
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                if perform_footer_action(app, FooterAction::BackToInput).await? {
                    return Ok(true);
                }
            }
            _ => {}
        },
        AppState::EditingCommand => match key.code {
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if perform_footer_action(app, FooterAction::PreviewEditedCommand).await? {
                    return Ok(true);
                }
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if perform_footer_action(app, FooterAction::PreviewEditedCommand).await? {
                    return Ok(true);
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
                    move_cursor_line_start(&app.command_draft, &mut app.command_cursor);
                }
            }
            KeyCode::End => {
                if app.focus == FocusArea::Proposal {
                    move_cursor_line_end(&app.command_draft, &mut app.command_cursor);
                }
            }
            KeyCode::Char(c) => {
                if app.focus == FocusArea::Proposal {
                    insert_char_at_cursor(&mut app.command_draft, &mut app.command_cursor, c);
                }
            }
            KeyCode::Backspace => {
                if app.focus == FocusArea::Proposal {
                    delete_char_before_cursor(&mut app.command_draft, &mut app.command_cursor);
                }
            }
            KeyCode::Delete => {
                if app.focus == FocusArea::Proposal {
                    delete_char_at_cursor(&mut app.command_draft, &mut app.command_cursor);
                }
            }
            KeyCode::Esc => {
                if perform_footer_action(app, FooterAction::CancelEditCommand).await? {
                    return Ok(true);
                }
            }
            _ => {}
        },
        AppState::Finished(_) | AppState::Error(_) => match key.code {
            KeyCode::Char('r') => {
                if perform_footer_action(app, FooterAction::Retry).await? {
                    return Ok(true);
                }
            }
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ') => {
                if perform_footer_action(app, FooterAction::ResetToInput).await? {
                    return Ok(true);
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
            // Non-interactive states.
        }
    }

    Ok(false)
}

fn handle_paste(app: &mut App, text: &str) {
    let editing_proposal = app.focus == FocusArea::Proposal
        && matches!(app.state, AppState::Input | AppState::EditingCommand);
    if !editing_proposal {
        return;
    }

    match app.state {
        AppState::Input => {
            for ch in text.chars() {
                insert_char_at_cursor(&mut app.input, &mut app.input_cursor, ch);
            }
            app.notice = None;
            app.clarify = None;
        }
        AppState::EditingCommand => {
            for ch in text.chars() {
                insert_char_at_cursor(&mut app.command_draft, &mut app.command_cursor, ch);
            }
        }
        _ => {}
    }
}

async fn handle_mouse_event(app: &mut App, mouse: MouseEvent) -> Result<bool> {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.output_scroll = app.output_scroll.saturating_sub(3);
        }
        MouseEventKind::ScrollDown => {
            app.output_scroll = app.output_scroll.saturating_add(3);
        }
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Down(MouseButton::Left) => {
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
                    let new_scroll = ((rel as u32) * (app.output_max_scroll as u32) / denom) as u16;
                    app.output_scroll = new_scroll.min(app.output_max_scroll);
                    return Ok(false);
                }
            }

            // Otherwise, treat it as a click on the button bar (if any) or focus change.
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if let Some(rect) = app.settings_button_rect {
                    if point_in_rect(rect, mouse.column, mouse.row) {
                        if perform_footer_action(app, FooterAction::Settings).await? {
                            return Ok(true);
                        }
                        return Ok(false);
                    }
                }

                // Click footer buttons if mouse is within the rect.
                let mut clicked_action: Option<(usize, FooterAction)> = None;
                for (idx, btn) in app.footer_buttons.iter().enumerate() {
                    if mouse.column >= btn.rect.x
                        && mouse.column < btn.rect.x + btn.rect.width
                        && mouse.row >= btn.rect.y
                        && mouse.row < btn.rect.y + btn.rect.height
                    {
                        clicked_action = Some((idx, btn.action));
                        break;
                    }
                }

                if let Some((idx, action)) = clicked_action {
                    app.focus = FocusArea::FooterButtons;
                    app.footer_focus = idx;
                    if perform_footer_action(app, action).await? {
                        return Ok(true);
                    }
                    return Ok(false);
                }

                // Convenience: click anywhere else to focus the proposal editor.
                if matches!(app.state, AppState::Input | AppState::EditingCommand) {
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
    }

    Ok(false)
}
