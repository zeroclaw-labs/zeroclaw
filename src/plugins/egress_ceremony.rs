//! The plugin egress grant ceremony: the pure half.
//!
//! The manifest **declares**; the operator's config **grants**. Installation is
//! the one moment where those two can be reconciled without an operator typing
//! anything, so `zeroclaw plugin install` seeds a *newly created*
//! `[[plugins.entries]]` row from the declaration and prints what it granted.
//!
//! Everything after that is a diff, never a write: a package upgrade whose
//! declaration grew does **not** extend an entry that already exists. The CLI
//! prints the difference with the exact command, and the operator applies it
//! deliberately. That is the security property of the ceremony — a package
//! update must not be able to widen its own network reach — so the "existing
//! entry" branch in [`crate::plugins::egress_ceremony`]'s callers is
//! deliberately write-free.
//!
//! This module owns only the comparison and command construction; every
//! user-facing string stays in the CLI so it routes through Fluent.

use zeroclaw_infra::net_guard::{egress_pattern_contains, normalize_egress_pattern};

/// The config path holding an instance's granted allowlist.
///
/// `instance_key` is the opaque `zpi1_` key from
/// `PluginInstanceScope::config_entry_key()` — the same key the instance's
/// private `config` map resolves against. One `[[plugins.entries]]` row per
/// instance carries both, so the grant and the config an operator edits are
/// never split across two rows.
#[must_use]
pub fn egress_hosts_path(instance_key: &str) -> String {
    format!("plugins.entries.{instance_key}.egress_hosts")
}

/// The exact `zeroclaw config set` invocation that makes `hosts` the instance
/// row's granted allowlist.
///
/// `config set` on a string array **replaces** the list rather than appending
/// to it, so a command that is meant to *add* a destination has to carry the
/// full resulting list. Callers building an "apply this addition" command must
/// therefore pass the union (see [`EgressDeclarationDiff::union`]), not just the
/// additions. The value is double-quoted because suffix patterns start with `*`
/// and a bare `*.example.com` would be glob-expanded by the operator's shell.
#[must_use]
pub fn egress_set_command(instance_key: &str, hosts: &[String]) -> String {
    format!(
        "zeroclaw config set {} \"{}\"",
        egress_hosts_path(instance_key),
        hosts.join(",")
    )
}

/// The legacy `[[plugins.entries]]` row an instance's grant is stranded on, if
/// any.
///
/// [`egress_set_command`] addresses the canonical `zpi1_` row, and dotted
/// `plugins.entries.<key>.…` paths resolve through natural-key lookup, which
/// only matches rows **already present in live config**. So on a pre-typed-config
/// install, where the row is still keyed by the package name, that command
/// targets a row that does not exist and fails with `Unknown property` instead
/// of writing the grant. The row has to be renamed first.
///
/// Returns `Some(row_name)` only when the canonical row is absent *and* one of
/// `legacy_candidates` is present, which is exactly the state that needs the
/// rename step printed before the grant command.
///
/// `legacy_candidates` is the set of names a pre-typed-config row could carry
/// for this instance: the package name, and the binding when a future
/// alias-aware key path makes the two differ. Every key derived today comes
/// from the default tool binding, whose binding string *is* the package name,
/// so callers pass one candidate and get the same answer.
///
/// `None` covers both "the canonical row is present" (the command resolves) and
/// "no row exists at all" (the command fails, but renaming nothing would not
/// help). Only the first is a state the printed grant command can act on.
#[must_use]
pub fn stranded_legacy_grant_row(
    instance_key: &str,
    legacy_candidates: &[String],
    row_names: &[String],
) -> Option<String> {
    if row_names.iter().any(|name| name == instance_key) {
        return None;
    }
    legacy_candidates
        .iter()
        .find(|candidate| row_names.iter().any(|name| name == *candidate))
        .cloned()
}

