use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::layout::Rect;

use crate::chat;
use crate::client::RpcClient;

pub(crate) struct Acp<'a> {
    inner: chat::Chat<'a>,
}

impl<'a> Acp<'a> {
    pub(crate) fn new(rpc: &'a RpcClient) -> Self {
        Self {
            inner: chat::Chat::new(rpc, " ACP "),
        }
    }

    pub(crate) async fn init(&mut self) -> anyhow::Result<()> {
        self.inner.init().await
    }

    pub(crate) fn draw(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        self.inner.draw(frame, area);
    }

    pub(crate) async fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.inner.handle_key(key).await
    }

    pub(crate) fn wants_text_input(&self) -> bool {
        self.inner.wants_text_input()
    }

    pub(crate) fn handle_paste(&mut self, text: &str) {
        self.inner.handle_paste(text);
    }

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) {
        self.inner.handle_mouse(mouse, area);
    }

    pub(crate) fn help_lines(&self) -> Vec<(&str, &str)> {
        self.inner.help_lines()
    }
}
