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

use zeroclaw_infra::net_guard::normalize_egress_pattern;

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
    /// Declared but not granted — denials waiting to happen.
    pub declared_not_granted: Vec<String>,
    /// Granted but no longer declared — left in place; informational only.
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
/// Set comparison on canonical forms: a destination that differs only by
/// authoring order or duplication is not a difference.
#[must_use]
pub fn diff_declaration(declared: &[String], granted: &[String]) -> EgressDeclarationDiff {
    let declared = canonical_hosts(declared);
    let granted = canonical_hosts(granted);
    let declared_not_granted: Vec<String> = declared
        .iter()
        .filter(|h| !granted.contains(h))
        .cloned()
        .collect();
    let granted_not_declared: Vec<String> = granted
        .iter()
        .filter(|h| !declared.contains(h))
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
        // entries; the diff must not collapse them.
        let diff = diff_declaration(&v(&["*.example.com"]), &v(&["example.com"]));
        assert_eq!(diff.declared_not_granted, v(&["*.example.com"]));
        assert_eq!(diff.granted_not_declared, v(&["example.com"]));
    }
}
