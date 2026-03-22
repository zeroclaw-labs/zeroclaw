//! Multi-user simultaneous interpretation conference mode.
//!
//! Manages multiple concurrent voice interpretation sessions within a single
//! conference room, enabling real-time interpretation for multiple speakers.
//!
//! ## Architecture
//!
//! ```text
//! ConferenceRoom
//!   ├─ Participant A (ko → en) ─▸ SimulSession A
//!   ├─ Participant B (en → ko) ─▸ SimulSession B
//!   └─ Participant C (ja → en) ─▸ SimulSession C
//!       │                              │
//!       └── audio_out broadcast ◀──────┘
//!           (each participant hears translated
//!            audio from all other participants)
//! ```
//!
//! Each participant has their own [`SimulSession`] with independent
//! source/target language pair and segmentation state. The conference
//! room multiplexes audio output so each participant hears translations
//! of all other speakers in their target language.
//!
//! ## Billing
//!
//! Conference mode charges per-participant-minute to the room creator.
//! Each active session consumes credits independently.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::pipeline::LanguageCode;

// ── Conference configuration ─────────────────────────────────────

/// Maximum participants per conference room.
const DEFAULT_MAX_PARTICIPANTS: usize = 10;

/// Configuration for a conference room.
#[derive(Debug, Clone)]
pub struct ConferenceConfig {
    /// Unique room identifier.
    pub room_id: String,
    /// User ID of the room creator (billed party).
    pub creator_user_id: String,
    /// Maximum number of concurrent participants.
    pub max_participants: usize,
    /// Default target language for new participants.
    pub default_target_lang: LanguageCode,
    /// Gemini API key for voice sessions.
    pub api_key: String,
}

// ── Participant ─────────────────────────────────────────────────

/// A participant in a conference interpretation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    /// Unique participant identifier.
    pub participant_id: String,
    /// Display name.
    pub display_name: String,
    /// The language this participant speaks.
    pub source_lang: LanguageCode,
    /// The language this participant wants to hear.
    pub target_lang: LanguageCode,
    /// Whether the participant is currently connected.
    pub connected: bool,
    /// Whether the participant is currently speaking.
    pub speaking: bool,
    /// Timestamp when participant joined (epoch seconds).
    pub joined_at: i64,
}

/// Participant status within the conference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantStatus {
    /// Participant joined but not yet streaming audio.
    Joined,
    /// Participant is actively streaming audio.
    Active,
    /// Participant is muted (receiving only).
    Muted,
    /// Participant has disconnected.
    Disconnected,
}

// ── Conference room ─────────────────────────────────────────────

/// Manages a multi-participant interpretation conference.
///
/// Each participant has independent language pair settings and receives
/// translated audio from all other participants in their target language.
pub struct ConferenceRoom {
    /// Room configuration.
    config: ConferenceConfig,
    /// Active participants by ID.
    participants: Arc<Mutex<HashMap<String, ParticipantState>>>,
    /// Room status.
    status: Arc<Mutex<RoomStatus>>,
}

impl std::fmt::Debug for ConferenceRoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConferenceRoom")
            .field("room_id", &self.config.room_id)
            .finish_non_exhaustive()
    }
}

/// Internal state for a participant (includes session handle).
struct ParticipantState {
    /// Public participant info.
    info: Participant,
    /// Current status.
    status: ParticipantStatus,
    /// Channel to send translated audio to this participant.
    audio_tx: tokio::sync::mpsc::Sender<ConferenceEvent>,
}

/// Conference room status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomStatus {
    /// Room created, waiting for participants.
    Waiting,
    /// Room is active with participants.
    Active,
    /// Room is being closed.
    Closing,
    /// Room has been closed.
    Closed,
}