/// Canonicalize a declared or granted list for comparison and for seeding.
///
/// Uses the same grammar the manifest and the config are validated against, so
/// "declared" and "granted" are compared in one vocabulary and a seeded entry
/// is written in exactly the form `Config::validate` accepts. Sorted and
/// deduplicated, mirroring `net_guard::normalize_egress_patterns`, so output is
/// deterministic regardless of authoring order.
///
/// An entry that fails the grammar is kept verbatim (trimmed) rather than
/// dropped: this runs against config an operator may have hand-edited, and
/// silently hiding an invalid grant would misreport what is on disk. Invalid
/// entries are rejected at config load, not here.
#[must_use]
pub fn canonical_hosts(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = raw
        .iter()
        .map(|h| normalize_egress_pattern(h).unwrap_or_else(|_| h.trim().to_string()))
        .filter(|h| !h.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Declaration-versus-grant comparison for one plugin instance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EgressDeclarationDiff {
    /// Canonical declared destinations (what the manifest asks for).
    pub declared: Vec<String>,
    /// Canonical granted destinations (what the entry actually permits).
    pub granted: Vec<String>,
    /// Declared destinations **no grant covers** — denials waiting to happen.
    /// Wildcard-containment aware: a declared host a granted `*.suffix` reaches
    /// is not listed here, because the runtime would already permit it.
    pub declared_not_granted: Vec<String>,
    /// Granted destinations **the declaration does not cover** — left in place;
    /// informational only. A grant already covered by the declaration is within
    /// it, not beyond it, so it is not listed here.
    pub granted_not_declared: Vec<String>,
}

impl EgressDeclarationDiff {
    /// Nothing to tell the operator about.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.declared_not_granted.is_empty() && self.granted_not_declared.is_empty()
    }

    /// The allowlist that grants every declared destination **without revoking
    /// anything already granted**. This is the value the printed apply-command
    /// carries: `config set` replaces the list, and an upgrade prompt that
    /// silently dropped an operator-authored host (a self-hosted Gitea, a LAN
    /// Nextcloud) would be worse than the gap it is closing.
    #[must_use]
    pub fn union(&self) -> Vec<String> {
        let mut out = self.granted.clone();
        out.extend(self.declared.iter().cloned());
        out.sort();
        out.dedup();
        out
    }
}

/// Compare a manifest declaration against an entry's granted allowlist.
///
/// Comparison is by **reachability**, not set membership, so the diagnostic
/// agrees with what the runtime actually enforces. The runtime resolves a
/// destination through wildcard containment
/// ([`net_guard::egress_pattern_contains`][c]) — a granted `*.example.com`
/// reaches `api.example.com` — so a declared host a grant already covers is not
/// a gap. Plain set membership would report it as "declared but not granted"
/// and tell the operator to grant a destination the runtime already permits,
/// which is exactly the false positive this comparison must not produce.
///
/// Both sides use the same predicate, in the covering direction each needs:
/// - `declared_not_granted`: a declared destination **no grant covers** — the
///   actionable gap, a denial waiting to happen. This mirrors runtime
///   reachability exactly.
/// - `granted_not_declared`: a granted destination **the declaration does not
///   cover** — informational only. A grant the declaration already covers is
///   *within* the declaration (an operator who narrowed a declared
///   `*.example.com` to one subdomain has not granted "beyond" it), so only
///   genuinely broader or unrelated grants — a wider `*.example.com`, or the
///   operator's own self-hosted destination — are surfaced.
///
/// Canonicalization still collapses order and duplication first, and the
/// grammar keeps `*.example.com` and its apex `example.com` distinct: a suffix
/// grant never covers its apex, so a declared apex stays a gap.
///
/// [c]: zeroclaw_infra::net_guard::egress_pattern_contains
#[must_use]
pub fn diff_declaration(declared: &[String], granted: &[String]) -> EgressDeclarationDiff {
    let declared = canonical_hosts(declared);
    let granted = canonical_hosts(granted);
    let declared_not_granted: Vec<String> = declared
        .iter()
        .filter(|d| !granted.iter().any(|g| egress_pattern_contains(g, d)))
        .cloned()
        .collect();
    let granted_not_declared: Vec<String> = granted
        .iter()
        .filter(|g| !declared.iter().any(|d| egress_pattern_contains(d, g)))
        .cloned()
        .collect();
    EgressDeclarationDiff {
        declared,
        granted,
        declared_not_granted,
        granted_not_declared,
    }
}

/// Should the upgrade diff be reported at all?
///
/// A manifest that declares nothing produces no diff, even when the entry
/// grants destinations. Those grants are the second, first-class grant
/// path — operator-authored, for plugins whose destination *is* instance
/// configuration (a self-hosted Gitea, a LAN Nextcloud) that no author could
/// have declared. Reporting them as "no longer declared" on every reinstall
/// would train operators to ignore the ceremony.
#[must_use]
pub fn should_report_diff(diff: &EgressDeclarationDiff) -> bool {
    !diff.declared.is_empty() && !diff.is_empty()
}

/// Split a grant list into the entries the runtime will accept and the ones it
/// will reject, keeping the rejected ones verbatim so they can be named.
///
/// Every entry is judged by `normalize_egress_pattern` exactly as the runtime
/// judges it — on the raw bytes, with no trimming and no skipping — so an
/// entry with boundary whitespace or an empty entry is rejected here because
/// it is rejected there. [`canonical_hosts`] deliberately preserves invalid
/// entries so a diff never hides what is on disk; this split exists because an
/// invalid entry must not take part in coverage (`egress_pattern_contains`
/// trusts its inputs, so a rejected `*.com` would "cover" `api.com`) and must
/// not be carried into a printed `config set`.
///
/// This decides which *entries* count; whether the runtime accepts the *row*
/// is [`runtime_rejection`]'s call, and only that.
#[must_use]
pub fn partition_valid_hosts(raw: &[String]) -> (Vec<String>, Vec<String>) {
    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    for entry in raw {
        match normalize_egress_pattern(entry) {
            Ok(canonical) => valid.push(canonical),
            Err(_) => invalid.push(entry.clone()),
        }
    }
    valid.sort();
    valid.dedup();
    invalid.sort();
    invalid.dedup();
    (valid, invalid)
}

/// What the runtime needs, besides the row itself, to decide whether it will
/// accept a grant: the deployment's `security.nat64_prefixes` and the
/// per-instance connection ceiling. Both live in the same config as the row;
/// the diagnostic passes them through untouched so its verdict is the
/// runtime's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRuntimeInputs {
    pub nat64_prefixes: Vec<String>,
    pub max_connections_per_instance: usize,
}

