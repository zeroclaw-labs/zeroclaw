//! Per-turn evidence from completed dispatches. Only hashes survive a round.

use crate::agent::tool_execution::{CompletedToolExecution, ToolExecutionOutcome};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use zeroclaw_tool_call_parser::ParsedToolCall;

#[derive(Default)]
pub(crate) struct RepetitionGuard {
    last: Option<(Signature, bool)>,
    repeats: usize,
    warned_failure: Option<Signature>,
    success_advised: bool,
    exhausted: bool,
}

#[derive(Default)]
pub(super) struct RepetitionDisposition {
    pub(super) message_key: Option<&'static str>,
    pub(super) close: bool,
}

impl RepetitionGuard {
    pub(crate) fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    pub(super) fn recovery_call_hash(&self) -> Option<u64> {
        self.warned_failure.map(|warned| warned.call)
    }

    pub(super) fn observe_round(
        &mut self,
        calls: &[ParsedToolCall],
        outcomes: &[CompletedToolExecution],
        ignored: &HashSet<&str>,
        threshold: usize,
    ) -> RepetitionDisposition {
        let evidence: Vec<_> = calls
            .iter()
            .zip(outcomes)
            .filter_map(|(call, completed)| {
                let name = completed.executed_tool_name.as_deref()?;
                if ignored.contains(name) {
                    return None;
                }
                Some((
                    signature(name, &call.arguments, &completed.outcome),
                    completed.outcome.success,
                ))
            })
            .collect();

        // A warning raised in this batch cannot spend the recovery attempt.
        // Inspect the whole later batch before closing, so another completed
        // call that changed the approach or succeeded can recover the turn.
        if let Some(warned) = self.warned_failure {
            if evidence
                .iter()
                .any(|&(hash, success)| success || hash != warned)
            {
                self.warned_failure = None;
            } else if evidence.iter().any(|&(hash, _)| hash == warned) {
                self.exhausted = true;
                return RepetitionDisposition {
                    message_key: Some("turn-repeated-failure-exhausted"),
                    close: true,
                };
            }
        }

        let mut disposition = RepetitionDisposition::default();
        for (hash, success) in evidence {
            if self.last == Some((hash, success)) {
                self.repeats = self.repeats.saturating_add(1);
            } else {
                self.last = Some((hash, success));
                self.repeats = 1;
                self.warned_failure = None;
            }
            if self.repeats < threshold.max(1) {
                continue;
            }
            if success {
                if !self.success_advised {
                    self.success_advised = true;
                    disposition.message_key = Some("turn-repeated-success-advisory");
                }
            } else if self.warned_failure != Some(hash) {
                self.warned_failure = Some(hash);
                disposition.message_key = Some("turn-repeated-failure-recovery");
            }
        }
        // A changed call later in the warning batch invalidates that warning.
        if disposition.message_key == Some("turn-repeated-failure-recovery")
            && self.warned_failure.is_none()
        {
            disposition.message_key = None;
        }
        disposition
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Signature {
    call: u64,
    result: u64,
}

pub(super) fn call_signature(name: &str, arguments: &serde_json::Value) -> u64 {
    let mut args = arguments.clone();
    if crate::agent::is_runtime_approved_arg_tool(name)
        && let Some(args) = args.as_object_mut()
    {
        args.remove("approved");
    }
    let args = zeroclaw_tool_call_parser::canonicalize_json_for_tool_signature(&args);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    args.to_string().hash(&mut hasher);
    hasher.finish()
}

fn signature(
    name: &str,
    arguments: &serde_json::Value,
    outcome: &ToolExecutionOutcome,
) -> Signature {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    outcome.success.hash(&mut hasher);
    // Failed output is already credential-scrubbed by the executor. Hash the
    // outcome seen by the model instead of retaining raw remote error bodies.
    outcome.output.hash(&mut hasher);
    Signature {
        call: call_signature(name, arguments),
        result: hasher.finish(),
    }
}