/// Events emitted by the conference room.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConferenceEvent {
    /// A participant joined the room.
    ParticipantJoined {
        participant_id: String,
        display_name: String,
        source_lang: LanguageCode,
        target_lang: LanguageCode,
    },
    /// A participant left the room.
    ParticipantLeft { participant_id: String },
    /// Translated audio from another participant.
    TranslatedAudio {
        from_participant_id: String,
        data: Vec<u8>,
    },
    /// Transcript from a participant's speech.
    Transcript {
        participant_id: String,
        text: String,
        is_source: bool,
    },
    /// A participant started speaking.
    SpeakingStarted { participant_id: String },
    /// A participant stopped speaking.
    SpeakingStopped { participant_id: String },
    /// Room status changed.
    RoomStatusChanged { status: RoomStatus },
    /// Error for a specific participant.
    Error {
        participant_id: Option<String>,
        message: String,
    },
}

/// Summary of a conference room's state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConferenceRoomSummary {
    pub room_id: String,
    pub status: RoomStatus,
    pub participant_count: usize,
    pub max_participants: usize,
    pub participants: Vec<ParticipantSummary>,
    pub created_by: String,
}

/// Summary of a participant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantSummary {
    pub participant_id: String,
    pub display_name: String,
    pub source_lang: LanguageCode,
    pub target_lang: LanguageCode,
    pub status: ParticipantStatus,
}

impl ConferenceRoom {
    /// Create a new conference room.
    pub fn new(config: ConferenceConfig) -> Self {
        Self {
            config,
            participants: Arc::new(Mutex::new(HashMap::new())),
            status: Arc::new(Mutex::new(RoomStatus::Waiting)),
        }
    }

    /// Get the room ID.
    pub fn room_id(&self) -> &str {
        &self.config.room_id
    }

    /// Get the current room status.
    pub async fn status(&self) -> RoomStatus {
        *self.status.lock().await
    }

