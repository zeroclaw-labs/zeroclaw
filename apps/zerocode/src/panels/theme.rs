//! Theme picker panel.

use std::path::Path;

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::panel::{Panel, PanelOutcome};
use crate::theme_picker::{ThemePicker, ThemePickerOutcome};
use crate::widgets::{HelpEntry, HelpNode};

pub struct ThemePanel {
    picker: ThemePicker,
}

impl ThemePanel {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            picker: ThemePicker::new(config_dir),
        }
    }
}

#[async_trait::async_trait]
impl Panel for ThemePanel {
    fn id(&self) -> &'static str {
        "theme"
    }

    fn title_key(&self) -> &'static str {
        "zc-pane-theme"
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        self.picker.draw_panel(frame, area);
    }

    async fn handle_key(&mut self, key: KeyEvent) -> PanelOutcome {
        match self.picker.handle_key(key) {
            ThemePickerOutcome::Continue | ThemePickerOutcome::Confirmed(_) => {}
            ThemePickerOutcome::Cancelled => {}
        }
        PanelOutcome::Continue
    }

    fn wants_text_input(&self) -> bool {
        true
    }

    fn help_context(&self) -> HelpNode {
        HelpNode {
            title: Some(crate::i18n::t("zc-pane-theme")),
            description: Some(crate::i18n::t("zc-theme-panel-help-desc")),
            entries: vec![
                HelpEntry::new(["↑", "↓"], crate::i18n::t("zc-theme-help-preview")),
                HelpEntry::new(["Type"], crate::i18n::t("zc-theme-help-filter")),
                HelpEntry::new(["Enter"], crate::i18n::t("zc-theme-help-save")),
                HelpEntry::new(["Esc"], crate::i18n::t("zc-theme-help-cancel")),
            ],
            children: vec![],
        }
    }
}
