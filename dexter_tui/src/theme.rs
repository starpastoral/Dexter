use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
pub struct Theme {
    // Base
    pub base_style: Style,
    // Borders
    pub border_style: Style,
    // Header
    pub header_title_style: Style,
    pub header_subtitle_style: Style,
    // Input
    pub input_prompt_style: Style,
    pub input_text_style: Style,
    pub input_cursor_style: Style,
    // Proposal/Command
    pub proposal_cmd_style: Style,
    // Status/Processing
    pub processing_spinner_style: Style,
    pub processing_text_style: Style,
    // Preview/Diff
    pub diff_header_style: Style,
    pub diff_added_style: Style,
    pub diff_removed_style: Style,
    // Footer
    pub footer_text_style: Style,
    pub footer_highlight_style: Style,
    pub footer_key_style: Style,
    pub footer_selected_style: Style,
    // History
    pub history_selected_style: Style,
    // Alerts/Errors
    pub error_style: Style,
    pub success_style: Style,
}

impl Theme {
    pub fn from_config(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "dark" => Self::dark(),
            "light" => Self::light(),
            "auto" => {
                // Detect system theme
                match dark_light::detect() {
                    dark_light::Mode::Dark => Self::dark(),
                    dark_light::Mode::Light => Self::light(),
                    // Default to dark if detection fails
                    dark_light::Mode::Default => Self::dark(),
                }
            }
            _ => Self::retro(), // Default to retro for "retro" or unknown values
        }
    }

    pub fn retro() -> Self {
        let amber = Color::Rgb(255, 176, 0);
        let amber_dim = Color::Rgb(150, 110, 0);
        let red_alert = Color::Rgb(255, 40, 40);
        let bg = Color::Black;

        Self {
            base_style: Style::default().fg(amber),
            border_style: Style::default().fg(amber_dim),

            header_title_style: Style::default().fg(amber).add_modifier(Modifier::BOLD),
            header_subtitle_style: Style::default().fg(amber_dim),

            input_prompt_style: Style::default().fg(amber).add_modifier(Modifier::BOLD),
            input_text_style: Style::default().fg(Color::White),
            input_cursor_style: Style::default()
                .bg(amber)
                .fg(bg)
                .add_modifier(Modifier::RAPID_BLINK),

            proposal_cmd_style: Style::default().bg(amber).fg(bg),

            processing_spinner_style: Style::default().fg(amber).add_modifier(Modifier::BOLD),
            processing_text_style: Style::default().fg(amber),

            diff_header_style: Style::default().fg(amber).add_modifier(Modifier::BOLD),
            diff_added_style: Style::default().fg(amber), // In retro, everything is amber
            diff_removed_style: Style::default().fg(amber_dim), // Old stuff dim

            footer_text_style: Style::default().fg(amber_dim),
            footer_highlight_style: Style::default().fg(amber),
            footer_key_style: Style::default().fg(bg).bg(amber),
            footer_selected_style: Style::default()
                .fg(Color::Blue)
                .bg(Color::Rgb(190, 190, 190))
                .add_modifier(Modifier::BOLD),
            history_selected_style: Style::default()
                .fg(Color::Gray)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),

            error_style: Style::default().fg(red_alert),
            success_style: Style::default().fg(bg).bg(amber),
        }
    }

    pub fn light() -> Self {
        let text_main = Color::Black;
        let text_dim = Color::DarkGray;
        let accent = Color::Blue;
        let red_alert = Color::Red;

        Self {
            base_style: Style::default().fg(text_main),
            border_style: Style::default().fg(accent),

            header_title_style: Style::default().fg(accent).add_modifier(Modifier::BOLD),
            header_subtitle_style: Style::default().fg(text_dim),

            input_prompt_style: Style::default().fg(accent).add_modifier(Modifier::BOLD),
            input_text_style: Style::default().fg(accent),
            input_cursor_style: Style::default()
                .bg(accent)
                .fg(Color::White)
                .add_modifier(Modifier::RAPID_BLINK),

            proposal_cmd_style: Style::default().bg(accent).fg(Color::White),

            processing_spinner_style: Style::default().fg(accent).add_modifier(Modifier::BOLD),
            processing_text_style: Style::default().fg(accent),

            diff_header_style: Style::default().fg(accent).add_modifier(Modifier::BOLD),
            diff_added_style: Style::default().fg(accent),
            diff_removed_style: Style::default().fg(text_dim),

            footer_text_style: Style::default().fg(text_dim),
            footer_highlight_style: Style::default().fg(accent),
            footer_key_style: Style::default().fg(Color::White).bg(accent),
            footer_selected_style: Style::default()
                .fg(Color::Blue)
                .bg(Color::Rgb(190, 190, 190))
                .add_modifier(Modifier::BOLD),
            history_selected_style: Style::default()
                .fg(Color::Gray)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),

            error_style: Style::default().fg(red_alert),
            success_style: Style::default().fg(Color::White).bg(accent),
        }
    }

    pub fn dark() -> Self {
        let amber = Color::Rgb(255, 176, 0);
        let amber_dim = Color::Rgb(150, 110, 0);
        let bg = Color::Rgb(14, 12, 10);
        let red_alert = Color::Rgb(255, 80, 80);

        Self {
            base_style: Style::default().fg(amber).bg(bg),
            border_style: Style::default().fg(amber_dim),

            header_title_style: Style::default().fg(amber).add_modifier(Modifier::BOLD),
            header_subtitle_style: Style::default().fg(amber_dim),

            input_prompt_style: Style::default().fg(amber).add_modifier(Modifier::BOLD),
            input_text_style: Style::default().fg(Color::White),
            input_cursor_style: Style::default()
                .bg(amber)
                .fg(bg)
                .add_modifier(Modifier::RAPID_BLINK),

            proposal_cmd_style: Style::default().bg(amber).fg(bg),

            processing_spinner_style: Style::default().fg(amber).add_modifier(Modifier::BOLD),
            processing_text_style: Style::default().fg(amber),

            diff_header_style: Style::default().fg(amber).add_modifier(Modifier::BOLD),
            diff_added_style: Style::default().fg(amber),
            diff_removed_style: Style::default().fg(amber_dim),

            footer_text_style: Style::default().fg(amber_dim),
            footer_highlight_style: Style::default().fg(amber),
            footer_key_style: Style::default().fg(bg).bg(amber),
            footer_selected_style: Style::default()
                .fg(Color::Blue)
                .bg(Color::Rgb(190, 190, 190))
                .add_modifier(Modifier::BOLD),
            history_selected_style: Style::default()
                .fg(Color::Gray)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),

            error_style: Style::default().fg(red_alert),
            success_style: Style::default().fg(bg).bg(amber),
        }
    }
}