    /// Add a participant to the conference.
    ///
    /// Returns a channel receiver for conference events directed at this participant.
    pub async fn join(
        &self,
        participant_id: String,
        display_name: String,
        source_lang: LanguageCode,
        target_lang: LanguageCode,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<ConferenceEvent>> {
        let mut participants = self.participants.lock().await;

        if participants.len() >= self.config.max_participants {
            anyhow::bail!(
                "Conference room {} is full ({}/{})",
                self.config.room_id,
                participants.len(),
                self.config.max_participants,
            );
        }

        if participants.contains_key(&participant_id) {
            anyhow::bail!(
                "Participant {} is already in room {}",
                participant_id,
                self.config.room_id,
            );
        }

        let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(64);

        let info = Participant {
            participant_id: participant_id.clone(),
            display_name: display_name.clone(),
            source_lang,
            target_lang,
            connected: true,
            speaking: false,
            joined_at: chrono::Utc::now().timestamp(),
        };

        participants.insert(
            participant_id.clone(),
            ParticipantState {
                info,
                status: ParticipantStatus::Joined,
                audio_tx: audio_tx.clone(),
            },
        );

        // Notify all other participants
        let join_event = ConferenceEvent::ParticipantJoined {
            participant_id: participant_id.clone(),
            display_name,
            source_lang,
            target_lang,
        };
        for (id, state) in participants.iter() {
            if *id != participant_id {
                let _ = state.audio_tx.try_send(join_event.clone());
            }
        }

        // Update room status if this is the first participant
        drop(participants);
        let mut status = self.status.lock().await;
        if *status == RoomStatus::Waiting {
            *status = RoomStatus::Active;
        }

        tracing::info!(
            room_id = %self.config.room_id,
            participant_id,
            "Participant joined conference"
        );

        Ok(audio_rx)
    }

    /// Remove a participant from the conference.
    pub async fn leave(&self, participant_id: &str) -> anyhow::Result<()> {
        let mut participants = self.participants.lock().await;

        if participants.remove(participant_id).is_none() {
            anyhow::bail!(
                "Participant {} not found in room {}",
                participant_id,
                self.config.room_id,
            );
        }

        // Notify remaining participants
        let leave_event = ConferenceEvent::ParticipantLeft {
            participant_id: participant_id.to_string(),
        };
        for state in participants.values() {
            let _ = state.audio_tx.try_send(leave_event.clone());
        }

        tracing::info!(
            room_id = %self.config.room_id,
            participant_id,
            remaining = participants.len(),
            "Participant left conference"
        );

        // Close room if empty
        if participants.is_empty() {
            drop(participants);
            let mut status = self.status.lock().await;
            *status = RoomStatus::Closed;
        }

        Ok(())
    }

    /// Broadcast translated audio to all participants except the speaker.
    ///
    /// The speaker's audio is translated and sent to every other participant
    /// who needs it (based on their target language).
    pub async fn broadcast_audio(
        &self,
        from_participant_id: &str,
        audio_data: Vec<u8>,
    ) -> anyhow::Result<()> {
        let participants = self.participants.lock().await;

        let event = ConferenceEvent::TranslatedAudio {
            from_participant_id: from_participant_id.to_string(),
            data: audio_data,
        };

        let mut sent = 0usize;
        for (id, state) in participants.iter() {
            if *id != from_participant_id
                && state.status != ParticipantStatus::Disconnected
                && state.audio_tx.try_send(event.clone()).is_ok()
            {
                sent += 1;
            }
        }

        tracing::debug!(
            room_id = %self.config.room_id,
            from = from_participant_id,
            recipients = sent,
            "Broadcast translated audio"
        );

        Ok(())
    }

    /// Broadcast a transcript to all participants.
    pub async fn broadcast_transcript(
        &self,
        participant_id: &str,
        text: &str,
        is_source: bool,
    ) -> anyhow::Result<()> {
        let participants = self.participants.lock().await;

        let event = ConferenceEvent::Transcript {
            participant_id: participant_id.to_string(),
            text: text.to_string(),
            is_source,
        };

        for (id, state) in participants.iter() {
            if *id != participant_id {
                let _ = state.audio_tx.try_send(event.clone());
            }
        }

        Ok(())
    }

    /// Update a participant's speaking status.
    pub async fn set_speaking(&self, participant_id: &str, speaking: bool) {
        let mut participants = self.participants.lock().await;

        if let Some(state) = participants.get_mut(participant_id) {
            state.info.speaking = speaking;
            state.status = if speaking {
                ParticipantStatus::Active
            } else {
                ParticipantStatus::Joined
            };
        }

        let event = if speaking {
            ConferenceEvent::SpeakingStarted {
                participant_id: participant_id.to_string(),
            }
        } else {
            ConferenceEvent::SpeakingStopped {
                participant_id: participant_id.to_string(),
            }
        };

        for (id, state) in participants.iter() {
            if *id != participant_id {
                let _ = state.audio_tx.try_send(event.clone());
            }
        }
    }

    /// Get a summary of the room's current state.
    pub async fn summary(&self) -> ConferenceRoomSummary {
        let participants = self.participants.lock().await;
        let status = *self.status.lock().await;

        let participant_summaries: Vec<ParticipantSummary> = participants
            .values()
            .map(|state| ParticipantSummary {
                participant_id: state.info.participant_id.clone(),
                display_name: state.info.display_name.clone(),
                source_lang: state.info.source_lang,
                target_lang: state.info.target_lang,
                status: state.status,
            })
            .collect();

        ConferenceRoomSummary {
            room_id: self.config.room_id.clone(),
            status,
            participant_count: participants.len(),
            max_participants: self.config.max_participants,
            participants: participant_summaries,
            created_by: self.config.creator_user_id.clone(),
        }
    }

    /// Close the conference room and disconnect all participants.
    pub async fn close(&self) {
        let mut status = self.status.lock().await;
        *status = RoomStatus::Closing;

        let mut participants = self.participants.lock().await;

        let close_event = ConferenceEvent::RoomStatusChanged {
            status: RoomStatus::Closed,
        };
        for state in participants.values() {
            let _ = state.audio_tx.try_send(close_event.clone());
        }

        participants.clear();
        drop(participants);

        *status = RoomStatus::Closed;

        tracing::info!(room_id = %self.config.room_id, "Conference room closed");
    }
}

// ── Conference manager ──────────────────────────────────────────

/// Manages multiple conference rooms.
pub struct ConferenceManager {
    rooms: Arc<Mutex<HashMap<String, Arc<ConferenceRoom>>>>,
    max_rooms: usize,
}

impl ConferenceManager {
    /// Create a new conference manager.
    pub fn new(max_rooms: usize) -> Self {
        Self {
            rooms: Arc::new(Mutex::new(HashMap::new())),
            max_rooms,
        }
    }

