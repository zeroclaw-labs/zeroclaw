// Post-Call Processing — record call outcome to brain (v3.0 Section B)
//
// After a phone call ends:
// 1. Insert phone_calls row (call metadata)
// 2. Append memory_timeline entry (transcript as evidence)
// 3. Create ontology Action ("phone_call")
// 4. Set needs_recompile=1 on linked memory → Dream Cycle rewrites truth
// 5. Record to delta journal for cross-device sync

use anyhow::{Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::memory::sqlite::SqliteMemory;
use crate::ontology::repo::OntologyRepo;
use crate::ontology::types::ActorKind;

/// Input data for post-call processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostCallData {
    /// Unique call identifier.
    pub call_uuid: String,
    /// Call direction: "in", "out", or "missed".
    pub direction: String,
    /// Raw caller number.
    pub caller_number: Option<String>,
    /// E.164 normalized caller number.
    pub caller_number_e164: Option<String>,
    /// Matched ontology object ID (from caller_match).
    pub caller_object_id: Option<i64>,
    /// Call start time (Unix timestamp seconds).
    pub started_at: u64,
    /// Call end time (Unix timestamp seconds).
    pub ended_at: Option<u64>,
    /// Call duration in milliseconds.
    pub duration_ms: Option<u64>,
    /// GPS latitude at time of call.
    pub gps_lat: Option<f64>,
    /// GPS longitude at time of call.
    pub gps_lon: Option<f64>,
    /// Full transcript from STT.
    pub transcript: Option<String>,
    /// AI-generated summary of the call.
    pub summary: Option<String>,
    /// Risk level: "safe", "warn", "danger".
    pub risk_level: String,
    /// Whether SOS was triggered during the call.
    pub sos_triggered: bool,
    /// Detected language (e.g. "ko", "en").
    pub language: Option<String>,
    /// Linked memory key (for compiled truth updates).
    pub memory_key: Option<String>,
    /// Linked memory ID (for timeline append).
    pub memory_id: Option<String>,
    /// Device that handled the call.
    pub device_id: String,
    /// Owner user ID.
    pub owner_user_id: String,
    /// Home timezone (IANA) for ontology action timestamps.
    pub home_timezone: String,
}

/// Result of post-call processing.
#[derive(Debug)]
pub struct PostCallResult {
    /// Whether phone_calls row was inserted.
    pub call_recorded: bool,
    /// UUID of the timeline entry (if transcript was available).
    pub timeline_uuid: Option<String>,
    /// ID of the ontology action (if object was matched).
    pub action_id: Option<i64>,
    /// Whether the memory was flagged for recompilation.
    pub recompile_flagged: bool,
}

/// Process post-call data: record to phone_calls, timeline, ontology, and flag recompile.
pub fn process_post_call(
    memory: &SqliteMemory,
    ontology: Option<&OntologyRepo>,
    data: &PostCallData,
) -> Result<PostCallResult> {
    let mut result = PostCallResult {
        call_recorded: false,
        timeline_uuid: None,
        action_id: None,
        recompile_flagged: false,
    };

    // 1. Insert phone_calls row
    insert_phone_call(memory, data)
        .context("failed to insert phone_calls row")?;
    result.call_recorded = true;

    // 2. Append to memory_timeline (if we have a transcript and memory_id)
    if let (Some(ref transcript), Some(ref memory_id)) = (&data.transcript, &data.memory_id) {
        if !transcript.trim().is_empty() {
            let metadata = serde_json::json!({
                "duration_ms": data.duration_ms,
                "gps_lat": data.gps_lat,
                "gps_lon": data.gps_lon,
                "caller_number": data.caller_number_e164,
                "language": data.language,
                "risk_level": data.risk_level,
            });

            let uuid = memory.append_timeline(
                memory_id,
                "call",
                data.started_at,
                &data.call_uuid,
                transcript,
                Some(&metadata.to_string()),
                &data.device_id,
            )?;
            result.timeline_uuid = Some(uuid);
        }
    }

    // 3. Create ontology Action (if we have a matched object)
    if let (Some(repo), Some(object_id)) = (ontology, data.caller_object_id) {
        let action_params = serde_json::json!({
            "call_uuid": data.call_uuid,
            "direction": data.direction,
            "duration_ms": data.duration_ms,
            "risk_level": data.risk_level,
            "language": data.language,
            "has_transcript": data.transcript.is_some(),
        });

        let occurred_at = chrono::DateTime::from_timestamp(data.started_at as i64, 0)
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string());

        match repo.insert_action_pending(
            "phone_call",
            &data.owner_user_id,
            &ActorKind::System,
            Some(object_id),
            &[],
            &action_params,
            Some("phone"),
            None,
            occurred_at.as_deref(),
            data.gps_lat.map(|lat| format!("{lat},{}", data.gps_lon.unwrap_or(0.0))).as_deref(),
            &data.home_timezone,
        ) {
            Ok(id) => result.action_id = Some(id),
            Err(e) => tracing::warn!("Failed to create phone_call ontology action: {e}"),
        }
    }

    // 4. Flag memory for recompilation (Dream Cycle will rewrite compiled_truth)
    if let Some(ref key) = data.memory_key {
        memory.mark_needs_recompile(key)?;
        result.recompile_flagged = true;
    }

    tracing::info!(
        call_uuid = data.call_uuid,
        direction = data.direction,
        caller = data.caller_number_e164.as_deref().unwrap_or("unknown"),
        recorded = result.call_recorded,
        timeline = result.timeline_uuid.is_some(),
        action = result.action_id.is_some(),
        recompile = result.recompile_flagged,
        "Post-call processing complete"
    );

    Ok(result)
}

