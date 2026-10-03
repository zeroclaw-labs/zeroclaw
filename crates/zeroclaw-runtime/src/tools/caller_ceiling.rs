//! The bounded-delegation tool ceiling, as it applies to the scheduler tools.
//!
//! A bounded delegate target runs with the caller's already-filtered registry as
//! its ceiling. That bound lives in the turn: it is applied when the target's
//! registry is assembled and it dies when the turn ends.
//!
//! The scheduler tools break that assumption, because the work they create does
//! not run in the turn that created it. When the job fires, the scheduler
//! rebuilds the owning agent's policy from config and passes the job's STORED
//! `allowed_tools` to `agent::run`; a job stored without one runs with the
//! owning agent's full registry. So for anything that schedules, the bound has
//! to be applied to persisted state, and there are three shapes:
//!
//! - [`cap_stored_allowed_tools`] for the writes that CREATE a job's tool set
//!   (`cron_add`'s agent branch, and a `cron_update` patch that names
//!   `allowed_tools`): intersect before storing, so the stored list carries the
//!   bound forward to the run.
//! - [`require_within_ceiling`] for the operations that LAUNCH or RE-POINT an
//!   existing AGENT job (`cron_run`, `schedule`'s resume, and any `cron_update`
//!   patch that can arm or re-point one): there is nothing left to intersect —
//!   the list was written earlier, possibly by the owning agent with no ceiling
//!   in force — so a job that is not already within the bound is refused. A
//!   patch that does not name `allowed_tools` still re-points execution:
//!   `prompt`, `schedule` and `enabled` are applied unconditionally, so the
//!   bound belongs on the RESULTING job, not on the patch.
//! - [`require_shell_within_ceiling`] for every route that creates, re-points,
//!   re-arms **or runs** a SHELL job: `cron_add`'s shell branch, `cron_update`
//!   against a shell job, `schedule`'s create / one-shot / resume, and
//!   **`cron_run`**, which is the verb that executes one. A shell job stores no
//!   `allowed_tools` to intersect: it stores a command that the scheduler runs
//!   under the owning agent's policy, never through a tool call, so the only
//!   bound available is whether the caller held `shell` itself.
//!
//! The tool NAMES are only one dimension of what the caller withholds. The
//! other is its COMMAND policy, and it needs no shape of its own here: a bounded
//! target's `SecurityPolicy` carries the command bounds of every caller above
//! it (`SecurityPolicy::caller_command_bounds`, attached in
//! `DelegateTool::policy_for_target`), and every scheduler tool REGISTERED
//! WITH THAT POLICY already judges the command it stores or re-arms with it.
//! That holds for a delegate-derived registration and for a `spawn_subagent`
//! child of one, which inherits the policy by clone. It does NOT hold for a
//! ceiling that reaches a turn built for another agent from tool names alone
//! (a `send_message_to_peer` relay, a live SOP step naming another agent): that
//! turn rebuilds its policy from config and carries no chain. The one gap
//! closed here for a registration that does carry it is a
//! job whose command is NOT being written (a `cron_update` of another field, a
//! `schedule` resume): [`require_job_within_ceiling`] re-judges the STORED
//! command for that case. A deferred AGENT job has no command yet, so the only
//! bound is not to defer the capability: a stored list that keeps `shell` is
//! refused ([`refuse_deferred_shell`]).
//!
//! Two things this enumeration is deliberately explicit about, because getting
//! either wrong is how the gaps this module exists to close were opened:
//!
//! - **`schedule` is in the third shape and not the first.** None of its routes
//!   writes an `allowed_tools` list, so there is nothing there to cap — which is
//!   a reason to bound it differently, never a reason to leave it unbounded.
//! - **`cron_run` is in BOTH shapes, chosen by `job_type`.** Bounding a shell
//!   job by its stored `allowed_tools` would bound it by a field it does not
//!   own. Such a job usually stores `None`, which [`require_within_ceiling`]
//!   refuses — the right outcome for the wrong reason, and only until an
//!   unbounded turn of the owning agent writes a harmless-looking list onto it
//!   (`crate::cron::store` applies an `allowed_tools` patch without consulting
//!   `job_type`).
//!
//! Both fail closed on an unsealed ceiling: a tool registered under bounded
//! delegation whose seal never completed has no bound to apply, and proceeding
//! would persist or launch exactly the escape the ceiling exists to prevent.

