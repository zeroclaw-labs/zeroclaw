use std::sync::Arc;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::layout::Rect;

use crate::chat;
use crate::client::RpcClient;

/// ACP pane — displayed as "Code" in the UI; internal name kept for historical reasons.
pub(crate) struct Acp {
    inner: chat::Chat,
}

impl Acp {
    pub(crate) fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            inner: chat::Chat::new(rpc, chat::PaneKind::Acp),
        }
    }

    pub(crate) async fn init(&mut self) -> anyhow::Result<()> {
        self.inner.init().await
    }

    pub(crate) async fn refresh_if_inactive(&mut self) {
        self.inner.refresh_if_inactive().await;
    }

    pub(crate) fn draw(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        self.inner.draw(frame, area);
    }

    pub(crate) async fn handle_key(
        &mut self,
        key: KeyEvent,
        term: &mut crate::config_manager::Term,
    ) -> bool {
        self.inner.handle_key(key, term).await
    }

    pub(crate) fn wants_text_input(&self) -> bool {
        self.inner.wants_text_input()
    }

    pub(crate) async fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) {
        self.inner.handle_mouse(mouse, area).await;
    }

    pub(crate) fn handle_paste(&mut self, text: &str) {
        self.inner.handle_paste(text);
    }

    pub(crate) fn ctx_tokens(&self) -> (Option<u64>, Option<u64>) {
        self.inner.ctx_tokens()
    }

    pub(crate) fn selected_agent(&self) -> Option<&str> {
        self.inner.selected_agent()
    }
}

impl crate::widgets::HelpContext for Acp {
    fn help_context(&self) -> crate::widgets::HelpNode {
        self.inner.help_context()
    }
}