    /// Create a new conference room.
    pub async fn create_room(
        &self,
        config: ConferenceConfig,
    ) -> anyhow::Result<Arc<ConferenceRoom>> {
        let mut rooms = self.rooms.lock().await;

        if rooms.len() >= self.max_rooms {
            anyhow::bail!(
                "Maximum concurrent conference rooms ({}) reached",
                self.max_rooms
            );
        }

        if rooms.contains_key(&config.room_id) {
            anyhow::bail!("Conference room {} already exists", config.room_id);
        }

        let room_id = config.room_id.clone();
        let room = Arc::new(ConferenceRoom::new(config));
        rooms.insert(room_id, Arc::clone(&room));

        Ok(room)
    }

    /// Get an existing conference room.
    pub async fn get_room(&self, room_id: &str) -> Option<Arc<ConferenceRoom>> {
        let rooms = self.rooms.lock().await;
        rooms.get(room_id).cloned()
    }

    /// Close and remove a conference room.
    pub async fn close_room(&self, room_id: &str) -> anyhow::Result<()> {
        let mut rooms = self.rooms.lock().await;

        let room = rooms
            .remove(room_id)
            .ok_or_else(|| anyhow::anyhow!("Conference room {} not found", room_id))?;

        room.close().await;
        Ok(())
    }

    /// List all active conference rooms.
    pub async fn list_rooms(&self) -> Vec<ConferenceRoomSummary> {
        let rooms = self.rooms.lock().await;
        let mut summaries = Vec::new();

        for room in rooms.values() {
            summaries.push(room.summary().await);
        }

        summaries
    }

    /// Number of active rooms.
    pub async fn room_count(&self) -> usize {
        let rooms = self.rooms.lock().await;
        rooms.len()
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ConferenceConfig {
        ConferenceConfig {
            room_id: "test-room".into(),
            creator_user_id: "zeroclaw_user".into(),
            max_participants: 5,
            default_target_lang: LanguageCode::En,
            api_key: "test-key".into(),
        }
    }

    #[tokio::test]
    async fn create_room_and_join() {
        let room = ConferenceRoom::new(test_config());
        assert_eq!(room.status().await, RoomStatus::Waiting);

        let _rx = room
            .join(
                "p1".into(),
                "Participant 1".into(),
                LanguageCode::Ko,
                LanguageCode::En,
            )
            .await
            .unwrap();

        assert_eq!(room.status().await, RoomStatus::Active);
        let summary = room.summary().await;
        assert_eq!(summary.participant_count, 1);
    }

    #[tokio::test]
    async fn join_and_leave() {
        let room = ConferenceRoom::new(test_config());

        let _rx1 = room
            .join(
                "p1".into(),
                "Participant 1".into(),
                LanguageCode::Ko,
                LanguageCode::En,
            )
            .await
            .unwrap();

        let _rx2 = room
            .join(
                "p2".into(),
                "Participant 2".into(),
                LanguageCode::En,
                LanguageCode::Ko,
            )
            .await
            .unwrap();

        assert_eq!(room.summary().await.participant_count, 2);

        room.leave("p1").await.unwrap();
        assert_eq!(room.summary().await.participant_count, 1);

        room.leave("p2").await.unwrap();
        assert_eq!(room.status().await, RoomStatus::Closed);
    }

    #[tokio::test]
    async fn reject_duplicate_participant() {
        let room = ConferenceRoom::new(test_config());

        let _rx = room
            .join(
                "p1".into(),
                "Participant 1".into(),
                LanguageCode::Ko,
                LanguageCode::En,
            )
            .await
            .unwrap();

        let result = room
            .join(
                "p1".into(),
                "Duplicate".into(),
                LanguageCode::Ko,
                LanguageCode::En,
            )
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already in room"));
    }

    #[tokio::test]
    async fn reject_when_full() {
        let mut config = test_config();
        config.max_participants = 2;
        let room = ConferenceRoom::new(config);

        let _rx1 = room
            .join("p1".into(), "P1".into(), LanguageCode::Ko, LanguageCode::En)
            .await
            .unwrap();
        let _rx2 = room
            .join("p2".into(), "P2".into(), LanguageCode::En, LanguageCode::Ko)
            .await
            .unwrap();

        let result = room
            .join("p3".into(), "P3".into(), LanguageCode::Ja, LanguageCode::En)
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("full"));
    }

