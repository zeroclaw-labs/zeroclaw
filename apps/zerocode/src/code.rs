use std::sync::Arc;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::layout::Rect;

use crate::client::RpcClient;
use crate::config::UiProfile;
use crate::transcript;

pub(crate) struct Code {
    inner: transcript::Transcript,
}

impl Code {
    pub(crate) fn new(rpc: Arc<RpcClient>, ui_profile: UiProfile) -> Self {
        Self {
            inner: transcript::Transcript::new(rpc, transcript::PaneKind::Code, ui_profile),
        }
    }

    pub(crate) async fn init(&mut self) -> anyhow::Result<()> {
        self.inner.init().await
    }

    pub(crate) fn set_ui_profile(&mut self, profile: UiProfile) {
        self.inner.set_ui_profile(profile);
    }

    pub(crate) fn set_adaptive_sidebar_visible(&mut self, visible: bool) {
        self.inner.set_adaptive_sidebar_visible(visible);
    }

    pub(crate) fn take_ui_command(&mut self) -> Option<transcript::TranscriptUiCommand> {
        self.inner.take_ui_command()
    }

    pub(crate) fn set_resume_session_id(&mut self, sid: Option<String>) {
        self.inner.set_resume_session_id(sid);
    }

    pub(crate) fn set_resume_agent_alias(&mut self, alias: Option<String>) {
        self.inner.set_resume_agent_alias(alias);
    }

    pub(crate) fn current_session_id(&self) -> Option<&str> {
        self.inner.current_session_id()
    }

    pub(crate) fn current_agent_alias(&self) -> Option<&str> {
        self.inner.current_agent_alias()
    }

    pub(crate) async fn refresh_if_inactive(&mut self) {
        self.inner.refresh_if_inactive().await;
    }

    pub(crate) async fn focus_agent(&mut self, alias: &str) {
        self.inner.focus_agent(alias).await;
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

    pub(crate) fn wants_quit_chord(&self) -> bool {
        self.inner.wants_quit_chord()
    }

    pub(crate) fn clear_input(&mut self) {
        self.inner.clear_input();
    }

    pub(crate) fn in_browse_mode(&self) -> bool {
        self.inner.in_browse_mode()
    }

    pub(crate) fn exit_browse_mode(&mut self) {
        self.inner.exit_browse_mode();
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

    pub(crate) fn info_message(&mut self) -> Option<&crate::widgets::InfoMessage> {
        self.inner.info_message()
    }

    pub(crate) fn set_info_notice(&mut self, msg: String) {
        self.inner.set_info_notice(msg);
    }
}

impl crate::widgets::HelpContext for Code {
    fn help_context(&self) -> crate::widgets::HelpNode {
        self.inner.help_context()
    }
}
