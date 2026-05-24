//! ZeroClaw TUI colour palette and style helpers.
//!
//! This module is shared between the onboarding UI (lib target) and the main
//! chat TUI (binary target). Not every helper is used by both targets, so we
//! suppress the dead-code lint here to avoid spurious warnings.
#![allow(dead_code)]

use ratatui::style::{Color, Modifier, Style};

/// Icy-blue ZeroClaw palette. Only the colors actually referenced by a style
/// helper below are kept — add more when a new widget needs them.
const ICY_BLUE: Color = Color::Rgb(100, 200, 255);
const ICY_CYAN: Color = Color::Rgb(140, 230, 255);
pub(crate) const ICY_WHITE: Color = Color::Rgb(220, 240, 255);
const FROST_DIM: Color = Color::Rgb(80, 130, 170);
const CRAB_ACCENT: Color = Color::Rgb(255, 100, 80);
const WARN_YELLOW: Color = Color::Rgb(255, 220, 80);
pub(crate) const SELECTION_BG: Color = Color::Rgb(30, 60, 100);

/// Purple-ish hue for tool call labels.
const TOOL_PURPLE: Color = Color::Rgb(180, 140, 255);

pub(crate) fn title_style() -> Style {
    Style::default().fg(ICY_BLUE).add_modifier(Modifier::BOLD)
}

pub(crate) fn heading_style() -> Style {
    Style::default().fg(ICY_CYAN).add_modifier(Modifier::BOLD)
}

pub(crate) fn body_style() -> Style {
    Style::default().fg(ICY_WHITE)
}

pub(crate) fn dim_style() -> Style {
    Style::default().fg(FROST_DIM)
}

pub(crate) fn accent_style() -> Style {
    Style::default()
        .fg(CRAB_ACCENT)
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn warn_style() -> Style {
    Style::default().fg(WARN_YELLOW)
}

pub(crate) fn selected_style() -> Style {
    Style::default()
        .fg(ICY_BLUE)
        .bg(SELECTION_BG)
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn input_style() -> Style {
    Style::default().fg(ICY_WHITE)
}

/// "You:" label in the chat conversation.
pub(crate) fn user_label_style() -> Style {
    Style::default().fg(ICY_CYAN).add_modifier(Modifier::BOLD)
}

/// "Agent:" label in the chat conversation.
pub(crate) fn agent_label_style() -> Style {
    Style::default().fg(ICY_BLUE).add_modifier(Modifier::BOLD)
}

/// Error messages (error phase, etc.).
pub(crate) fn error_style() -> Style {
    Style::default().fg(CRAB_ACCENT)
}

/// Tool call label `[tool: name]`.
pub(crate) fn tool_label_style() -> Style {
    Style::default()
        .fg(TOOL_PURPLE)
        .add_modifier(Modifier::BOLD)
}

/// Inline code spans in markdown.
pub(crate) fn code_inline_style() -> Style {
    Style::default().fg(WARN_YELLOW)
}

/// Code block body lines.
pub(crate) fn code_block_style() -> Style {
    Style::default().fg(ICY_WHITE)
}

/// Thought / thinking output.
pub(crate) fn thought_style() -> Style {
    Style::default().fg(FROST_DIM).add_modifier(Modifier::ITALIC)
}

/// Overlay border/title accent (session list, rename, approval).
pub(crate) fn overlay_border_style() -> Style {
    Style::default().fg(ICY_CYAN)
}

/// Approval overlay border (warning tone).
pub(crate) fn approval_border_style() -> Style {
    Style::default().fg(WARN_YELLOW)
}

/// Highlight style for list items (agent picker, session list).
pub(crate) fn list_highlight_style() -> Style {
    Style::default().fg(ICY_CYAN).add_modifier(Modifier::BOLD)
}