use std::sync::{Arc, OnceLock};

/// The caller's sealed tool set, shared with every tool the bounded assembly
/// rebuilds. `OnceLock` because the sealed set does not exist yet when these
/// tools are constructed: the bounded assembly fills the handle after it
/// finishes filtering, so every holder descends from one ceiling rather than
/// from a separately-derived copy.
pub(crate) type CallerCeiling = Arc<OnceLock<Vec<String>>>;

/// Read a ceiling handle, distinguishing "no bound in force" from "bound in
/// force but not sealed". The second case is an error everywhere.
fn sealed<'a>(
    tool: &str,
    ceiling: Option<&'a CallerCeiling>,
) -> Result<Option<&'a [String]>, String> {
    match ceiling {
        None => Ok(None),
        Some(handle) => match handle.get() {
            Some(list) => Ok(Some(list.as_slice())),
            None => Err(format!(
                "{tool}: refused — this tool was registered under bounded delegation \
                 but the caller's tool ceiling was never sealed, so the job's tool \
                 set cannot be bounded"
            )),
        },
    }
}

/// Intersect a job's requested `allowed_tools` with the caller ceiling, for the
/// tools that write a job's stored tool set.
///
/// - No ceiling: the request is stored unchanged.
/// - `requested = None` under a ceiling: an unset list means "inherit", and what
///   a bounded registration inherits is the caller's sealed set — not the owning
///   agent's registry. The ceiling itself is stored.
/// - Empty intersection: refused, never stored. An empty `allowed_tools` is read
///   back as unset, i.e. unrestricted, so storing one would silently reopen the
///   full registry — the opposite of the request.
pub(crate) fn cap_stored_allowed_tools(
    tool: &str,
    ceiling: Option<&CallerCeiling>,
    requested: Option<Vec<String>>,
) -> Result<Option<Vec<String>>, String> {
    let Some(ceiling) = sealed(tool, ceiling)? else {
        return Ok(requested);
    };
    let capped: Vec<String> = match requested {
        Some(list) => list
            .into_iter()
            .filter(|name| ceiling.iter().any(|allowed| allowed == name))
            .collect(),
        None => ceiling.to_vec(),
    };
    if capped.is_empty() {
        return Err(format!(
            "{tool}: refused — no requested tool survives the calling agent's bounded \
             tool ceiling, and an empty allowed_tools is stored as unrestricted"
        ));
    }
    refuse_deferred_shell(tool, &capped)?;
    Ok(Some(capped))
}

/// Refuse a deferred AGENT job whose tool list keeps `shell`.
///
/// A job that starts an agent later runs it under the OWNING agent's policy,
/// rebuilt from config when the job fires — the bounded caller's command
/// policy is not part of what the scheduler has, and a job row has nowhere to
/// carry it. So a stored list that keeps `shell` hands the later run a shell
/// judged by the target's command policy alone. Shell commands stored as
/// shell jobs are covered differently (the command is judged against the
/// caller's command policy when it is written); an agent job's commands do
/// not exist yet, so the only bound available is not to defer the capability.
///
/// Refusing rather than stripping: silently dropping `shell` would store a
/// job that behaves differently from the one requested. The caller can list
/// the tools it wants, without `shell`.
fn refuse_deferred_shell(tool: &str, stored: &[String]) -> Result<(), String> {
    if stored.iter().any(|name| name == SHELL_TOOL_NAME) {
        return Err(format!(
            "{tool}: refused — this agent job would keep '{SHELL_TOOL_NAME}', and a deferred \
             agent later runs it under the owning agent's command policy alone, without the \
             calling agent's; list the tools the job needs without '{SHELL_TOOL_NAME}'"
        ));
    }
    Ok(())
}

