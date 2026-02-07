use anyhow::Result;
use dexter_core::CachePolicy;

use crate::app::editor::char_count;
use crate::app::state::{App, AppState, FocusArea, FooterAction};

pub async fn perform_footer_action(app: &mut App, action: FooterAction) -> Result<bool> {
    match action {
        FooterAction::Quit => return Ok(true),
        FooterAction::Settings => {
            app.pending_open_settings = true;
            app.dirty = true;
        }
        FooterAction::ToggleDebug => {
            app.show_debug = !app.show_debug;
            app.logs.push(format!(
                "Debug Mode: {}",
                if app.show_debug { "ON" } else { "OFF" }
            ));
            app.dirty = true;
        }
        FooterAction::Retry => {
            if app.input.trim().is_empty() {
                app.state = AppState::Input;
                app.focus = FocusArea::Proposal;
                app.input_cursor = char_count(&app.input);
                app.dirty = true;
            } else {
                app.reset_for_new_request();
                app.focus = FocusArea::FooterButtons;
                app.footer_focus = 0;
                app.state = AppState::PendingRouting;
                app.dirty = true;
            }
        }
        FooterAction::ClearInput => {
            app.input.clear();
            app.input_cursor = 0;
            app.notice = None;
            app.clarify = None;
            app.focus = FocusArea::Proposal;
            app.dirty = true;
        }
        FooterAction::Submit => {
            if !app.input.trim().is_empty() {
                app.logs
                    .push(format!("Input submitted ({} chars)", app.input.len()));
                app.reset_for_new_request();
                app.focus = FocusArea::FooterButtons;
                app.footer_focus = 0;
                app.state = AppState::PendingRouting;
                app.dirty = true;
            }
        }
        FooterAction::Execute => {
            app.focus = FocusArea::FooterButtons;
            app.footer_focus = 0;
            app.execute_command().await?;
            app.dirty = true;
        }
        FooterAction::BackToInput => {
            app.reset_to_input_preserve_text();
        }
        FooterAction::EditCommand => {
            if let Some(cmd) = &app.generated_command {
                app.command_draft = cmd.clone();
            }
            app.command_cursor = char_count(&app.command_draft);
            app.state = AppState::EditingCommand;
            app.focus = FocusArea::Proposal;
            app.footer_focus = 0;
            app.dirty = true;
        }
        FooterAction::EditInput => {
            app.reset_to_input_preserve_text();
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
            app.dirty = true;
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
                app.dirty = true;
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
            app.dirty = true;
        }
        FooterAction::ResetToInput => {
            app.reset_to_input_preserve_text();
        }
        FooterAction::ClarifySelect(idx) => {
            if let Some(payload) = &app.clarify {
                if let Some(opt) = payload.options.get(idx) {
                    app.input = opt.resolved_intent.clone();
                    app.input_cursor = char_count(&app.input);
                    app.logs.push(format!("Clarify selected: {}", opt.label));
                    app.reset_for_new_request();
                    app.focus = FocusArea::FooterButtons;
                    app.footer_focus = 0;
                    app.state = AppState::PendingRouting;
                    app.dirty = true;
                }
            }
        }
    }

    Ok(false)
}
