use ratatui::style::{Color, Modifier, Style};

/// Icy-blue ZeroClaw palette. Only the colors actually referenced by a style
/// helper below are kept — add more when a new widget needs them.
const ICY_BLUE: Color = Color::Rgb(100, 200, 255);
const ICY_CYAN: Color = Color::Rgb(140, 230, 255);
const ICY_WHITE: Color = Color::Rgb(220, 240, 255);
const FROST_DIM: Color = Color::Rgb(80, 130, 170);
const CRAB_ACCENT: Color = Color::Rgb(255, 100, 80);
const WARN_YELLOW: Color = Color::Rgb(255, 220, 80);
const SELECTION_BG: Color = Color::Rgb(30, 60, 100);

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

pub fn warn_style() -> Style {
    Style::default().fg(WARN_YELLOW)
}

pub fn selected_style() -> Style {
    Style::default()
        .fg(ICY_BLUE)
        .bg(SELECTION_BG)
        .add_modifier(Modifier::BOLD)
}

pub fn border_style() -> Style {
    Style::default().fg(ICY_BLUE)
}

pub fn input_style() -> Style {
    Style::default().fg(ICY_WHITE)
}
