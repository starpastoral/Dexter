mod app;
mod setup;
mod theme;
mod ui;

use anyhow::Result;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use dexter_core::Config;
use ratatui::Terminal;
use std::io::stdout;

use crate::app::runtime::run_app;
use crate::app::state::App;
use crate::setup::runtime::run_setup_wizard;

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = Config::load().await?;

    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let mut keyboard_enhancement_enabled = false;
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
        if execute!(stdout, PushKeyboardEnhancementFlags(flags)).is_ok() {
            keyboard_enhancement_enabled = true;
        }
    }

    let mouse_capture_enabled = execute!(stdout, EnableMouseCapture).is_ok();
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let args: Vec<String> = std::env::args().collect();
    let force_setup = args.contains(&"--setup".to_string());

    if !config.has_keys() || force_setup {
        match run_setup_wizard(&mut terminal, config.clone()).await {
            Ok(new_config) => {
                config = new_config;
            }
            Err(e) => {
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
    let res = run_app(&mut terminal, &mut app).await;

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
