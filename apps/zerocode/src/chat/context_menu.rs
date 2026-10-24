use ratatui::layout::Rect;

use super::{CopyHitKind, CopyHitRegion, UrlHitRegion};
use crate::mouse;
use crate::path_open::is_path_target;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatContextMenuAction {
    SendNow,
    Copy,
    AddToChat,
    OpenLink,
    CopyLink,
    OpenPath,
    RevealPath,
    CopyPath,
    AddPathToChat,
    Edit,
    Delete,
}

pub(super) const TRANSCRIPT_CONTEXT_ACTIONS: &[ChatContextMenuAction] =
    &[ChatContextMenuAction::Copy];
pub(super) const CHARACTER_SELECTION_CONTEXT_ACTIONS: &[ChatContextMenuAction] = &[
    ChatContextMenuAction::AddToChat,
    ChatContextMenuAction::Copy,
];
const URL_CONTEXT_ACTIONS: &[ChatContextMenuAction] = &[
    ChatContextMenuAction::OpenLink,
    ChatContextMenuAction::CopyLink,
];
pub(super) const URL_WITH_COPY_CONTEXT_ACTIONS: &[ChatContextMenuAction] = &[
    ChatContextMenuAction::OpenLink,
    ChatContextMenuAction::CopyLink,
    ChatContextMenuAction::Copy,
];
pub(super) const PATH_CONTEXT_ACTIONS: &[ChatContextMenuAction] = &[
    ChatContextMenuAction::OpenPath,
    ChatContextMenuAction::RevealPath,
    ChatContextMenuAction::CopyPath,
    ChatContextMenuAction::AddPathToChat,
];
const PATH_WITH_COPY_CONTEXT_ACTIONS: &[ChatContextMenuAction] = &[
    ChatContextMenuAction::OpenPath,
    ChatContextMenuAction::RevealPath,
    ChatContextMenuAction::CopyPath,
    ChatContextMenuAction::AddPathToChat,
    ChatContextMenuAction::Copy,
];
pub(super) const QUEUE_CONTEXT_ACTIONS: &[ChatContextMenuAction] = &[
    ChatContextMenuAction::SendNow,
    ChatContextMenuAction::Copy,
    ChatContextMenuAction::Edit,
    ChatContextMenuAction::Delete,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChatContextMenuTarget {
    Transcript(CopyHitRegion),
    Url(UrlHitRegion),
    UrlWithCopy {
        url: UrlHitRegion,
        copy: CopyHitRegion,
    },
    Queue(u64),
}

impl ChatContextMenuTarget {
    pub(super) fn actions(&self) -> &'static [ChatContextMenuAction] {
        match self {
            Self::Transcript(target) if target.kind == CopyHitKind::Transcript => {
                CHARACTER_SELECTION_CONTEXT_ACTIONS
            }
            Self::Transcript(_) => TRANSCRIPT_CONTEXT_ACTIONS,
            Self::Url(url) if is_path_target(&url.url) => PATH_CONTEXT_ACTIONS,
            Self::Url(_) => URL_CONTEXT_ACTIONS,
            Self::UrlWithCopy { url, .. } if is_path_target(&url.url) => {
                PATH_WITH_COPY_CONTEXT_ACTIONS
            }
            Self::UrlWithCopy { .. } => URL_WITH_COPY_CONTEXT_ACTIONS,
            Self::Queue(_) => QUEUE_CONTEXT_ACTIONS,
        }
    }

    pub(super) fn copy_kind(&self) -> Option<CopyHitKind> {
        match self {
            Self::Transcript(copy) | Self::UrlWithCopy { copy, .. } => Some(copy.kind),
            Self::Url(_) | Self::Queue(_) => None,
        }
    }

    pub(super) fn is_url(&self) -> bool {
        matches!(self, Self::Url(_) | Self::UrlWithCopy { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChatContextMenu {
    pub(super) rect: Rect,
    pub(super) target: ChatContextMenuTarget,
    pub(super) selected: usize,
}

impl ChatContextMenu {
    pub(super) fn selected_action(&self) -> Option<ChatContextMenuAction> {
        self.target.actions().get(self.selected).copied()
    }

    pub(super) fn select_step(&mut self, delta: isize) {
        let count = self.target.actions().len();
        if count > 0 {
            self.selected = (self.selected as isize + delta).clamp(0, count as isize - 1) as usize;
        }
    }

    pub(super) fn select_at(&mut self, column: u16, row: u16) -> bool {
        let Some(index) = self.action_at(column, row) else {
            return false;
        };
        self.selected = index;
        true
    }

    fn action_at(&self, column: u16, row: u16) -> Option<usize> {
        if self.rect.width <= 2 || self.rect.height <= 2 {
            return None;
        }
        let inner = Rect::new(
            self.rect.x + 1,
            self.rect.y + 1,
            self.rect.width - 2,
            self.rect.height - 2,
        );
        if !mouse::in_rect(column, row, inner) {
            return None;
        }
        let index = usize::from(row.saturating_sub(inner.y));
        (index < self.target.actions().len()).then_some(index)
    }

    pub(super) fn into_request(self) -> Option<ChatContextMenuRequest> {
        let action = self.selected_action()?;
        match (self.target, action) {
            (ChatContextMenuTarget::Transcript(target), ChatContextMenuAction::Copy) => {
                Some(ChatContextMenuRequest::CopyTranscript(target))
            }
            (ChatContextMenuTarget::Transcript(target), ChatContextMenuAction::AddToChat)
                if target.kind == CopyHitKind::Transcript =>
            {
                Some(ChatContextMenuRequest::AddToChat(target))
            }
            (ChatContextMenuTarget::Url(url), ChatContextMenuAction::OpenLink)
            | (ChatContextMenuTarget::UrlWithCopy { url, .. }, ChatContextMenuAction::OpenLink) => {
                Some(ChatContextMenuRequest::OpenUrl(url.url))
            }
            (ChatContextMenuTarget::Url(url), ChatContextMenuAction::CopyLink)
            | (ChatContextMenuTarget::UrlWithCopy { url, .. }, ChatContextMenuAction::CopyLink) => {
                Some(ChatContextMenuRequest::CopyUrl(url.url))
            }
            (ChatContextMenuTarget::Url(url), ChatContextMenuAction::OpenPath)
            | (ChatContextMenuTarget::UrlWithCopy { url, .. }, ChatContextMenuAction::OpenPath)
                if is_path_target(&url.url) =>
            {
                Some(ChatContextMenuRequest::OpenUrl(url.url))
            }
            (ChatContextMenuTarget::Url(url), ChatContextMenuAction::RevealPath)
            | (ChatContextMenuTarget::UrlWithCopy { url, .. }, ChatContextMenuAction::RevealPath)
                if is_path_target(&url.url) =>
            {
                Some(ChatContextMenuRequest::RevealPath(url.url))
            }
            (ChatContextMenuTarget::Url(url), ChatContextMenuAction::CopyPath)
            | (ChatContextMenuTarget::UrlWithCopy { url, .. }, ChatContextMenuAction::CopyPath)
                if is_path_target(&url.url) =>
            {
                Some(ChatContextMenuRequest::CopyUrl(url.url))
            }
            (ChatContextMenuTarget::Url(url), ChatContextMenuAction::AddPathToChat)
            | (
                ChatContextMenuTarget::UrlWithCopy { url, .. },
                ChatContextMenuAction::AddPathToChat,
            ) if is_path_target(&url.url) => Some(ChatContextMenuRequest::AddPathToChat(url.url)),
            (ChatContextMenuTarget::UrlWithCopy { copy, .. }, ChatContextMenuAction::Copy) => {
                Some(ChatContextMenuRequest::CopyTranscript(copy))
            }
            (ChatContextMenuTarget::Queue(id), action) => {
                Some(ChatContextMenuRequest::Queue { id, action })
            }
            (ChatContextMenuTarget::Transcript(_), _)
            | (ChatContextMenuTarget::Url(_), _)
            | (ChatContextMenuTarget::UrlWithCopy { .. }, _) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChatContextMenuRequest {
    AddToChat(CopyHitRegion),
    CopyTranscript(CopyHitRegion),
    /// Open an HTTP(S) URL or a local path (`path_open` decides how).
    OpenUrl(String),
    /// Copy a URL or a local path.
    CopyUrl(String),
    RevealPath(String),
    AddPathToChat(String),
    Queue {
        id: u64,
        action: ChatContextMenuAction,
    },
}
