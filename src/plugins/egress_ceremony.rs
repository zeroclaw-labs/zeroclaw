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
/// additions.
///
/// The joined list is one quoted argument. The hosts come from a plugin's
/// manifest, which is publisher-controlled text, and the operator pastes this
/// command into their shell with their own authority: a POSIX shell performs
/// `$(...)`, backtick and `$var` substitution inside double quotes, so a
/// declared `$(id).example.com` would run `id` before ZeroClaw saw the
/// argument. The value is therefore quoted so that the operator's shell on
/// this host passes every byte literally, including the `*` that starts a
/// suffix pattern (see [`ShellDialect`]); on Windows, where the shell cannot
/// be known and `cmd.exe` has no fully literal form, a value that either
/// Windows shell would expand is refused rather than rendered (see
/// [`POWERSHELL_ONLY_MARKER`]). The one thing the shell may do to this
/// argument is nothing.
///
/// The directory carries the same treatment. `--config-dir` (and the
/// `ZEROCLAW_CONFIG_DIR` it sets) is process-local, so a command copied out of
/// `zeroclaw --config-dir /srv/a plugin list` and pasted into the operator's
/// shell would otherwise load the ambient default configuration. The canonical
/// row key names the package, capability and binding but not the profile, so
/// that command would replace *another* profile's allowlist with a list
/// computed from this one. Every printed command therefore carries the
/// directory it was computed against, shell-quoted.
#[must_use]
pub fn egress_set_command(
    config_dir: &std::path::Path,
    instance_key: &str,
    hosts: &[String],
) -> String {
    egress_set_command_for(ShellDialect::host(), config_dir, instance_key, hosts)
}

/// [`egress_set_command`] rendered for an explicit shell dialect, so the form
/// printed on one host can be proven on another.
#[must_use]
pub fn egress_set_command_for(
    dialect: ShellDialect,
    config_dir: &std::path::Path,
    instance_key: &str,
    hosts: &[String],
) -> String {
    let joined = hosts.join(",");
    let (dialect, marker) = dialect.command_form(&[&config_dir.to_string_lossy(), &joined]);
    format!(
        "{marker}{} config set {} {}",
        zeroclaw_invocation_for(dialect, config_dir),
        egress_hosts_path(instance_key),
        dialect.quote_literal(&joined)
    )
}

/// The command that creates an instance's missing row with its grant.
///
/// `config set plugins.entries.<key>.egress_hosts` resolves only keys already
/// present in config, so for an instance with no row it fails with an unknown
/// property and the plugin stays denied. That happens to an HTTP plugin
/// installed before this ceremony existed, and after a row is removed by hand.
/// `config patch` creates the keyed row for an `add` and writes the list in the
/// same transaction, so the one printed command is the whole repair.
///
/// The patch is JSON on standard input: `printf` feeds it on POSIX shells, and
/// PowerShell pipes a string to a native command's input. The JSON's double
/// quotes are not literal in `cmd.exe`, so on Windows this is always the
/// PowerShell form behind [`POWERSHELL_ONLY_MARKER`].
#[must_use]
pub fn egress_create_command(
    config_dir: &std::path::Path,
    instance_key: &str,
    hosts: &[String],
) -> String {
    egress_create_command_for(ShellDialect::host(), config_dir, instance_key, hosts)
}

/// [`egress_create_command`] rendered for an explicit shell dialect.
#[must_use]
pub fn egress_create_command_for(
    dialect: ShellDialect,
    config_dir: &std::path::Path,
    instance_key: &str,
    hosts: &[String],
) -> String {
    let patch = serde_json::json!([{
        "op": "add",
        "path": format!("/plugins/entries/{instance_key}/egress_hosts"),
        "value": hosts,
    }])
    .to_string();
    let (dialect, marker) = dialect.command_form(&[&config_dir.to_string_lossy(), &patch]);
    let feed = match dialect {
        ShellDialect::Posix => format!("printf '%s\\n' {}", dialect.quote_literal(&patch)),
        // PowerShell; a Windows line never gets here, because the patch's
        // double quotes always send it to the PowerShell form.
        ShellDialect::PowerShell | ShellDialect::Windows => dialect.quote_literal(&patch),
    };
    format!(
        "{marker}{feed} | {} config patch -",
        zeroclaw_invocation_for(dialect, config_dir)
    )
}

