use ratatui::style::{Color, Modifier, Style};

/// Icy-blue ZeroClaw palette.
pub const ICY_BLUE: Color = Color::Rgb(100, 200, 255);
pub const ICY_CYAN: Color = Color::Rgb(140, 230, 255);
pub const ICY_WHITE: Color = Color::Rgb(220, 240, 255);
pub const FROST_DIM: Color = Color::Rgb(80, 130, 170);
pub const FROST_BG: Color = Color::Rgb(10, 15, 30);
pub const CRAB_ACCENT: Color = Color::Rgb(255, 100, 80);
pub const SUCCESS_GREEN: Color = Color::Rgb(80, 220, 120);
pub const WARN_YELLOW: Color = Color::Rgb(255, 220, 80);
pub const ERR_RED: Color = Color::Rgb(255, 80, 80);
pub const SELECTION_BG: Color = Color::Rgb(30, 60, 100);

pub fn title_style() -> Style {
    Style::default().fg(ICY_BLUE).add_modifier(Modifier::BOLD)
}

pub fn heading_style() -> Style {
    Style::default().fg(ICY_CYAN).add_modifier(Modifier::BOLD)
}

pub fn body_style() -> Style {
    Style::default().fg(ICY_WHITE)
}

pub fn dim_style() -> Style {
    Style::default().fg(FROST_DIM)
}

pub fn accent_style() -> Style {
    Style::default()
        .fg(CRAB_ACCENT)
        .add_modifier(Modifier::BOLD)
}

pub fn success_style() -> Style {
    Style::default().fg(SUCCESS_GREEN)
}

pub fn warn_style() -> Style {
    Style::default().fg(WARN_YELLOW)
}

pub fn selected_style() -> Style {
    Style::default()
        .fg(ICY_BLUE)
        .bg(SELECTION_BG)
        .add_modifier(Modifier::BOLD)
}

pub fn unselected_style() -> Style {
    Style::default().fg(FROST_DIM)
}

pub fn border_style() -> Style {
    Style::default().fg(ICY_BLUE)
}

pub fn input_style() -> Style {
    Style::default().fg(ICY_WHITE)
}
