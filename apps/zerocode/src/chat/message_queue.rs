use std::collections::VecDeque;

use crate::attachment::PendingAttachment;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueueItemStatus {
    Pending,
    Injected,
}

#[derive(Debug, Clone)]
pub(crate) struct QueuedMessage {
    pub id: u64,
    pub text: String,
    pub attachments: Vec<PendingAttachment>,
    pub status: QueueItemStatus,
}

/// The queue owns both its pause and the explanation shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueuePauseReason {
    Generic,
    MissingCompletion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdmissionError {
    Empty,
    Full,
}

/// Client-owned queue state. The caller supplies live turn state and owns
/// attachment cleanup when a message leaves the queue.
#[derive(Debug, Clone, Default)]
pub(super) struct MessageQueue {
    items: VecDeque<QueuedMessage>,
    next_id: u64,
    paused: Option<QueuePauseReason>,
    resume_override: bool,
    selected: Option<u64>,
}

impl MessageQueue {
    pub(super) const CAPACITY: usize = 32;

    pub(super) fn items(&self) -> &VecDeque<QueuedMessage> {
        &self.items
    }

    pub(super) fn len(&self) -> usize {
        self.items.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(super) fn pause_reason(&self) -> Option<QueuePauseReason> {
        self.paused
    }

    pub(super) fn paused(&self) -> bool {
        self.paused.is_some()
    }

    pub(super) fn selected(&self) -> Option<u64> {
        self.selected
    }

    pub(super) fn selected_id(&self) -> Option<u64> {
        self.selected.filter(|id| self.message(*id).is_some())
    }

    pub(super) fn message(&self, id: u64) -> Option<&QueuedMessage> {
        self.items.iter().find(|message| message.id == id)
    }

    #[cfg(test)]
    pub(super) fn resume_override(&self) -> bool {
        self.resume_override
    }

    pub(super) fn enqueue(
        &mut self,
        text: String,
        attachments: Vec<PendingAttachment>,
    ) -> Result<(), (AdmissionError, Vec<PendingAttachment>)> {
        self.admit(text, attachments, QueueItemStatus::Pending)
    }

    pub(super) fn inject(
        &mut self,
        text: String,
        attachments: Vec<PendingAttachment>,
        turn_in_flight: bool,
    ) -> Result<(), (AdmissionError, Vec<PendingAttachment>)> {
        self.admit(text, attachments, QueueItemStatus::Injected)?;
        // Force-send intent survives the live turn's cancel auto-pause.
        self.resume();
        if turn_in_flight {
            self.resume_override = true;
        }
        Ok(())
    }

    fn admit(
        &mut self,
        text: String,
        attachments: Vec<PendingAttachment>,
        status: QueueItemStatus,
    ) -> Result<(), (AdmissionError, Vec<PendingAttachment>)> {
        if text.trim().is_empty() && attachments.is_empty() {
            return Err((AdmissionError::Empty, attachments));
        }
        if self.items.len() >= Self::CAPACITY {
            return Err((AdmissionError::Full, attachments));
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let message = QueuedMessage {
            id,
            text,
            attachments,
            status,
        };
        match status {
            QueueItemStatus::Pending => self.items.push_back(message),
            QueueItemStatus::Injected => self.items.insert(self.injected_insert_at(), message),
        }
        Ok(())
    }

    fn injected_insert_at(&self) -> usize {
        self.items
            .iter()
            .position(|message| message.status == QueueItemStatus::Pending)
            .unwrap_or(self.items.len())
    }

    pub(super) fn next_dispatch_index(&self, turn_in_flight: bool) -> Option<usize> {
        if turn_in_flight {
            return None;
        }
        if let Some(index) = self
            .items
            .iter()
            .position(|message| message.status == QueueItemStatus::Injected)
        {
            return Some(index);
        }
        if self.paused() {
            return None;
        }
        self.items
            .iter()
            .position(|message| message.status == QueueItemStatus::Pending)
    }

    pub(super) fn take_next_dispatchable(&mut self, turn_in_flight: bool) -> Option<QueuedMessage> {
        let index = self.next_dispatch_index(turn_in_flight)?;
        let message = self.items.remove(index)?;
        self.reset_resume_override();
        if self.selected == Some(message.id) {
            self.selected = None;
        }
        Some(message)
    }

    pub(super) fn toggle_pause(&mut self) -> bool {
        self.paused = if self.paused() {
            None
        } else {
            Some(QueuePauseReason::Generic)
        };
        self.paused()
    }

    pub(super) fn resume(&mut self) -> bool {
        let was_paused = self.paused();
        self.paused = None;
        was_paused
    }

    pub(super) fn acknowledge_terminal_notification(&mut self) {
        if self.paused == Some(QueuePauseReason::MissingCompletion) {
            self.paused = Some(QueuePauseReason::Generic);
        }
    }

    pub(super) fn settle_turn(&mut self, clean: bool, pause_reason: QueuePauseReason) {
        if !clean && !self.resume_override && !self.items.is_empty() {
            self.paused = Some(pause_reason);
        }
        self.reset_resume_override();
    }

    pub(super) fn reset_resume_override(&mut self) {
        self.resume_override = false;
    }

    pub(super) fn restore(&mut self, mut snapshot: Self) {
        snapshot.selected = snapshot.selected_id();
        snapshot.reset_resume_override();
        *self = snapshot;
    }

    pub(super) fn ensure_selection(&mut self) {
        if self.selected.is_none()
            && let Some(front) = self.items.front()
        {
            self.selected = Some(front.id);
        }
    }

    pub(super) fn select(&mut self, id: u64) -> bool {
        if self.message(id).is_some() && self.selected != Some(id) {
            self.selected = Some(id);
            true
        } else {
            false
        }
    }

    pub(super) fn select_step(&mut self, delta: isize) -> bool {
        if self.items.is_empty() {
            self.selected = None;
            return false;
        }
        let current = self
            .selected
            .and_then(|id| self.items.iter().position(|message| message.id == id))
            .unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(self.items.len() as isize) as usize;
        self.selected = Some(self.items[next].id);
        true
    }

    /// Returns whether status or pause changed, or `None` for an unknown id.
    pub(super) fn promote(&mut self, id: u64, turn_in_flight: bool) -> Option<bool> {
        let position = self.items.iter().position(|message| message.id == id)?;
        let pending = self.items[position].status == QueueItemStatus::Pending;
        if pending {
            let mut message = self.items.remove(position)?;
            message.status = QueueItemStatus::Injected;
            self.items.insert(self.injected_insert_at(), message);
        }
        let resumed = self.resume();
        if turn_in_flight {
            self.resume_override = true;
        }
        Some(pending || resumed)
    }

    pub(super) fn delete(&mut self, id: u64) -> Option<QueuedMessage> {
        let position = self.items.iter().position(|message| message.id == id)?;
        let message = self.items.remove(position)?;
        // Deleting any item selects the nearest survivor, even if another
        // item was selected before the removal.
        self.select_nearest(position);
        Some(message)
    }

    pub(super) fn take_for_edit(&mut self, id: u64) -> Option<QueuedMessage> {
        let position = self.items.iter().position(|message| message.id == id)?;
        let message = self.items.remove(position)?;
        self.selected = self.items.front().map(|message| message.id);
        Some(message)
    }

    pub(super) fn remove_at(&mut self, position: usize) -> Option<QueuedMessage> {
        let message = self.items.remove(position)?;
        // Slash removal preserves an unrelated selection.
        if self.selected == Some(message.id) {
            self.select_nearest(position);
        }
        Some(message)
    }

    fn select_nearest(&mut self, position: usize) {
        self.selected = self
            .items
            .get(position.min(self.items.len().saturating_sub(1)))
            .map(|message| message.id);
    }

    pub(super) fn clear(&mut self) -> VecDeque<QueuedMessage> {
        let messages = std::mem::take(&mut self.items);
        self.next_id = 0;
        self.paused = None;
        self.reset_resume_override();
        self.selected = None;
        messages
    }
}