/// `zeroclaw --config-dir '<dir>'`: the invocation prefix every printed
/// operator command starts with, so it acts on the configuration the operator
/// inspected rather than whichever one their shell resolves by default,
/// rendered for an explicit shell dialect.
#[must_use]
pub fn zeroclaw_invocation_for(dialect: ShellDialect, config_dir: &std::path::Path) -> String {
    format!(
        "zeroclaw --config-dir {}",
        dialect.quote_literal(&config_dir.to_string_lossy())
    )
}

/// The quoting dialect of the shell an operator pastes a printed command into.
///
/// The commands this module renders are copied out of `zeroclaw plugin install`
/// and `zeroclaw plugin list` output and pasted into the operator's interactive
/// shell, so a value is quoted for *that* shell, not for the shell the runtime
/// uses to execute tools. ZeroClaw cannot see which shell its output lands in.
/// On Linux and macOS every supported shell shares the POSIX single-quote
/// form. On Windows the operator may be in either of two shells, `cmd.exe`
/// (the default the native runtime documents) or PowerShell (the default
/// Windows Terminal opens), so the Windows form has to be literal in both, and
/// a value that is literal in neither is refused rather than trusted to
/// whichever shell happens to be open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellDialect {
    /// `sh`, `bash`, `zsh`, `fish`: single quotes protect every character but
    /// the single quote itself, which is closed, escaped and reopened
    /// (`'it'\''s'`).
    Posix,
    /// PowerShell (`powershell`, `pwsh`) on its own: single quotes protect
    /// every character but the single quote itself, which is doubled
    /// (`'it''s'`). This form is never printed unmarked; it is what a
    /// [`Self::Windows`] line falls back to, behind [`POWERSHELL_ONLY_MARKER`],
    /// when a value cannot be passed literally from `cmd.exe`.
    PowerShell,
    /// A Windows console, whichever of `cmd.exe` and PowerShell is running it:
    /// one double-quoted argument, which both shells hand to the native binary
    /// untouched as long as the value holds nothing either of them expands
    /// inside double quotes. [`windows_form_is_literal`] is that test.
    Windows,
}

/// Whether `raw` survives a Windows console as one double-quoted argument, in
/// both `cmd.exe` and PowerShell.
///
/// `cmd.exe` has no fully literal quoting form: inside double quotes it still
/// expands `%name%`, expands `!name!` under delayed expansion, cannot escape an
/// embedded `"`, and treats a single quote as an ordinary character.
/// PowerShell's double quotes interpolate `$` and the backtick. Both shells
/// then hand the argument to the native binary through the C runtime's
/// command-line rules, where a trailing backslash swallows the closing quote.
/// A value is literal in both shells only when it contains none of `"`, `%`,
/// `!`, `$` and `` ` ``, no control character, and does not end in `\`.
///
/// Every declared host of the ordinary shape (`api.example.com`,
/// `*.cdn.example.com`) and every ordinary profile path, spaces included,
/// passes. A manifest that declares `$(id).example.com` does not: the egress
/// grammar accepts it, and no Windows form could promise it reaches
/// `config set` unexpanded, so [`egress_set_command`] refuses it instead.
#[must_use]
pub fn windows_form_is_literal(raw: &str) -> bool {
    !raw.ends_with('\\')
        && !raw
            .chars()
            .any(|c| matches!(c, '"' | '%' | '!' | '$' | '`') || c.is_control())
}

/// The prefix of a Windows command line whose value cannot be passed literally
/// from `cmd.exe`.
///
/// It begins with `#`, which `cmd.exe` cannot run and PowerShell reads as a
/// comment to the end of the line, so pasting the whole line into either shell
/// executes nothing. The command after the marker is the
/// [`ShellDialect::PowerShell`] form, and the marker says so: the operator
/// copies that part into PowerShell alone, where single quotes make every byte
/// literal.
pub const POWERSHELL_ONLY_MARKER: &str =
    "# PowerShell only, cmd.exe cannot pass this value literally: ";