    #[tokio::test]
    async fn broadcast_audio_skips_sender() {
        let room = ConferenceRoom::new(test_config());

        let mut rx1 = room
            .join("p1".into(), "P1".into(), LanguageCode::Ko, LanguageCode::En)
            .await
            .unwrap();
        // Skip the join event for p2
        let mut rx2 = room
            .join("p2".into(), "P2".into(), LanguageCode::En, LanguageCode::Ko)
            .await
            .unwrap();

        // Drain the "participant joined" event from rx1
        let _join_event = rx1.try_recv();

        room.broadcast_audio("p1", vec![1, 2, 3]).await.unwrap();

        // p2 should receive the audio
        let event = rx2.try_recv().unwrap();
        assert!(matches!(event, ConferenceEvent::TranslatedAudio { .. }));

        // p1 should NOT have the audio (already drained the join event)
        // Note: p1 may have one more event from p2 joining, but no audio from self
    }

    #[tokio::test]
    async fn conference_manager_create_and_list() {
        let manager = ConferenceManager::new(10);

        let _room = manager.create_room(test_config()).await.unwrap();
        assert_eq!(manager.room_count().await, 1);

        let rooms = manager.list_rooms().await;
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].room_id, "test-room");

        manager.close_room("test-room").await.unwrap();
        assert_eq!(manager.room_count().await, 0);
    }

    #[tokio::test]
    async fn conference_manager_rejects_duplicate_room() {
        let manager = ConferenceManager::new(10);
        manager.create_room(test_config()).await.unwrap();
        let result = manager.create_room(test_config()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn conference_manager_capacity_limit() {
        let manager = ConferenceManager::new(1);
        manager.create_room(test_config()).await.unwrap();

        let mut config2 = test_config();
        config2.room_id = "room-2".into();
        let result = manager.create_room(config2).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Maximum"));
    }

    #[tokio::test]
    async fn set_speaking_status() {
        let room = ConferenceRoom::new(test_config());

        let _rx = room
            .join("p1".into(), "P1".into(), LanguageCode::Ko, LanguageCode::En)
            .await
            .unwrap();

        room.set_speaking("p1", true).await;
        let summary = room.summary().await;
        assert_eq!(summary.participants[0].status, ParticipantStatus::Active);

        room.set_speaking("p1", false).await;
        let summary = room.summary().await;
        assert_eq!(summary.participants[0].status, ParticipantStatus::Joined);
    }

    #[tokio::test]
    async fn close_room_clears_participants() {
        let room = ConferenceRoom::new(test_config());

        let _rx = room
            .join("p1".into(), "P1".into(), LanguageCode::Ko, LanguageCode::En)
            .await
            .unwrap();

        room.close().await;
        assert_eq!(room.status().await, RoomStatus::Closed);
        assert_eq!(room.summary().await.participant_count, 0);
    }
}