/// Refuse to launch a job whose stored tool set is not already within the
/// caller ceiling, for the tools that run an existing job.
///
/// `stored = None` is a refusal, not a pass: an unset list means the job runs
/// with the owning agent's full registry, which is by definition outside any
/// ceiling. Launching it from a bounded turn would execute tools the caller was
/// never granted, which is the same escape as storing an unbounded job.
pub(crate) fn require_within_ceiling(
    tool: &str,
    ceiling: Option<&CallerCeiling>,
    stored: Option<&[String]>,
) -> Result<(), String> {
    let Some(ceiling) = sealed(tool, ceiling)? else {
        return Ok(());
    };
    let Some(stored) = stored else {
        return Err(format!(
            "{tool}: refused — this job stores no allowed_tools, so it would run with \
             the owning agent's full registry, outside the calling agent's bounded \
             tool ceiling"
        ));
    };
    let outside: Vec<&str> = stored
        .iter()
        .filter(|name| !ceiling.iter().any(|allowed| allowed == *name))
        .map(String::as_str)
        .collect();
    if !outside.is_empty() {
        return Err(format!(
            "{tool}: refused — this job's stored allowed_tools reach beyond the calling \
             agent's bounded tool ceiling ({})",
            outside.join(", ")
        ));
    }
    // Within the ceiling is not enough for `shell`: the launch or re-arm would
    // start an agent whose shell carries the owning agent's command policy
    // alone. See `refuse_deferred_shell`.
    refuse_deferred_shell(tool, stored)?;
    Ok(())
}

/// The registry name of the tool that runs a shell command inside a turn.
pub(crate) const SHELL_TOOL_NAME: &str = "shell";

/// Refuse a bounded operation that would create, re-point or re-arm a stored
/// SHELL job when the caller could not have run a shell command itself.
///
/// A shell job has no `allowed_tools` column: it stores a command string, and
/// [`crate::cron::scheduler`] runs it under the OWNING agent's policy without
/// consulting any tool list. So neither [`cap_stored_allowed_tools`] nor
/// [`require_within_ceiling`] has anything to work with, and the bound that
/// does apply is the caller's own shell capability: if `shell` was not in the
/// caller's sealed set, the caller could not have run the command during the
/// bounded turn, and deferring it through the scheduler must not become the way
/// to. This is the fail-closed half of the contract — the alternative, storing
/// a per-job ceiling the scheduler enforces at replay, would bound the command
/// itself and is deliberately NOT what this does.
pub(crate) fn require_shell_within_ceiling(
    tool: &str,
    ceiling: Option<&CallerCeiling>,
) -> Result<(), String> {
    let Some(ceiling) = sealed(tool, ceiling)? else {
        return Ok(());
    };
    if ceiling.iter().any(|allowed| allowed == SHELL_TOOL_NAME) {
        return Ok(());
    }
    Err(format!(
        "{tool}: refused — this stores or re-arms a shell job, whose command the \
         scheduler later runs under the owning agent's policy, and 'shell' is \
         outside the calling agent's bounded tool ceiling"
    ))
}