impl ShellDialect {
    /// The dialect of the operator's shell on the host this binary runs on:
    /// the Windows console form on Windows, POSIX everywhere else.
    #[must_use]
    pub const fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }

    /// Quote `raw` as one literal argument in this dialect: after the shell has
    /// parsed the result, the argument is `raw`, byte for byte.
    ///
    /// For [`Self::Windows`] that promise holds only while
    /// [`windows_form_is_literal`] does; a value outside it is rendered in the
    /// [`Self::PowerShell`] form, and it is the command renderer's job
    /// ([`Self::command_form`]) to mark the whole line so `cmd.exe` never runs
    /// it.
    #[must_use]
    pub fn quote_literal(self, raw: &str) -> String {
        match self {
            Self::Posix => format!("'{}'", raw.replace('\'', "'\\''")),
            Self::PowerShell => format!("'{}'", raw.replace('\'', "''")),
            Self::Windows if windows_form_is_literal(raw) => format!("\"{raw}\""),
            Self::Windows => Self::PowerShell.quote_literal(raw),
        }
    }

    /// The dialect a whole command line carrying `values` is rendered in, and
    /// the marker it starts with.
    ///
    /// Every dialect but [`Self::Windows`] renders itself, unmarked. A Windows
    /// line renders itself only when every value passes
    /// [`windows_form_is_literal`]; otherwise the line is the PowerShell form
    /// behind [`POWERSHELL_ONLY_MARKER`], so that no part of it can execute in
    /// `cmd.exe` and the shell it is meant for is named in the line itself.
    #[must_use]
    pub fn command_form(self, values: &[&str]) -> (Self, &'static str) {
        match self {
            Self::Windows if !values.iter().all(|value| windows_form_is_literal(value)) => {
                (Self::PowerShell, POWERSHELL_ONLY_MARKER)
            }
            other => (other, ""),
        }
    }
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
        /// Whether the canonical row exists. It decides the repair command:
        /// `config set` resolves only keys already in config, so a missing
        /// row needs [`egress_create_command`], which creates it.
        row_exists: bool,
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
                row_exists: row_names.iter().any(|row| row == instance_key),
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
    config_dir: &std::path::Path,
    instance_key: &str,
    declared: &[String],
    state: &EgressGrantState,
    runtime: &EgressRuntimeInputs,
) -> EgressGapPlan {
    match state {
        EgressGrantState::Enforced {
            granted,
            allow_private,
            row_exists,
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
            let command = if *row_exists {
                egress_set_command(config_dir, instance_key, &union)
            } else {
                egress_create_command(config_dir, instance_key, &union)
            };
            EgressGapPlan::Grant {
                command,
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
                    Some(egress_set_command(config_dir, instance_key, &union)),
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
    /// The configuration every test command is computed against.
    fn dir() -> &'static std::path::Path {
        std::path::Path::new("/srv/zeroclaw/profile-a")
    }

    #[test]
    fn every_printed_command_targets_the_selected_configuration_shell_quoted() {
        let awkward = std::path::Path::new("/tmp/it's here/profile a");
        let command = super::egress_set_command_for(
            super::ShellDialect::Posix,
            awkward,
            "zpi1_k",
            &["api.example.com".to_string()],
        );
        assert_eq!(
            command,
            "zeroclaw --config-dir '/tmp/it'\\''s here/profile a' config set \
             plugins.entries.zpi1_k.egress_hosts 'api.example.com'"
        );
        assert!(
            command.starts_with(&super::zeroclaw_invocation_for(
                super::ShellDialect::Posix,
                awkward
            )),
            "the grant command must start with the selected-configuration invocation"
        );
    }

    /// The host form is the dialect of the shell an operator on this platform
    /// pastes into, and the plain renderers are that form exactly.
    #[test]
    fn the_printed_form_is_the_host_shells_dialect() {
        let expected = if cfg!(windows) {
            super::ShellDialect::Windows
        } else {
            super::ShellDialect::Posix
        };
        assert_eq!(super::ShellDialect::host(), expected);
        let dir = std::path::Path::new("/srv/it's here/profile a");
        let hosts = [
            "it's.example.com".to_string(),
            "*.cdn.example.com".to_string(),
        ];
        assert_eq!(
            super::egress_set_command(dir, "zpi1_k", &hosts),
            super::egress_set_command_for(expected, dir, "zpi1_k", &hosts)
        );
    }

    /// The PowerShell form is PowerShell's literal string: single quotes, with
    /// an embedded quote doubled rather than backslash-escaped, because
    /// PowerShell gives a backslash no meaning and would print `'it'\''s'` as
    /// three tokens. Every metacharacter PowerShell expands inside double
    /// quotes (`$env:NAME`, `$(...)`, a backtick escape) stays literal, as does
    /// the space in the profile path and the `*` of a suffix pattern. The
    /// egress grammar (`normalize_egress_pattern`) rejects none of these
    /// characters, so a manifest can declare each of these host shapes, and
    /// this is the form a Windows line falls back to for them.
    #[test]
    fn the_powershell_form_is_powershells_literal_string() {
        let hosts = [
            "$(id).example.com",
            "`id`.example.com",
            "$env:username.example.com",
            "*.cdn.example.com",
            "it's.example.com",
        ]
        .map(String::from);
        let dir = std::path::Path::new(r"C:\Users\op erator\it's\.zeroclaw");
        let command =
            super::egress_set_command_for(super::ShellDialect::PowerShell, dir, "zpi1_k", &hosts);
        let expected = concat!(
            r"zeroclaw --config-dir 'C:\Users\op erator\it''s\.zeroclaw' ",
            "config set plugins.entries.zpi1_k.egress_hosts ",
            "'$(id).example.com,`id`.example.com,$env:username.example.com,",
            "*.cdn.example.com,it''s.example.com'"
        );
        assert_eq!(command, expected);
        assert!(
            !command.contains("\\'"),
            "PowerShell has no backslash escape, so none may be printed: {command}"
        );
    }

    /// The Windows form is one double-quoted argument per value: the only
    /// quoting `cmd.exe` and PowerShell agree on, which both pass to the native
    /// binary untouched for a value that holds nothing either shell expands.
    /// An ordinary host list and an ordinary profile path, spaces included,
    /// render this way, and the line carries no marker.
    #[test]
    fn the_windows_form_is_one_double_quoted_argument_for_both_windows_shells() {
        let hosts = ["api.example.com", "*.cdn.example.com"].map(String::from);
        let dir = std::path::Path::new(r"C:\Users\op erator\.zeroclaw");
        let command =
            super::egress_set_command_for(super::ShellDialect::Windows, dir, "zpi1_k", &hosts);
        assert_eq!(
            command,
            concat!(
                r#"zeroclaw --config-dir "C:\Users\op erator\.zeroclaw" "#,
                r#"config set plugins.entries.zpi1_k.egress_hosts "api.example.com,*.cdn.example.com""#
            )
        );
        assert!(command.starts_with(&super::zeroclaw_invocation_for(
            super::ShellDialect::Windows,
            dir
        )));
        assert!(!command.starts_with(super::POWERSHELL_ONLY_MARKER));
    }

    /// A value that either Windows shell would alter inside double quotes has
    /// no form both shells pass literally, so the Windows line is refused: it
    /// becomes the PowerShell form behind a `#` marker, which `cmd.exe` cannot
    /// run and PowerShell reads as a comment. The refusal is per line, not per
    /// value: one awkward host marks the whole command, and an awkward profile
    /// path does the same even when every host is plain. An embedded single
    /// quote is not awkward: both shells treat it as an ordinary character
    /// inside double quotes.
    #[test]
    fn the_windows_form_refuses_a_value_either_windows_shell_would_expand() {
        let plain_dir = std::path::Path::new(r"C:\Users\operator\.zeroclaw");
        for host in [
            "$(id).example.com",
            "`id`.example.com",
            "$env:username.example.com",
            "%USERNAME%.example.com",
            "it!s.example.com",
        ] {
            assert!(!super::windows_form_is_literal(host), "{host}");
            let hosts = ["api.example.com".to_string(), host.to_string()];
            let command = super::egress_set_command_for(
                super::ShellDialect::Windows,
                plain_dir,
                "zpi1_k",
                &hosts,
            );
            let rest = command
                .strip_prefix(super::POWERSHELL_ONLY_MARKER)
                .unwrap_or_else(|| panic!("the line for {host} must be marked: {command}"));
            assert_eq!(
                rest,
                super::egress_set_command_for(
                    super::ShellDialect::PowerShell,
                    plain_dir,
                    "zpi1_k",
                    &hosts
                ),
                "after the marker comes the PowerShell form exactly"
            );
        }
        for dir in [
            r"C:\Users\op erator\.zeroclaw\",
            r"C:\%USERPROFILE%\.zeroclaw",
        ] {
            assert!(!super::windows_form_is_literal(dir), "{dir}");
            let command = super::egress_set_command_for(
                super::ShellDialect::Windows,
                std::path::Path::new(dir),
                "zpi1_k",
                &["api.example.com".to_string()],
            );
            assert!(
                command.starts_with(super::POWERSHELL_ONLY_MARKER),
                "a profile path no Windows shell passes literally marks the line: {command}"
            );
        }
        // A single quote is an ordinary character inside double quotes in both
        // Windows shells, so an embedded quote needs no refusal here.
        for plain in [
            "api.example.com",
            "*.cdn.example.com",
            "it's.example.com",
            r"C:\Users\op erator\.zeroclaw",
        ] {
            assert!(super::windows_form_is_literal(plain), "{plain}");
        }
    }

    /// The Windows shells, driven for real. `zeroclaw` is swapped for a native
    /// argv echo: `powershell -File echo.ps1`, whose arguments arrive through
    /// the same C-runtime command-line parsing a native `zeroclaw.exe` would
    /// see, so what the script prints is what `config set` would receive.
    #[cfg(windows)]
    mod windows_shells {
        use std::io::Write;
        use std::os::windows::process::CommandExt;

        struct ArgvEcho {
            _dir: tempfile::TempDir,
            invocation: String,
        }

        fn argv_echo() -> ArgvEcho {
            let dir = tempfile::tempdir().expect("temp dir");
            let script = dir.path().join("echo.ps1");
            std::fs::write(
                &script,
                "foreach ($a in $args) { [Console]::Out.WriteLine($a) }\r\n",
            )
            .expect("write echo.ps1");
            let path = script.to_string_lossy().into_owned();
            assert!(
                super::super::windows_form_is_literal(&path),
                "the temp path must be plain for both shells: {path}"
            );
            ArgvEcho {
                _dir: dir,
                invocation: format!(
                    "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{path}\""
                ),
            }
        }

        /// The printed line with `zeroclaw` replaced by the argv echo.
        fn line_with_echo(echo: &ArgvEcho, command: &str) -> String {
            command
                .strip_prefix("zeroclaw ")
                .map(|rest| format!("{} {rest}", echo.invocation))
                .expect("the command starts with the binary name")
        }

        /// Run `line` as if pasted into `cmd.exe`: `/C` takes the rest of the
        /// command line verbatim, so nothing is re-quoted on the way in.
        fn cmd_exe(line: &str) -> std::process::Output {
            std::process::Command::new("cmd.exe")
                .arg("/C")
                .raw_arg(line)
                .output()
                .expect("cmd.exe must be available")
        }

        /// Run `line` as if pasted into PowerShell: `-Command -` reads the
        /// command text from stdin, unmodified.
        fn powershell(line: &str) -> std::process::Output {
            let mut child = std::process::Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    "-",
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("powershell.exe must be available");
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(format!("{line}\r\n").as_bytes())
                .expect("write the pasted line");
            child.wait_with_output().expect("powershell output")
        }

        fn argv(output: &std::process::Output) -> Vec<String> {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn expected(dir: &std::path::Path, hosts: &[String]) -> Vec<String> {
            vec![
                "--config-dir".to_string(),
                dir.to_string_lossy().into_owned(),
                "config".to_string(),
                "set".to_string(),
                super::super::egress_hosts_path("zpi1_k"),
                hosts.join(","),
            ]
        }

        /// The plain Windows line, pasted into `cmd.exe`: the directory with a
        /// space and the host list with a `*` suffix pattern each arrive as one
        /// argument.
        #[test]
        fn the_windows_form_survives_cmd_exe_as_the_intended_arguments() {
            let echo = argv_echo();
            let hosts = ["api.example.com", "*.cdn.example.com"].map(String::from);
            let dir = std::path::Path::new(r"C:\Users\op erator\.zeroclaw");
            let command = super::super::egress_set_command(dir, "zpi1_k", &hosts);
            assert!(!command.starts_with(super::super::POWERSHELL_ONLY_MARKER));
            let output = cmd_exe(&line_with_echo(&echo, &command));
            assert_eq!(argv(&output), expected(dir, &hosts), "{command}");
        }

        /// The same plain line, pasted into PowerShell, where the arguments
        /// additionally pass through PowerShell's native-command re-quoting.
        #[test]
        fn the_windows_form_survives_powershell_as_the_intended_arguments() {
            let echo = argv_echo();
            let hosts = ["api.example.com", "*.cdn.example.com"].map(String::from);
            let dir = std::path::Path::new(r"C:\Users\op erator\.zeroclaw");
            let command = super::super::egress_set_command(dir, "zpi1_k", &hosts);
            let output = powershell(&line_with_echo(&echo, &command));
            assert_eq!(argv(&output), expected(dir, &hosts), "{command}");
        }

        /// A refused line, pasted whole, runs nothing in either shell, and the
        /// PowerShell form after the marker delivers a publisher-controlled
        /// `$(id)` host to the native binary byte for byte.
        #[test]
        fn a_refused_windows_line_runs_nothing_and_its_powershell_form_is_literal() {
            let echo = argv_echo();
            let hosts = [
                "$(id).example.com",
                "`id`.example.com",
                "$env:username.example.com",
                "*.cdn.example.com",
            ]
            .map(String::from);
            let dir = std::path::Path::new(r"C:\Users\op erator\.zeroclaw");
            let command = super::super::egress_set_command(dir, "zpi1_k", &hosts);
            let rest = command
                .strip_prefix(super::super::POWERSHELL_ONLY_MARKER)
                .expect("a $(id) host marks the line");
            let pasted_whole = line_with_echo(&echo, rest);
            let pasted_whole = format!("{}{pasted_whole}", super::super::POWERSHELL_ONLY_MARKER);

            let in_cmd = cmd_exe(&pasted_whole);
            assert!(!in_cmd.status.success(), "cmd.exe cannot run a # line");
            assert!(
                in_cmd.stdout.is_empty(),
                "nothing may execute: {}",
                String::from_utf8_lossy(&in_cmd.stdout)
            );

            let in_powershell = powershell(&pasted_whole);
            assert!(in_powershell.status.success());
            assert!(
                in_powershell.stdout.is_empty(),
                "a # line is a comment: {}",
                String::from_utf8_lossy(&in_powershell.stdout)
            );

            let output = powershell(&line_with_echo(&echo, rest));
            assert_eq!(argv(&output), expected(dir, &hosts), "{rest}");
        }
    }

    /// The printed command is pasted into the operator's shell, so the proof
    /// has to be the shell's own argument parsing, not a substring check: a
    /// POSIX `sh` tokenises the command with `zeroclaw` swapped for `printf`,
    /// and every declared host, including ones that carry command-substitution
    /// syntax, a glob and a quote, must arrive as one literal argument.
    #[cfg(unix)]
    #[test]
    fn the_grant_value_survives_the_operator_shell_as_one_literal_argument() {
        let hosts = [
            "$(id).example.com",
            "`id`.example.com",
            "$HOME.example.com",
            "*.cdn.example.com",
            "it's.example.com",
        ]
        .map(String::from);
        let dir = std::path::Path::new("/srv/it's here/profile a");
        let command = super::egress_set_command(dir, "zpi1_k", &hosts);
        let script = command
            .strip_prefix("zeroclaw ")
            .map(|rest| format!("printf '%s\\n' {rest}"))
            .expect("the command starts with the binary name");
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .env_remove("HOME")
            .output()
            .expect("sh must be available");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let argv: Vec<String> = String::from_utf8(output.stdout)
            .expect("utf-8 argv")
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            argv,
            vec![
                "--config-dir".to_string(),
                dir.to_string_lossy().into_owned(),
                "config".to_string(),
                "set".to_string(),
                super::egress_hosts_path("zpi1_k"),
                hosts.join(","),
            ],
            "every byte of the host list must reach the argument untouched: {command}"
        );
    }
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
        let cmd = egress_set_command(dir(), key, &v(&["api.example.com", "*.cdn.example.com"]));
        // The value is quoted for the host shell: POSIX single quotes on Unix,
        // the double-quoted form both Windows shells share on Windows.
        let quoted = ShellDialect::host().quote_literal("api.example.com,*.cdn.example.com");
        assert_eq!(
            cmd,
            format!(
                "{} config set plugins.entries.{key}.egress_hosts {quoted}",
                zeroclaw_invocation_for(ShellDialect::host(), dir())
            )
        );
        assert!(
            !cmd.contains("plugins.entries.weather-tool."),
            "the command must not address a package-name-keyed row: {cmd}"
        );
        assert!(
            cmd.ends_with(&quoted),
            "the list must be one quoted argument, so the `*` of a suffix \
             pattern is never glob-expanded by the operator's shell: {cmd}"
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

    /// With no row, the repair is a `config patch` that creates it; `config
    /// set` would fail with an unknown property. On POSIX the JSON reaches the
    /// command's input intact through the shell (the end-to-end half is
    /// `the_absent_row_repair_creates_the_row_and_its_grant` in `main.rs`); on
    /// Windows the JSON's quotes send the whole line to the PowerShell form.
    #[test]
    fn an_instance_without_a_row_is_repaired_by_a_command_that_creates_it() {
        let key = "zpi1_WyJ3ZWF0aGVyLXRvb2wiLCJ0b29sIiwid2VhdGhlci10b29sIl0";
        let absent = EgressGrantState::Enforced {
            granted: Vec::new(),
            allow_private: Vec::new(),
            row_exists: false,
        };
        let EgressGapPlan::Grant {
            command, missing, ..
        } = plan_egress_gap(dir(), key, &v(&["api.example.com"]), &absent, &rt())
        else {
            panic!("an ungranted declaration must plan a grant");
        };
        assert_eq!(missing, v(&["api.example.com"]));
        assert_eq!(
            command,
            super::egress_create_command(dir(), key, &v(&["api.example.com"]))
        );
        assert!(!command.contains(" config set "), "{command}");

        let posix = super::egress_create_command_for(
            super::ShellDialect::Posix,
            dir(),
            key,
            &v(&["api.example.com"]),
        );
        assert!(posix.starts_with("printf '%s\\n' '[{"), "{posix}");
        assert!(
            posix.ends_with("| zeroclaw --config-dir '/srv/zeroclaw/profile-a' config patch -"),
            "{posix}"
        );
        let windows = super::egress_create_command_for(
            super::ShellDialect::Windows,
            dir(),
            key,
            &v(&["api.example.com"]),
        );
        assert!(
            windows.starts_with(super::POWERSHELL_ONLY_MARKER),
            "{windows}"
        );

        // The same declaration on an existing row keeps the ordinary command.
        let EgressGapPlan::Grant { command, .. } =
            plan_egress_gap(dir(), key, &v(&["api.example.com"]), &enforced(&[]), &rt())
        else {
            panic!("an ungranted declaration must plan a grant");
        };
        assert!(command.contains(" config set "), "{command}");
    }

    fn enforced(granted: &[&str]) -> EgressGrantState {
        EgressGrantState::Enforced {
            granted: v(granted),
            allow_private: Vec::new(),
            row_exists: true,
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
        // would not help — but marked as having no row, because the command
        // that grants it has to create the row first.
        assert_eq!(
            resolve_grant_state(key, &legacy, &[], lookup),
            EgressGrantState::Enforced {
                granted: Vec::new(),
                allow_private: Vec::new(),
                row_exists: false,
            }
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
            plan_egress_gap(dir(), key, &v(&["api.example.com"]), &state, &rt()),
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
                dir(),
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
            plan_egress_gap(dir(), key, &[], &state, &rt()),
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
            dir(),
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
                dir(),
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
                dir(),
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
                dir(),
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
                command: egress_set_command(
                    dir(),
                    key,
                    &v(&["api.example.com", "api2.example.com"])
                ),
            }
        );
        // No row at all reads as enforced-empty: every declared host is a gap.
        assert!(matches!(
            plan_egress_gap(dir(), key, &v(&["api.example.com"]), &enforced(&[]), &rt()),
            EgressGapPlan::Grant { .. }
        ));
        // No row and nothing declared: an empty grant is a row the runtime
        // accepts (it means no reach), so there is nothing to report.
        assert_eq!(
            plan_egress_gap(dir(), key, &[], &enforced(&[]), &rt()),
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
                dir(),
                key,
                &v(&["api.example.com"]),
                &enforced(&["api.example.com"]),
                &bad_nat64
            ),
            EgressGapPlan::Nothing
        );
        assert!(matches!(
            plan_egress_gap(
                dir(),
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
                dir(),
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
        let plan = plan_egress_gap(dir(), key, &v(&["api.com"]), &stranded(&["*.com"]), &rt());
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
        assert_eq!(command, egress_set_command(dir(), key, &v(&["api.com"])));
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
            dir(),
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
        assert_eq!(
            command,
            egress_set_command(dir(), key, &v(&["api.example.com"]))
        );
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
        let plan = plan_egress_gap(dir(), key, &v(&["api.example.com"]), &state, &rt());
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
            dir(),
            key,
            &v(&["api.example.com"]),
            &EgressGrantState::Enforced {
                granted: v(&["api.example.com"]),
                allow_private: v(&["other.example.com"]),
                row_exists: true,
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