/// Which part of the deployment a refusal points at, so the report names a
/// path the operator can actually change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionScope {
    /// The row itself: a host pattern the grammar rejects, or a private
    /// carve-out no granted host covers. Editing the row's `egress_hosts` or
    /// `egress_allow_private` is the fix.
    Row,
    /// The deployment: `security.nat64_prefixes` or
    /// `plugins.limits.max_connections_per_instance`. Every instance is
    /// refused alike, and no row edit changes it, so it is reported once by
    /// the caller and never as a row repair.
    Deployment,
}

/// The runtime's refusal of a policy, with the scope it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRejection {
    pub scope: RejectionScope,
    pub reason: String,
}

/// Ask the runtime whether it would refuse this row, and if so, why.
///
/// This is the one source of truth for acceptance. The diagnostic does not
/// re-implement the grammar, the private-carve-out containment rule, or any
/// other rule: it builds the policy exactly as the runtime's
/// `plugin_egress_policy` does at request time and returns the constructor's
/// own error, classified by what it points at. Anything this accepts, the
/// runtime enforces; anything it rejects, the runtime refuses whole, and the
/// instance is denied everything.
///
/// The constructor checks the connection ceiling first, then the hosts, then
/// the carve-outs, then the NAT64 prefixes. So a bad ceiling masks row
/// problems until it is fixed, while bad NAT64 prefixes never hide a row
/// problem. The report follows that precedence rather than guessing.
#[must_use]
pub fn runtime_rejection(
    hosts: &[String],
    allow_private: &[String],
    runtime: &EgressRuntimeInputs,
) -> Option<RuntimeRejection> {
    use zeroclaw_plugins::egress::{EgressError, EgressPolicy};
    match EgressPolicy::new(
        hosts,
        allow_private,
        &runtime.nat64_prefixes,
        runtime.max_connections_per_instance,
    ) {
        Ok(_) => None,
        Err(error) => {
            let scope = match &error {
                EgressError::InvalidNat64Prefix(_) | EgressError::InvalidConnectionLimit => {
                    RejectionScope::Deployment
                }
                _ => RejectionScope::Row,
            };
            Some(RuntimeRejection {
                scope,
                reason: error.to_string(),
            })
        }
    }
}

/// The row-scoped half of [`runtime_rejection`]: the reason the runtime
/// refuses this row for what is *in* the row, or `None` when the row is fine
/// or the refusal is deployment-wide (which the caller reports separately).
#[must_use]
pub fn row_rejection(
    hosts: &[String],
    allow_private: &[String],
    runtime: &EgressRuntimeInputs,
) -> Option<String> {
    runtime_rejection(hosts, allow_private, runtime)
        .filter(|rejection| rejection.scope == RejectionScope::Row)
        .map(|rejection| rejection.reason)
}

/// The deployment-wide verdict on its own: would the runtime refuse even an
/// empty row? An empty grant is always accepted (it means no reach), so any
/// refusal here comes from the deployment inputs alone.
#[must_use]
pub fn deployment_rejection(runtime: &EgressRuntimeInputs) -> Option<String> {
    runtime_rejection(&[], &[], runtime)
        .filter(|rejection| rejection.scope == RejectionScope::Deployment)
        .map(|rejection| rejection.reason)
}

/// Where one instance's egress grant lives, and whether the runtime honors it.
///
/// This is the distinction the gap diagnostic has to keep straight. The runtime
/// resolves an instance's allowlist by its canonical `zpi1_` key and nothing
/// else, so a grant an operator authored on a pre-typed-config row (keyed by
/// the package name) is **not in effect** — the plugin has no network reach —
/// even though it is exactly the list the operator wants carried forward. Two
/// questions, two answers:
///
/// - *What does the runtime enforce?* decides whether there is a gap and
///   whether a migration is needed. For a stranded row the answer is "nothing".
/// - *What has the operator authored?* decides what a printed command must
///   carry, because `config set` replaces the list and must not revoke a host
///   the operator wrote themselves.
///
/// Collapsing the two into one list is how both prior defects happened: read
/// the enforced (empty) grant for both and the printed command drops the
/// operator's hosts; read the authored grant for both and a row that already
/// covers the declaration is reported as healthy while every request is still
/// denied. The variants make the split explicit so a caller cannot conflate
/// them by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressGrantState {
    /// The grant the runtime reads: the canonical row's allowlist and private
    /// carve-outs, or empty when no row exists. Enforcement and authorship
    /// agree here.
    Enforced {
        granted: Vec<String>,
        allow_private: Vec<String>,
    },
    /// The canonical row is absent and the operator's grant sits on a legacy
    /// package-name row the runtime does not read. Nothing is enforced until
    /// the row is renamed; `authored` and `allow_private` are what the rename
    /// brings into effect and what any grant command must carry forward.
    Stranded {
        legacy_row: String,
        authored: Vec<String>,
        allow_private: Vec<String>,
    },
}

/// Resolve where an instance's grant lives from the rows present in config.
///
/// `granted_on` reads a row's `(egress_hosts, egress_allow_private)` by name
/// (the caller's `PluginsConfig::entry_egress`); passing it in keeps this module
/// free of the config types and lets the decision be tested against a plain
/// lookup.
#[must_use]
pub fn resolve_grant_state(
    instance_key: &str,
    legacy_candidates: &[String],
    row_names: &[String],
    granted_on: impl Fn(&str) -> (Vec<String>, Vec<String>),
) -> EgressGrantState {
    match stranded_legacy_grant_row(instance_key, legacy_candidates, row_names) {
        Some(legacy_row) => {
            let (authored, allow_private) = granted_on(&legacy_row);
            EgressGrantState::Stranded {
                legacy_row,
                authored,
                allow_private,
            }
        }
        None => {
            let (granted, allow_private) = granted_on(instance_key);
            EgressGrantState::Enforced {
                granted,
                allow_private,
            }
        }
    }
}

