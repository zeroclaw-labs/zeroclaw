//! Process-wide registry of sent SOP gate prompts, keyed by gate reference.
//!
//! PROCESS-WIDE, not per-channel-instance, deliberately: the daemon builds more
//! than one channel map today (the SOP approval route adapter's map in
//! `build_sop_adapters` and the orchestrator's own), so the `DiscordChannel`
//! that SENDS a gate prompt and the one that later FINALIZES it are different
//! instances of the same alias. A per-instance registry made the finalize a
//! silent no-op (the resolved gate's embed never updated). One shared registry
//! makes finalize instance-agnostic while keeping credentials on the live
//! `DiscordChannel`: each record carries the sending alias so only the matching
//! channel instance can PATCH the message with its current owner token.
//!
//! In-memory only: a restart loses the mapping, after which finalize no-ops and
//! the stale buttons resolve as already-answered via the marker path — the same
//! degraded mode as before, never a wrong edit.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Registry entries older than this are swept on the next insert: a gate
/// parked this long has almost certainly been resolved through another surface
/// (or its run reaped), and an unswept registry would otherwise grow for the
/// daemon's whole lifetime. A swept entry only degrades finalize to a no-op —
/// the same mode as a restart, never a wrong edit.
const SWEEP_AFTER: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// One sent gate prompt: where its message lives, which configured alias sent
/// it, and any input-bearing choices (for modal pre-fill).
#[derive(Clone)]
pub(crate) struct GatePromptRecord {
    pub(crate) channel_alias: String,
    pub(crate) channel_id: String,
    pub(crate) message_id: String,
    pub(crate) title: String,
    /// Body the finalized embed keeps (the approval context, minus the reply
    /// instructions); the outcome line is appended under it so the record of
    /// WHAT was approved survives resolution. `None` = outcome-only.
    pub(crate) resolved_description: Option<String>,
    /// Input-bearing choices (Edit / Revise) so a live process can pre-fill
    /// their modals. Best-effort: lost on restart, after which the modal opens
    /// blank (the draft is still readable in the embed).
    pub(crate) inputs: Vec<GatePromptInput>,
}

impl std::fmt::Debug for GatePromptRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatePromptRecord")
            .field("channel_alias", &self.channel_alias)
            .field("channel_id", &self.channel_id)
            .field("message_id", &self.message_id)
            .field("title", &self.title)
            .field("resolved_description", &self.resolved_description)
            .field("inputs", &self.inputs)
            .finish()
    }
}

/// The text-collection spec of one input-bearing choice on a sent prompt.
#[derive(Debug, Clone)]
pub(crate) struct GatePromptInput {
    pub(crate) choice_id: String,
    pub(crate) label: String,
    pub(crate) prefill: Option<String>,
}

type GatePromptEntries = Vec<(Instant, GatePromptRecord)>;
type GatePromptRegistry = HashMap<String, GatePromptEntries>;

static GATE_PROMPTS: LazyLock<Mutex<GatePromptRegistry>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record a sent gate prompt under its reference. A gate can be presented more
/// than once (request, escalation, or restart recovery), and every live prompt
/// must be finalized when that gate resolves. Sweeps expired entries so the
/// registry stays bounded by live-gate volume, not daemon uptime.
pub(crate) fn record(reference: &str, record: GatePromptRecord) {
    let mut map = GATE_PROMPTS.lock().expect("gate prompt registry poisoned");
    map.values_mut()
        .for_each(|records| records.retain(|(at, _)| at.elapsed() < SWEEP_AFTER));
    map.retain(|_, records| !records.is_empty());
    map.entry(reference.to_string())
        .or_default()
        .push((Instant::now(), record));
}

/// Remove all prompts for `reference` that were sent by `channel_alias`. The
/// caller PATCHes every returned message; on a transient failure it re-records
/// the failed and unattempted entries so a later terminal event can retry.
pub(crate) fn take_for_alias(reference: &str, channel_alias: &str) -> Vec<GatePromptRecord> {
    let mut map = GATE_PROMPTS.lock().expect("gate prompt registry poisoned");
    let Some(records) = map.remove(reference) else {
        return Vec::new();
    };
    let mut matching = Vec::new();
    let mut retained = Vec::new();
    for (at, record) in records {
        if at.elapsed() >= SWEEP_AFTER {
            continue;
        }
        if record.channel_alias == channel_alias {
            matching.push(record);
        } else {
            retained.push((at, record));
        }
    }
    if !retained.is_empty() {
        map.insert(reference.to_string(), retained);
    }
    matching
}

/// The input spec of `choice_id` on the prompt recorded under `reference`,
/// without consuming the record (a modal open must not stop a later finalize).
pub(crate) fn input_for(reference: &str, choice_id: &str) -> Option<GatePromptInput> {
    GATE_PROMPTS
        .lock()
        .expect("gate prompt registry poisoned")
        .get(reference)
        .and_then(|records| {
            records
                .iter()
                .rev()
                .find_map(|(_, record)| record.inputs.iter().find(|i| i.choice_id == choice_id))
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(title: &str) -> GatePromptRecord {
        GatePromptRecord {
            channel_alias: "primary".into(),
            channel_id: "c1".into(),
            message_id: "m1".into(),
            title: title.into(),
            resolved_description: Some("the approval context".into()),
            inputs: vec![GatePromptInput {
                choice_id: "edit".into(),
                label: "Edited body".into(),
                prefill: Some("draft".into()),
            }],
        }
    }

    #[test]
    fn record_is_visible_across_callers_and_take_consumes() {
        // Unique reference per test: the registry is process-wide by design.
        let reference = "run-registry-take";
        record(reference, rec("A"));
        // Any caller (a different channel instance) sees it…
        let mut got = take_for_alias(reference, "primary");
        let got = got.pop().expect("recorded entry is visible process-wide");
        assert_eq!(got.title, "A");
        assert_eq!(got.channel_alias, "primary");
        // …and take consumed it.
        assert!(take_for_alias(reference, "primary").is_empty());
    }

    #[test]
    fn reinsert_after_failed_finalize_allows_retry() {
        let reference = "run-registry-retry";
        record(reference, rec("A"));
        let got = take_for_alias(reference, "primary")
            .pop()
            .expect("first take");
        // Simulate a failed PATCH: put it back, a later event retries.
        record(reference, got);
        assert!(
            !take_for_alias(reference, "primary").is_empty(),
            "re-inserted entry is retryable"
        );
    }

    #[test]
    fn input_for_reads_without_consuming() {
        let reference = "run-registry-input";
        record(reference, rec("A"));
        let input = input_for(reference, "edit").expect("edit input recorded");
        assert_eq!(input.prefill.as_deref(), Some("draft"));
        assert!(input_for(reference, "revise").is_none(), "unknown choice");
        assert!(
            !take_for_alias(reference, "primary").is_empty(),
            "input_for must not consume the record"
        );
    }

    #[test]
    fn records_every_prompt_for_a_gate_and_partitions_by_sending_alias() {
        let reference = "run-registry-fanout";
        record(reference, rec("Initial request"));
        let mut escalation = rec("Escalation");
        escalation.channel_alias = "escalation".into();
        escalation.channel_id = "c2".into();
        escalation.message_id = "m2".into();
        escalation.inputs.clear();
        record(reference, escalation);

        let primary = take_for_alias(reference, "primary");
        assert_eq!(primary.len(), 1);
        assert_eq!(primary[0].title, "Initial request");
        let escalation = take_for_alias(reference, "escalation");
        assert_eq!(escalation.len(), 1);
        assert_eq!(escalation[0].title, "Escalation");
    }
}
