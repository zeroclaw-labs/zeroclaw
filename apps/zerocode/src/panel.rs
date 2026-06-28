//! Pluggable panel system for the zerocode TUI.
//!
//! The built-in panes (Dashboard, Config, Code, Chat, Logs, Doctor,
//! Quickstart) stay concrete and hand-wired in [`crate::app`]. This module
//! adds an *additive* extension point: a [`Panel`] trait plus a single
//! registration site ([`register_panels`]) so further panels can be
//! surfaced as their own top-level modes without editing the core dispatch
//! by hand.
//!
//! Panels are in-process `Box<dyn Panel>` values (no dynamic library or
//! WASM loading). Runtime/agent-driven content is delivered by panels that
//! subscribe to daemon notifications (see [`crate::panels::canvas_panel`]),
//! not by loading external code.

use std::sync::Arc;

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::client::RpcClient;
use crate::widgets::HelpNode;

/// What the event loop should do after a panel handles a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelOutcome {
    /// Keep running.
    Continue,
    /// Quit the application (mirrors a built-in pane returning `true`).
    Quit,
}

/// A registrable TUI panel, surfaced as its own entry in the mode bar.
///
/// The trait is deliberately decoupled from the app's internals: panels
/// never see the terminal handle, the connection state, or `anyhow::Result`
/// in their key path. A panel that needs to talk to the daemon captures an
/// [`Arc<RpcClient>`] at construction.
#[async_trait::async_trait]
pub trait Panel: Send {
    /// Stable identifier, e.g. `"report"` or `"finance"`. Used for routing
    /// and (for canvas panels) matching daemon notifications.
    fn id(&self) -> &'static str;

    /// Fluent message key for the mode-bar label, e.g. `"zc-pane-report"`.
    fn title_key(&self) -> &'static str;

    /// One-time async setup against the live daemon. Runs at startup and
    /// again after each reconnect (panels are rebuilt against the recovered
    /// client). Default: no-op.
    async fn init(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Render the panel into `area`.
    fn draw(&mut self, frame: &mut Frame, area: Rect);

    /// Handle a key event. Default: ignore and continue.
    async fn handle_key(&mut self, _key: KeyEvent) -> PanelOutcome {
        PanelOutcome::Continue
    }

    /// Periodic refresh tick (fires when no input arrives) for live panels.
    /// Default: no-op.
    async fn tick(&mut self) {}

    /// Whether the panel is currently capturing text input. While `true`,
    /// global single-key shortcuts (help, reload) are suppressed. Default:
    /// `false`.
    fn wants_text_input(&self) -> bool {
        false
    }

    /// Contribution to the help modal.
    fn help_context(&self) -> HelpNode;

    /// Handle pasted text. Default: ignore.
    fn handle_paste(&mut self, _text: &str) {}
}

/// Build the ordered set of registered panels for this session.
///
/// This is the single registration site. To add a panel, construct it here;
/// it appears in the mode bar after the built-in panes, in the order
/// returned. Called once at startup and again on every reconnect so each
/// panel binds to the live [`RpcClient`].
pub fn register_panels(rpc: Arc<RpcClient>) -> Vec<Box<dyn Panel>> {
    vec![Box::new(crate::panels::report::ReportPanel::new(rpc.clone()))]
}