/// What `plugin list` has to tell the operator about one instance.
///
/// Three facts travel with a report, each from a different judge:
/// - `missing`: declared destinations no accepted grant covers (containment,
///   over the entries the runtime accepts);
/// - `invalid`: the individual granted entries the grammar rejects, named so
///   the operator can find them;
/// - `rejected`: the runtime's own reason for refusing the row as it stands
///   (for a stranded row: as the rename would bring it into effect). This is
///   [`runtime_rejection`]'s verdict and covers everything the grammar and
///   the private-carve-out rule reject, not only the entries in `invalid`.
///
/// `repair_incomplete` is the runtime's row-scoped reason for *still*
/// refusing the row after the printed command is applied. The command
/// carries only accepted hosts, so the only row-scoped thing that can remain
/// is a private carve-out no host grants; the operator must fix
/// `egress_allow_private` by hand, and the report says so rather than calling
/// the command a complete repair. Deployment-wide refusals (NAT64 prefixes,
/// the connection ceiling) are never attributed to a row: the caller reports
/// them once, naming their own config paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressGapPlan {
    /// The declaration is covered and the runtime accepts the row as it is.
    Nothing,
    /// A canonical (or absent) row: destinations the runtime denies and/or a
    /// row the runtime refuses, plus the one command that grants the
    /// declaration with every accepted existing host kept.
    Grant {
        missing: Vec<String>,
        invalid: Vec<String>,
        rejected: Option<String>,
        repair_incomplete: Option<String>,
        command: String,
    },
    /// The grant is stranded on a legacy row, so the rename is always
    /// required: the runtime enforces nothing until it happens. `grant` is
    /// `None` only when the rename alone yields a row the runtime accepts
    /// that covers the declaration; otherwise the grant command follows the
    /// rename, because renaming alone would put a refused or incomplete
    /// allowlist into effect.
    Migrate {
        legacy_row: String,
        missing: Vec<String>,
        invalid: Vec<String>,
        rejected: Option<String>,
        repair_incomplete: Option<String>,
        grant: Option<String>,
    },
}