/// Insert a phone_calls row into SQLite.
fn insert_phone_call(memory: &SqliteMemory, data: &PostCallData) -> Result<()> {
    let conn = memory.connection();
    conn.execute(
        "INSERT INTO phone_calls
            (call_uuid, direction, caller_number, caller_number_e164, caller_object_id,
             started_at, ended_at, duration_ms, gps_lat, gps_lon,
             transcript, summary, risk_level, sos_triggered, language,
             memory_id, device_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            data.call_uuid,
            data.direction,
            data.caller_number,
            data.caller_number_e164,
            data.caller_object_id,
            data.started_at as i64,
            data.ended_at.map(|t| t as i64),
            data.duration_ms.map(|t| t as i64),
            data.gps_lat,
            data.gps_lon,
            data.transcript,
            data.summary,
            data.risk_level,
            data.sos_triggered as i32,
            data.language,
            data.memory_id,
            data.device_id,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, SqliteMemory) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        (tmp, mem)
    }

    fn sample_data() -> PostCallData {
        PostCallData {
            call_uuid: "call-001".to_string(),
            direction: "in".to_string(),
            caller_number: Some("010-1234-5678".to_string()),
            caller_number_e164: Some("+821012345678".to_string()),
            caller_object_id: None,
            started_at: 1700000000,
            ended_at: Some(1700000300),
            duration_ms: Some(300_000),
            gps_lat: Some(37.5665),
            gps_lon: Some(126.9780),
            transcript: Some("안녕하세요, 상담 관련 전화드립니다.".to_string()),
            summary: Some("상담 문의 전화".to_string()),
            risk_level: "safe".to_string(),
            sos_triggered: false,
            language: Some("ko".to_string()),
            memory_key: None,
            memory_id: None,
            device_id: "device_a".to_string(),
            owner_user_id: "user1".to_string(),
            home_timezone: "Asia/Seoul".to_string(),
        }
    }

    #[test]
    fn insert_phone_call_basic() {
        let (_tmp, mem) = setup();
        let data = sample_data();
        insert_phone_call(&mem, &data).unwrap();

        // Verify it was inserted
        let conn = mem.conn_for_test();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM phone_calls WHERE call_uuid = ?1", params!["call-001"], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn insert_phone_call_minimal() {
        let (_tmp, mem) = setup();
        let data = PostCallData {
            call_uuid: "call-min".to_string(),
            direction: "missed".to_string(),
            caller_number: None,
            caller_number_e164: None,
            caller_object_id: None,
            started_at: 1700000000,
            ended_at: None,
            duration_ms: None,
            gps_lat: None,
            gps_lon: None,
            transcript: None,
            summary: None,
            risk_level: "safe".to_string(),
            sos_triggered: false,
            language: None,
            memory_key: None,
            memory_id: None,
            device_id: "dev1".to_string(),
            owner_user_id: "user1".to_string(),
            home_timezone: "Asia/Seoul".to_string(),
        };
        insert_phone_call(&mem, &data).unwrap();
    }

    #[tokio::test]
    async fn process_post_call_records_call() {
        let (_tmp, mem) = setup();
        let data = sample_data();
        let result = process_post_call(&mem, None, &data).unwrap();
        assert!(result.call_recorded);
        assert!(result.timeline_uuid.is_none()); // no memory_id linked
        assert!(result.action_id.is_none()); // no ontology repo
        assert!(!result.recompile_flagged); // no memory_key
    }

    #[tokio::test]
    async fn process_post_call_with_timeline() {
        let (_tmp, mem) = setup();
        use crate::memory::traits::Memory;

        // Create a memory entry to link to
        mem.store("client_a", "Client A info", crate::memory::traits::MemoryCategory::Core, None)
            .await
            .unwrap();
        let entry = mem.get("client_a").await.unwrap().unwrap();

        let mut data = sample_data();
        data.memory_id = Some(entry.id.clone());
        data.memory_key = Some("client_a".to_string());

        let result = process_post_call(&mem, None, &data).unwrap();
        assert!(result.call_recorded);
        assert!(result.timeline_uuid.is_some());
        assert!(result.recompile_flagged);

        // Verify timeline was appended
        let timeline = mem.get_timeline(&entry.id, 10).unwrap();
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].event_type, "call");
        assert_eq!(timeline[0].source_ref, "call-001");
    }

    #[test]
    fn post_call_data_serialization() {
        let data = sample_data();
        let json = serde_json::to_string(&data).unwrap();
        let parsed: PostCallData = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.call_uuid, "call-001");
    }
}