/// The bound a stored job must satisfy for a bounded caller to write or run it:
/// the shell capability for a shell job, the stored `allowed_tools` for an agent
/// job. One predicate for every tool that judges a job, so the arms cannot drift
/// apart, and usable as the guard `cron::update_job_for_agent` evaluates inside
/// its write transaction.
///
/// A shell job is also judged by its STORED command. Patching a job's name or
/// schedule, or re-arming it, leaves whatever command it already holds in
/// force, and that command was last judged when it was written — possibly by
/// the owning agent in an unbounded turn, under a policy that carries none of
/// this caller's restrictions. `security` is the bounded tool's own policy, so
/// `validate_command_execution_for_shell` applies the caller chain it carries.
/// The same signal as the ceiling checks above decides whether this runs: with
/// no ceiling in force (an unbounded registration) nothing is re-judged and that
/// path is unchanged.
///
/// `approved` is whatever the calling tool took from its own request, and it
/// only ever relaxes the approval gates a Supervised policy applies to
/// medium- and high-risk commands; it never widens an allowlist.
pub(crate) fn require_job_within_ceiling(
    tool: &str,
    ceiling: Option<&CallerCeiling>,
    security: &crate::security::SecurityPolicy,
    runtime: &dyn crate::platform::RuntimeAdapter,
    approved: bool,
    job: &crate::cron::CronJob,
) -> Result<(), String> {
    match job.job_type {
        crate::cron::JobType::Shell => {
            require_shell_within_ceiling(tool, ceiling)?;
            if ceiling.is_some() {
                crate::cron::validate_shell_command_with_security(
                    runtime,
                    security,
                    &job.command,
                    approved,
                )
                .map_err(|reason| format!("{tool}: refused — {reason}"))?;
            }
            Ok(())
        }
        crate::cron::JobType::Agent => {
            require_within_ceiling(tool, ceiling, job.allowed_tools.as_deref())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealed_ceiling(names: &[&str]) -> CallerCeiling {
        let handle: CallerCeiling = Arc::new(OnceLock::new());
        let _ = handle.set(names.iter().map(|n| (*n).to_string()).collect());
        handle
    }

    fn unsealed_ceiling() -> CallerCeiling {
        Arc::new(OnceLock::new())
    }

    fn names(list: &Option<Vec<String>>) -> Vec<&str> {
        list.as_ref()
            .map(|l| l.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    // ── cap_stored_allowed_tools ────────────────────────────────────────────

    #[test]
    fn no_ceiling_stores_the_request_unchanged() {
        // Broken state this pins: capping unconditionally would narrow every
        // ordinary (unbounded) cron_add to nothing.
        let requested = Some(vec!["shell".to_string(), "file_write".to_string()]);
        let stored = cap_stored_allowed_tools("cron_add", None, requested).expect("no ceiling");
        assert_eq!(names(&stored), vec!["shell", "file_write"]);

        let stored = cap_stored_allowed_tools("cron_add", None, None).expect("no ceiling");
        assert!(stored.is_none(), "an unbounded unset list stays unset");
    }

    #[test]
    fn unsealed_ceiling_refuses_instead_of_storing() {
        // Broken state: treating an unfilled OnceLock as "no ceiling" would make
        // every bounded registration unbounded whenever the seal is skipped.
        let handle = unsealed_ceiling();
        let error = cap_stored_allowed_tools("cron_add", Some(&handle), None)
            .expect_err("an unsealed ceiling must refuse");
        assert!(error.contains("never sealed"), "got: {error}");
    }

    #[test]
    fn unset_request_under_a_ceiling_stores_the_ceiling() {
        // "Unset means inherit" — and what a bounded registration inherits is
        // the caller's sealed set, NOT the owning agent's full registry. Broken
        // state: passing `None` through, which the scheduler reads as
        // unrestricted.
        // (`file_read`, not `shell`: a stored list that keeps `shell` is refused
        // by `refuse_deferred_shell`, tested below.)
        let handle = sealed_ceiling(&["file_read", "cron_add"]);
        let stored = cap_stored_allowed_tools("cron_add", Some(&handle), None).expect("capped");
        assert_eq!(names(&stored), vec!["file_read", "cron_add"]);
    }

    #[test]
    fn request_is_intersected_keeping_the_admitted_half() {
        // Both halves in one assertion: `file_write` is dropped (the negative)
        // and `shell` survives (the positive). Without the positive half this
        // test would also pass against a cap that stores nothing at all.
        let handle = sealed_ceiling(&["file_read", "cron_add"]);
        let requested = Some(vec!["file_read".to_string(), "file_write".to_string()]);
        let stored =
            cap_stored_allowed_tools("cron_add", Some(&handle), requested).expect("capped");
        assert_eq!(
            names(&stored),
            vec!["file_read"],
            "the out-of-ceiling tool is dropped and the admitted one survives"
        );
    }

    // ── deferred agent jobs never keep `shell` ──────────────────────────────

    #[test]
    fn an_agent_job_naming_shell_is_refused_at_write() {
        // The caller holds `shell`, so the intersection keeps it — and a later
        // run of the stored agent would carry the target's command policy only.
        let handle = sealed_ceiling(&["shell", "cron_add"]);
        let requested = Some(vec!["shell".to_string(), "cron_add".to_string()]);
        let error = cap_stored_allowed_tools("cron_add", Some(&handle), requested)
            .expect_err("a stored list that keeps `shell` must be refused");
        assert!(error.contains("'shell'"), "{error}");
    }

    #[test]
    fn an_agent_job_inheriting_a_ceiling_with_shell_is_refused_at_write() {
        // "Unset means inherit the caller's ceiling", which contains `shell`.
        let handle = sealed_ceiling(&["shell", "cron_add"]);
        cap_stored_allowed_tools("cron_add", Some(&handle), None)
            .expect_err("inheriting a ceiling that holds `shell` keeps it");
    }

    #[test]
    fn an_agent_job_without_shell_is_still_stored() {
        // Control: the refusal is about `shell`, not about agent jobs under a
        // ceiling that happens to contain it.
        let handle = sealed_ceiling(&["shell", "cron_add"]);
        let stored = cap_stored_allowed_tools(
            "cron_add",
            Some(&handle),
            Some(vec!["cron_add".to_string()]),
        )
        .expect("a list without `shell` is unaffected");
        assert_eq!(names(&stored), vec!["cron_add"]);
    }

    #[test]
    fn launching_a_stored_agent_job_that_keeps_shell_is_refused() {
        // `shell` is within the ceiling, so `require_within_ceiling` alone
        // would pass it; the launch would still start an agent whose shell has
        // no caller restriction.
        let handle = sealed_ceiling(&["shell", "cron_run"]);
        let error = require_within_ceiling("cron_run", Some(&handle), Some(&["shell".to_string()]))
            .expect_err("an agent job keeping `shell` must not launch from a bounded turn");
        assert!(error.contains("'shell'"), "{error}");
    }

    #[test]
    fn a_stored_shell_entry_is_not_refused_when_no_ceiling_is_in_force() {
        // The unbounded path is untouched: no ceiling, no bound.
        require_within_ceiling("cron_run", None, Some(&["shell".to_string()]))
            .expect("no ceiling in force");
        let stored = cap_stored_allowed_tools("cron_add", None, Some(vec!["shell".to_string()]))
            .expect("no ceiling in force");
        assert_eq!(names(&stored), vec!["shell"]);
    }

    #[test]
    fn fully_out_of_ceiling_request_refuses_and_never_stores_empty() {
        // The trap this exists for: an empty `allowed_tools` is read back as
        // unset, i.e. UNRESTRICTED. Storing the empty intersection would hand
        // the job the owning agent's whole registry — the exact escape being
        // closed. Broken state: `Ok(Some(vec![]))` or `Ok(None)` here.
        let handle = sealed_ceiling(&["shell"]);
        let requested = Some(vec!["file_write".to_string()]);
        let result = cap_stored_allowed_tools("cron_add", Some(&handle), requested);
        let error = result.expect_err("an empty intersection must refuse, not store");
        assert!(error.contains("stored as unrestricted"), "got: {error}");
    }

    // ── require_within_ceiling ──────────────────────────────────────────────

    #[test]
    fn launching_without_a_ceiling_is_unaffected() {
        // Broken state: refusing unconditionally would break ordinary cron_run.
        require_within_ceiling("cron_run", None, Some(&["file_write".to_string()]))
            .expect("no ceiling in force");
        require_within_ceiling("cron_run", None, None).expect("no ceiling in force");
    }

    #[test]
    fn launching_a_job_within_the_ceiling_is_allowed() {
        // The positive half of the two refusals below: without it, a
        // `require_within_ceiling` that refused everything would look correct.
        let handle = sealed_ceiling(&["file_read", "cron_add"]);
        require_within_ceiling("cron_run", Some(&handle), Some(&["file_read".to_string()]))
            .expect("a job inside the ceiling still runs");
    }

    #[test]
    fn launching_an_unrestricted_job_is_refused() {
        // `None` is not "nothing to check": it means the job runs with the
        // owning agent's full registry. Broken state: treating it as a pass.
        let handle = sealed_ceiling(&["shell"]);
        let error = require_within_ceiling("cron_run", Some(&handle), None)
            .expect_err("an unrestricted job must not launch from a bounded turn");
        assert!(error.contains("stores no allowed_tools"), "got: {error}");
    }

    #[test]
    fn launching_a_job_reaching_beyond_the_ceiling_names_the_offender() {
        let handle = sealed_ceiling(&["shell"]);
        let stored = ["shell".to_string(), "file_write".to_string()];
        let error = require_within_ceiling("cron_run", Some(&handle), Some(&stored))
            .expect_err("a job reaching beyond the ceiling must not launch");
        assert!(
            error.contains("file_write") && !error.contains("shell,"),
            "the message must name the offending tool, not the admitted one: {error}"
        );
    }

    #[test]
    fn launching_under_an_unsealed_ceiling_is_refused() {
        let handle = unsealed_ceiling();
        let error = require_within_ceiling("cron_run", Some(&handle), Some(&["shell".to_string()]))
            .expect_err("an unsealed ceiling must refuse at launch too");
        assert!(error.contains("never sealed"), "got: {error}");
    }

    // ── require_shell_within_ceiling ────────────────────────────────────────

    #[test]
    fn storing_a_shell_job_without_a_ceiling_is_unaffected() {
        // Broken state: refusing unconditionally would break every ordinary
        // `schedule`/`cron_add` shell job outside bounded delegation.
        require_shell_within_ceiling("schedule", None).expect("no ceiling in force");
    }

    #[test]
    fn storing_a_shell_job_is_allowed_when_the_caller_held_shell() {
        // The positive half of the refusals below. A bounded caller that could
        // have run the command in its own turn loses nothing by deferring it,
        // so this must keep working — otherwise the refusal below would look
        // correct while simply banning the tool.
        let handle = sealed_ceiling(&["shell", "cron_add"]);
        require_shell_within_ceiling("cron_add", Some(&handle))
            .expect("a caller holding `shell` may still defer a shell command");
    }

    #[test]
    fn storing_a_shell_job_is_refused_when_shell_is_outside_the_ceiling() {
        // The escape: a shell job carries no `allowed_tools`, so nothing
        // downstream can bound it — the scheduler runs the stored command under
        // the OWNING agent's policy. Broken state: letting the write through
        // because there is no list to intersect.
        let handle = sealed_ceiling(&["cron_add", "spawn_subagent"]);
        let error = require_shell_within_ceiling("cron_add", Some(&handle))
            .expect_err("a caller without `shell` must not defer a shell command");
        assert!(
            error.contains("shell") && error.contains("owning agent's policy"),
            "the message must say what the refusal protects, not just that it refused: {error}"
        );
    }

    #[test]
    fn storing_a_shell_job_under_an_unsealed_ceiling_is_refused() {
        // Same fail-closed reading as the other two shapes: a bound in force but
        // not yet sealed is an error, never an absent bound.
        let handle = unsealed_ceiling();
        let error = require_shell_within_ceiling("schedule", Some(&handle))
            .expect_err("an unsealed ceiling must refuse the shell route too");
        assert!(error.contains("never sealed"), "got: {error}");
    }

    #[test]
    fn a_ceiling_that_merely_mentions_shell_inside_another_name_does_not_admit_it() {
        // `shell` is matched as a whole name, not as a substring: a ceiling
        // holding `shell_history` or `powershell` grants no shell execution.
        // Broken state: a `contains`-style match, which would silently admit it.
        let handle = sealed_ceiling(&["shell_history", "powershell"]);
        require_shell_within_ceiling("schedule", Some(&handle))
            .expect_err("a name containing `shell` is not the `shell` tool");
    }
}