/// Decide what to report for one instance from its declaration and grant state.
///
/// Pure apart from consulting the runtime's own policy constructor: the caller
/// renders the plan through Fluent. Keeping the decision here means the
/// "is migration needed?" and "would the runtime accept this?" rules are
/// unit-testable functions rather than control flow interleaved with string
/// formatting.
#[must_use]
pub fn plan_egress_gap(
    instance_key: &str,
    declared: &[String],
    state: &EgressGrantState,
    runtime: &EgressRuntimeInputs,
) -> EgressGapPlan {
    match state {
        EgressGrantState::Enforced {
            granted,
            allow_private,
        } => {
            // Coverage is judged over the entries the runtime accepts; whether
            // the row as a whole is accepted is the runtime's call.
            let (valid, invalid) = partition_valid_hosts(granted);
            let rejected = row_rejection(granted, allow_private, runtime);
            let diff = diff_declaration(declared, &valid);
            if diff.declared_not_granted.is_empty() && rejected.is_none() {
                return EgressGapPlan::Nothing;
            }
            let union = diff.union();
            let repair_incomplete = row_rejection(&union, allow_private, runtime);
            EgressGapPlan::Grant {
                command: egress_set_command(instance_key, &union),
                missing: diff.declared_not_granted,
                invalid,
                rejected,
                repair_incomplete,
            }
        }
        EgressGrantState::Stranded {
            legacy_row,
            authored,
            allow_private,
        } => {
            // Compare against what the rename WILL bring into effect, not
            // against the (empty) grant the runtime enforces today: the rename
            // is planned unconditionally, so the open question is whether a
            // grant step has to follow it. It must when a declared destination
            // is still uncovered, and it must when the runtime would refuse
            // the row the rename produces, because renaming alone would then
            // put a refused allowlist into effect.
            let (valid, invalid) = partition_valid_hosts(authored);
            let rejected = row_rejection(authored, allow_private, runtime);
            let diff = diff_declaration(declared, &valid);
            let needs_grant = !diff.declared_not_granted.is_empty() || rejected.is_some();
            let (grant, repair_incomplete) = if needs_grant {
                let union = diff.union();
                (
                    Some(egress_set_command(instance_key, &union)),
                    row_rejection(&union, allow_private, runtime),
                )
            } else {
                (None, None)
            };
            EgressGapPlan::Migrate {
                legacy_row: legacy_row.clone(),
                missing: diff.declared_not_granted,
                invalid,
                rejected,
                repair_incomplete,
                grant,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn canonical_hosts_sorts_dedups_and_keeps_invalid_entries_visible() {
        // Sorted + deduplicated so seeded output and diffs are deterministic.
        assert_eq!(
            canonical_hosts(&v(&["b.example.com", "a.example.com", "b.example.com"])),
            v(&["a.example.com", "b.example.com"])
        );
        // Whitespace is normalized away, empties vanish.
        assert_eq!(
            canonical_hosts(&v(&["  a.example.com  ", ""])),
            v(&["a.example.com"])
        );
        // A hand-edited entry that fails the grammar is still surfaced: the
        // operator has to be able to see what is actually on disk.
        assert_eq!(
            canonical_hosts(&v(&["NOT-LOWERCASE.example.com"])),
            v(&["NOT-LOWERCASE.example.com"])
        );
    }

    #[test]
    fn diff_is_empty_when_declaration_matches_grant_in_any_order() {
        let diff = diff_declaration(
            &v(&["b.example.com", "a.example.com"]),
            &v(&["a.example.com", "b.example.com", "a.example.com"]),
        );
        assert!(diff.is_empty(), "same set, different order/dupes: {diff:?}");
        assert!(!should_report_diff(&diff));
    }

    #[test]
    fn diff_splits_declared_not_granted_from_granted_not_declared() {
        let diff = diff_declaration(
            &v(&["api.example.com", "api2.example.com"]),
            &v(&["api.example.com", "gitea.internal.example.com"]),
        );
        assert_eq!(diff.declared_not_granted, v(&["api2.example.com"]));
        assert_eq!(
            diff.granted_not_declared,
            v(&["gitea.internal.example.com"])
        );
        assert!(should_report_diff(&diff));
    }

    #[test]
    fn union_adds_the_declaration_without_revoking_an_operator_authored_grant() {
        // The apply-command's value: `config set` REPLACES the list, so the
        // command must carry the operator's own grant through.
        let diff = diff_declaration(
            &v(&["api.example.com", "api2.example.com"]),
            &v(&["api.example.com", "gitea.internal.example.com"]),
        );
        assert_eq!(
            diff.union(),
            v(&[
                "api.example.com",
                "api2.example.com",
                "gitea.internal.example.com"
            ])
        );
    }

    #[test]
    fn a_manifest_declaring_nothing_never_reports_operator_authored_grants() {
        // The second grant path: the author cannot know a self-hosted
        // destination, so the operator authors it. Silence, not a diff.
        let diff = diff_declaration(&[], &v(&["gitea.internal.example.com"]));
        assert_eq!(
            diff.granted_not_declared,
            v(&["gitea.internal.example.com"])
        );
        assert!(
            !should_report_diff(&diff),
            "an empty declaration must not report the operator's own grants"
        );
    }

    #[test]
    fn set_command_targets_the_instance_row_and_quotes_the_value() {
        // The path is keyed by the opaque `zpi1_` instance key, not the
        // package name: that is the row `entry_config` resolves against, so
        // config and grant stay on one row.
        let key = "zpi1_WyJ3ZWF0aGVyLXRvb2wiLCJ0b29sIiwid2VhdGhlci10b29sIl0";
        let cmd = egress_set_command(key, &v(&["api.example.com", "*.cdn.example.com"]));
        assert_eq!(
            cmd,
            format!(
                "zeroclaw config set plugins.entries.{key}.egress_hosts \"api.example.com,*.cdn.example.com\""
            )
        );
        assert!(
            !cmd.contains("plugins.entries.weather-tool."),
            "the command must not address a package-name-keyed row: {cmd}"
        );
        assert!(
            cmd.contains('"'),
            "an unquoted '*.suffix' would be glob-expanded by the operator's shell"
        );
    }

    #[test]
    fn a_package_name_row_strands_the_grant_only_while_the_canonical_row_is_absent() {
        let key = "zpi1_WyJ3ZWF0aGVyLXRvb2wiLCJ0b29sIiwid2VhdGhlci10b29sIl0";
        let legacy = v(&["weather-tool"]);

        // Pre-typed-config install: the row is still package-name keyed, so the
        // printed `config set ...<key>.egress_hosts` command cannot resolve.
        assert_eq!(
            stranded_legacy_grant_row(key, &legacy, &v(&["weather-tool"])),
            Some("weather-tool".to_string())
        );

        // The canonical row is what the command addresses. Once it exists the
        // command resolves, even if the stale row was left behind.
        assert_eq!(
            stranded_legacy_grant_row(key, &legacy, &v(&[key, "weather-tool"])),
            None,
            "a present canonical row makes the grant command resolvable"
        );

        // Someone else's package-name row is not this instance's grant.
        assert_eq!(
            stranded_legacy_grant_row(key, &legacy, &v(&["other-tool"])),
            None
        );

        // No rows at all: renaming nothing would not help, so there is no
        // migration step to print.
        assert_eq!(stranded_legacy_grant_row(key, &legacy, &[]), None);
    }

    #[test]
    fn suffix_patterns_and_apex_are_distinct_destinations() {
        // The grammar treats `*.example.com` and `example.com` as different
        // entries, and containment never collapses them: a suffix grant does
        // not cover its apex, and an exact grant does not cover a suffix.
        let diff = diff_declaration(&v(&["*.example.com"]), &v(&["example.com"]));
        assert_eq!(diff.declared_not_granted, v(&["*.example.com"]));
        assert_eq!(diff.granted_not_declared, v(&["example.com"]));
    }

    #[test]
    fn a_declared_subdomain_covered_by_a_granted_wildcard_is_not_a_gap() {
        // IftekharUddin's blocker: the runtime reaches `api.example.com` through
        // a granted `*.example.com`, so the declaration-versus-grant diagnostic
        // must NOT report it as an ungranted gap — it must never tell the
        // operator to grant a destination that is already reachable.
        let diff = diff_declaration(&v(&["api.example.com"]), &v(&["*.example.com"]));
        assert!(
            diff.declared_not_granted.is_empty(),
            "a declared host a granted wildcard covers is already reachable, not a gap: {diff:?}"
        );
        // The broader grant is still surfaced informationally (left in place):
        // `*.example.com` reaches more than the declared `api.example.com`.
        assert_eq!(diff.granted_not_declared, v(&["*.example.com"]));

        // Apex is NOT covered by the suffix, so a declared apex stays an
        // actionable gap even when a `*.` of the same domain is granted.
        let apex = diff_declaration(&v(&["example.com"]), &v(&["*.example.com"]));
        assert_eq!(
            apex.declared_not_granted,
            v(&["example.com"]),
            "`*.example.com` never covers its apex `example.com`"
        );
    }

    #[test]
    fn a_grant_within_a_declared_wildcard_is_not_reported_as_beyond_the_declaration() {
        // The informational side, made symmetric with runtime containment: an
        // operator who narrowed a declared `*.example.com` to a single
        // subdomain has granted WITHIN the declaration, not beyond it, so
        // `api.example.com` is not reported as "granted, no longer declared".
        // The unmet remainder of the declared wildcard is still the actionable
        // gap.
        let diff = diff_declaration(&v(&["*.example.com"]), &v(&["api.example.com"]));
        assert!(
            diff.granted_not_declared.is_empty(),
            "a grant the declaration covers is within it, not beyond it: {diff:?}"
        );
        assert_eq!(
            diff.declared_not_granted,
            v(&["*.example.com"]),
            "the rest of the declared wildcard the narrow grant does not cover is still a gap"
        );
    }

    fn rt() -> EgressRuntimeInputs {
        EgressRuntimeInputs {
            nat64_prefixes: Vec::new(),
            max_connections_per_instance: 4,
        }
    }

    fn enforced(granted: &[&str]) -> EgressGrantState {
        EgressGrantState::Enforced {
            granted: v(granted),
            allow_private: Vec::new(),
        }
    }

    fn stranded(authored: &[&str]) -> EgressGrantState {
        EgressGrantState::Stranded {
            legacy_row: "weather-tool".to_string(),
            authored: v(authored),
            allow_private: Vec::new(),
        }
    }

    #[test]
    fn grant_state_separates_what_is_enforced_from_what_was_authored() {
        let key = "zpi1_WyJ3ZWF0aGVyLXRvb2wiLCJ0b29sIiwid2VhdGhlci10b29sIl0";
        let legacy = v(&["weather-tool"]);
        // Stands in for `entry_egress`: only the legacy row carries hosts.
        let lookup = |row: &str| {
            if row == "weather-tool" {
                (v(&["api.example.com", "gitea.example.net"]), Vec::new())
            } else {
                (Vec::new(), Vec::new())
            }
        };

        // Canonical row absent, legacy row present: stranded, and the
        // authored grant is carried through for the command to preserve.
        assert_eq!(
            resolve_grant_state(key, &legacy, &v(&["weather-tool"]), lookup),
            stranded(&["api.example.com", "gitea.example.net"])
        );
        // Canonical row present: what is enforced is that row's grant (here
        // nothing), never the leftover legacy row's.
        assert_eq!(
            resolve_grant_state(key, &legacy, &v(&[key, "weather-tool"]), lookup),
            enforced(&[])
        );
        // No rows at all: enforced-empty, not stranded — renaming nothing
        // would not help, and the grant command is the right next step.
        assert_eq!(
            resolve_grant_state(key, &legacy, &[], lookup),
            enforced(&[])
        );
    }

    #[test]
    fn a_stranded_grant_always_plans_the_rename_even_when_it_covers_the_declaration() {
        // The false negative this split exists to make impossible: the
        // authored grant covers the declaration, so a diff against it is
        // empty — but the runtime enforces nothing until the rename. The plan
        // must be Migrate, and with nothing left to grant and a row the
        // runtime accepts, no command.
        let key = "zpi1_k";
        let state = stranded(&["api.example.com", "gitea.example.net"]);
        assert_eq!(
            plan_egress_gap(key, &v(&["api.example.com"]), &state, &rt()),
            EgressGapPlan::Migrate {
                legacy_row: "weather-tool".to_string(),
                missing: Vec::new(),
                invalid: Vec::new(),
                rejected: None,
                repair_incomplete: None,
                grant: None,
            }
        );
        // A wildcard that covers the declaration is the same case.
        assert!(matches!(
            plan_egress_gap(
                key,
                &v(&["api.example.com"]),
                &stranded(&["*.example.com"]),
                &rt()
            ),
            EgressGapPlan::Migrate { grant: None, .. }
        ));
        // Even with nothing declared: the operator's own grant is inert until
        // the rename, and `config set` cannot target the row until then.
        assert!(matches!(
            plan_egress_gap(key, &[], &state, &rt()),
            EgressGapPlan::Migrate { grant: None, .. }
        ));
    }

    #[test]
    fn a_stranded_grant_with_an_uncovered_declaration_plans_the_rename_then_a_union_grant() {
        // The grant-loss case: the grant step must carry the operator-only
        // host forward, because `config set` replaces the list.
        let key = "zpi1_k";
        let state = stranded(&["api.example.com", "gitea.example.net"]);
        let plan = plan_egress_gap(
            key,
            &v(&["api.example.com", "api2.example.com"]),
            &state,
            &rt(),
        );
        let EgressGapPlan::Migrate {
            legacy_row,
            missing,
            invalid,
            rejected,
            repair_incomplete,
            grant: Some(command),
        } = plan
        else {
            panic!("expected a migrate plan with a grant step: {plan:?}");
        };
        assert_eq!(legacy_row, "weather-tool");
        assert_eq!(missing, v(&["api2.example.com"]));
        assert!(invalid.is_empty());
        assert_eq!(rejected, None, "a valid row is not refused");
        assert_eq!(
            repair_incomplete, None,
            "the union is a row the runtime accepts"
        );
        assert_eq!(
            command,
            egress_set_command(
                key,
                &v(&["api.example.com", "api2.example.com", "gitea.example.net"])
            )
        );
    }

    #[test]
    fn an_enforced_grant_plans_exactly_as_the_canonical_diagnostic_always_did() {
        let key = "zpi1_k";
        // Covered (through the wildcard) and accepted: nothing to say.
        assert_eq!(
            plan_egress_gap(
                key,
                &v(&["api.example.com"]),
                &enforced(&["*.example.com"]),
                &rt()
            ),
            EgressGapPlan::Nothing
        );
        // A gap: the union command against the canonical key.
        assert_eq!(
            plan_egress_gap(
                key,
                &v(&["api.example.com", "api2.example.com"]),
                &enforced(&["api.example.com"]),
                &rt()
            ),
            EgressGapPlan::Grant {
                missing: v(&["api2.example.com"]),
                invalid: Vec::new(),
                rejected: None,
                repair_incomplete: None,
                command: egress_set_command(key, &v(&["api.example.com", "api2.example.com"])),
            }
        );
        // No row at all reads as enforced-empty: every declared host is a gap.
        assert!(matches!(
            plan_egress_gap(key, &v(&["api.example.com"]), &enforced(&[]), &rt()),
            EgressGapPlan::Grant { .. }
        ));
        // No row and nothing declared: an empty grant is a row the runtime
        // accepts (it means no reach), so there is nothing to report.
        assert_eq!(
            plan_egress_gap(key, &[], &enforced(&[]), &rt()),
            EgressGapPlan::Nothing
        );
    }

    #[test]
    fn partition_judges_raw_entries_exactly_as_the_runtime_does() {
        let (valid, invalid) = partition_valid_hosts(&v(&[
            "api.example.com",
            "*.com",
            " api.example.com ",
            "",
            "*.example.com",
        ]));
        assert_eq!(valid, v(&["*.example.com", "api.example.com"]));
        // `*.com` wildcards a single label; the padded entry has boundary
        // whitespace; the empty entry is empty. The runtime rejects all three
        // on the raw bytes, so no trimming or skipping may rescue them here.
        assert_eq!(invalid, v(&["", " api.example.com ", "*.com"]));
    }

    #[test]
    fn runtime_rejection_is_the_policy_constructors_verdict_with_its_scope() {
        assert_eq!(
            runtime_rejection(&v(&["api.example.com"]), &[], &rt()),
            None
        );
        assert_eq!(
            runtime_rejection(&[], &[], &rt()),
            None,
            "an empty grant is accepted and means no reach"
        );
        let padded = runtime_rejection(&v(&[" api.example.com "]), &[], &rt())
            .expect("boundary whitespace is refused");
        assert_eq!(padded.scope, RejectionScope::Row);
        assert!(padded.reason.contains("whitespace"), "{}", padded.reason);
        let carveout =
            runtime_rejection(&v(&["api.example.com"]), &v(&["other.example.com"]), &rt())
                .expect("a carve-out no host grants is refused");
        assert_eq!(carveout.scope, RejectionScope::Row);
        assert!(
            carveout.reason.contains("not granted"),
            "{}",
            carveout.reason
        );

        // Deployment-wide refusals point at the deployment, not the row.
        let bad_nat64 = EgressRuntimeInputs {
            nat64_prefixes: v(&["2001:db8::/97"]),
            max_connections_per_instance: 4,
        };
        let refused = runtime_rejection(&v(&["api.example.com"]), &[], &bad_nat64)
            .expect("a malformed NAT64 prefix list is refused");
        assert_eq!(refused.scope, RejectionScope::Deployment);
        let zero = EgressRuntimeInputs {
            nat64_prefixes: Vec::new(),
            max_connections_per_instance: 0,
        };
        assert_eq!(
            runtime_rejection(&v(&["api.example.com"]), &[], &zero)
                .expect("a zero ceiling is refused")
                .scope,
            RejectionScope::Deployment
        );
        // The split halves agree with the whole.
        assert!(deployment_rejection(&bad_nat64).is_some());
        assert!(deployment_rejection(&zero).is_some());
        assert_eq!(deployment_rejection(&rt()), None);
        assert_eq!(
            row_rejection(&v(&["api.example.com"]), &[], &bad_nat64),
            None
        );
        assert!(row_rejection(&v(&[" api.example.com "]), &[], &rt()).is_some());
    }

    #[test]
    fn a_deployment_wide_refusal_is_never_attributed_to_a_row() {
        // The row is valid and covers the declaration; only the deployment's
        // NAT64 list is malformed. The plan must not call the row refused, must
        // not print a no-op command, and must not point at
        // `egress_allow_private`. The caller reports the deployment refusal
        // once, on its own.
        let key = "zpi1_k";
        let bad_nat64 = EgressRuntimeInputs {
            nat64_prefixes: v(&["2001:db8::/97"]),
            max_connections_per_instance: 16,
        };
        assert_eq!(
            plan_egress_gap(
                key,
                &v(&["api.example.com"]),
                &enforced(&["api.example.com"]),
                &bad_nat64
            ),
            EgressGapPlan::Nothing
        );
        assert!(matches!(
            plan_egress_gap(
                key,
                &v(&["api.example.com"]),
                &stranded(&["api.example.com"]),
                &bad_nat64
            ),
            EgressGapPlan::Migrate {
                rejected: None,
                repair_incomplete: None,
                grant: None,
                ..
            }
        ));
        // A genuine row problem still surfaces under a bad NAT64 list, because
        // the constructor judges the hosts before the prefixes.
        assert!(matches!(
            plan_egress_gap(
                key,
                &v(&["api.example.com"]),
                &enforced(&[" api.example.com "]),
                &bad_nat64
            ),
            EgressGapPlan::Grant {
                rejected: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn a_rejected_authored_entry_never_covers_and_is_kept_out_of_the_command() {
        // The containment relation trusts its inputs, so a rejected `*.com`
        // would "cover" `api.com` and a naive planner would print the rename
        // alone. After that rename the runtime would build the policy from
        // the row, reject `*.com`, and deny every request. The plan must name
        // the entry, keep the rename, and print a grant that omits it.
        let key = "zpi1_k";
        let plan = plan_egress_gap(key, &v(&["api.com"]), &stranded(&["*.com"]), &rt());
        let EgressGapPlan::Migrate {
            missing,
            invalid,
            rejected,
            repair_incomplete,
            grant: Some(command),
            ..
        } = plan
        else {
            panic!("a rejected entry must force the grant step: {plan:?}");
        };
        assert_eq!(missing, v(&["api.com"]), "`*.com` covers nothing");
        assert_eq!(invalid, v(&["*.com"]));
        assert!(
            rejected
                .expect("the runtime refuses the row")
                .contains("*.com")
        );
        assert_eq!(repair_incomplete, None, "the union is accepted");
        assert_eq!(command, egress_set_command(key, &v(&["api.com"])));
        assert!(!command.contains("*.com"), "{command}");
    }

    #[test]
    fn a_padded_entry_is_refused_like_the_runtime_refuses_it_and_the_command_repairs_it() {
        // A grant the runtime refuses for boundary whitespace, on a canonical
        // row that otherwise covers the declaration. A planner that trimmed
        // before judging would call this healthy while every request is
        // denied. The repair carries the canonical declared host and leaves
        // the padded entry behind.
        let key = "zpi1_k";
        let plan = plan_egress_gap(
            key,
            &v(&["api.example.com"]),
            &enforced(&[" api.example.com "]),
            &rt(),
        );
        let EgressGapPlan::Grant {
            missing,
            invalid,
            rejected,
            repair_incomplete,
            command,
        } = plan
        else {
            panic!("a refused row must be reported: {plan:?}");
        };
        assert_eq!(
            missing,
            v(&["api.example.com"]),
            "a refused entry covers nothing"
        );
        assert_eq!(invalid, v(&[" api.example.com "]));
        assert!(
            rejected
                .expect("the runtime refuses the row")
                .contains("whitespace")
        );
        assert_eq!(repair_incomplete, None);
        assert_eq!(command, egress_set_command(key, &v(&["api.example.com"])));
    }

    #[test]
    fn an_ungranted_private_carveout_forces_the_grant_step_and_is_reported_as_incomplete() {
        // The hosts are valid and cover the declaration, so a host-only
        // planner would offer the rename alone. But `egress_allow_private`
        // names a host no grant covers, and the runtime refuses the whole row
        // for it. The rename must not be offered alone, and because the
        // printed command only replaces the hosts, the report must say the
        // carve-out still has to be fixed by hand.
        let key = "zpi1_k";
        let state = EgressGrantState::Stranded {
            legacy_row: "weather-tool".to_string(),
            authored: v(&["api.example.com"]),
            allow_private: v(&["other.example.com"]),
        };
        let plan = plan_egress_gap(key, &v(&["api.example.com"]), &state, &rt());
        let EgressGapPlan::Migrate {
            missing,
            rejected,
            repair_incomplete,
            grant,
            ..
        } = plan
        else {
            panic!("expected a migrate plan: {plan:?}");
        };
        assert!(missing.is_empty(), "the hosts cover the declaration");
        assert!(
            rejected
                .expect("the runtime refuses the row")
                .contains("not granted")
        );
        assert!(
            grant.is_some(),
            "the rename alone would put a refused row into effect"
        );
        assert!(
            repair_incomplete
                .expect("the command replaces only the hosts")
                .contains("egress_allow_private")
        );

        // Same row, canonical: reported as a repair even though the
        // declaration is covered — silence would call a denied instance
        // healthy.
        let plan = plan_egress_gap(
            key,
            &v(&["api.example.com"]),
            &EgressGrantState::Enforced {
                granted: v(&["api.example.com"]),
                allow_private: v(&["other.example.com"]),
            },
            &rt(),
        );
        assert!(matches!(
            plan,
            EgressGapPlan::Grant {
                rejected: Some(_),
                repair_incomplete: Some(_),
                ..
            }
        ));
    }
}
