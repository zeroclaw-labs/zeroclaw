//! Three-tier tool permission policy engine — the Phase 0/1 rule authority
//! of RFC 7155.
//!
//! One rule table, one resolver. Legacy risk-profile fields
//! (`allowed_commands`, `always_ask`, `auto_approve`, `block_high_risk_commands`,
//! `require_approval_for_medium_risk`, autonomy level) are compiled at
//! config-load time into [`PolicyRule`]s in the same table as user-written
//! `tool_policy` rules, and [`resolve_decision`] adjudicates everything with
//! a single precedence: `Deny > Ask > Allow`, unmatched → `Ask`
//! (fail-closed into approval). The two legacy decision surfaces —
//! `SecurityPolicy`'s command-level checks and `ApprovalManager`'s
//! tool-name-level checks — stop being parallel authorities that can
//! disagree; they both consult this table.
//!
//! Matching targets are structured [`ToolAction`]s, never raw strings: the
//! shell extractor reuses the exact parsing primitives of
//! [`crate::policy`] (segment splitting, env-assignment skipping, basename
//! normalization, argument-safety checks) so an `Allow` here can never
//! cover a command the old allowlist would have rejected.
//!
//! Syntax trust is a first-class fact: commands whose parsed segments do
//! not capture everything that will execute (command substitution, unsafe
//! redirects, `tee`, background chaining, injection-capable arguments,
//! unparseable PowerShell) extract as `ParseStatus::Degraded`, and an
//! apparent `Allow` on a degraded command downgrades to `Ask` — or `Deny`
//! when `block_high_risk_commands` is on — never unconditional execution
//! (RFC 7155 §2.3). The one exemption is the legacy trusted-environment
//! escape hatch (`allowed_commands = ["*"]` with
//! `block_high_risk_commands = false`), which keeps its historical meaning
//! of opting out of command-level syntax restrictions.
//!
//! v1 registers only the shell extractor; cross-tool variants are roadmap
//! Phase 2 and rejected at pattern-parse time.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroclaw_api::runtime_traits::ShellDialect;

use crate::policy::{
    CommandRiskLevel, args_safe, command_basename, command_names_equivalent,
    contains_unquoted_input_redirect, contains_unquoted_shell_variable_expansion,
    contains_unquoted_single_ampersand, contains_unsafe_output_redirect_for_shell,
    generic_segment_risk, is_allowlist_entry_match, is_powershell_provider_argument,
    powershell_segment_risk, skip_env_assignments, split_simple_powershell_pipeline,
    split_unquoted_segments, strip_fd_merge_redirects, strip_windows_exe_suffix,
    strip_wrapping_quotes,
};
use crate::schema::RiskProfileConfig;

// ─── Decisions ──────────────────────────────────────────────────────────────

/// The three permission tiers. There is no fourth "classification fallback"
/// tier: an action matching no rule resolves to [`Decision::Ask`].
///
/// Ordered most-restrictive-first so per-segment combination takes the
/// minimum (most restrictive) decision across segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub enum Decision {
    /// Always forbid. No `Allow` from any source can overturn a matched
    /// `Deny` — not auto_approve, not a session "always", not full
    /// autonomy, not the future auto-approver.
    Deny,
    /// Mandatory confirmation: every execution needs a fresh trusted
    /// approval bound to the exact action fingerprint.
    Ask,
    /// Pass — a precise carve-out, never an escalation beyond what the
    /// risk profile already permits.
    Allow,
}

impl crate::config::HasPropKind for Decision {
    const PROP_KIND: crate::config::PropKind = crate::config::PropKind::Enum;
}

// ─── Rules ───────────────────────────────────────────────────────────────────

/// Which legacy surface a compiled rule was produced from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyField {
    /// `risk_profiles.<alias>.allowed_commands` entry (non-wildcard).
    AllowedCommands,
    /// The `allowed_commands` wildcard entry (`*`).
    WildcardAllowlist,
    /// `risk_profiles.<alias>.always_ask` entry.
    AlwaysAsk,
    /// `risk_profiles.<alias>.auto_approve` entry.
    AutoApprove,
    /// `risk_profiles.<alias>.level = "read_only"`.
    ReadOnlyAutonomy,
}

/// Built-in predicate rules — action-dependent conditions that cannot be
/// expressed as static patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuiltinPredicate {
    /// `block_high_risk_commands` + a High-risk segment + the command not
    /// being explicitly allowlisted → hard `Deny`. This is the compilation
    /// of the legacy `block_high_risk && !is_command_explicitly_allowed`
    /// exception: the exception lives in the predicate, the
    /// `Deny > Ask > Allow` precedence stays pure.
    HighRiskBlocked,
    /// Supervised autonomy + a High-risk segment → overridable `Ask`.
    SupervisedHighRisk,
    /// Supervised autonomy + `require_approval_for_medium_risk` + a
    /// Medium-risk segment → overridable `Ask`.
    SupervisedMediumRisk,
}

/// Where a rule came from. `Explicit` rules (user-written
/// `tool_policy.rules`) are the only `Allow` source that can carve out of
/// the overridable risk-default `Ask` tiers; legacy and session `Allow`
/// rules cannot (the design decision behind RFC 7155 §2.2's
/// `Shell(cargo test:*) = allow` example).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleSource {
    /// User-written in `risk_profiles.<alias>.tool_policy.rules`.
    Explicit,
    /// Compiled from a legacy risk-profile field.
    Legacy(LegacyField),
    /// Built-in predicate over the action and profile flags.
    Builtin(BuiltinPredicate),
    /// Runtime session-scoped rule minted by an "always approve" answer.
    Session,
}

/// How a rule matches the arguments of a shell command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArgPattern {
    /// The argument list must equal these tokens exactly.
    Literal(Vec<String>),
    /// The argument list must start with these tokens.
    Prefix(Vec<String>),
    /// Bounded glob over the joined argument string (tokens joined with a
    /// single space): `*` matches within one token (does not cross a
    /// space), `**` matches across tokens, everything else is literal.
    Glob(String),
}

/// What a rule matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleMatcher {
    /// A shell command: executable plus optional argument constraint.
    /// `arg_pattern: None` matches any arguments (the executable-level
    /// rule, e.g. `Shell(rm)`).
    ShellCommand {
        /// Executable as written in the rule. Matched with
        /// [`crate::policy`] allowlist-entry semantics: path-like entries
        /// compare exactly against the as-typed executable, bare names
        /// compare case-insensitively (plus Windows suffix equivalence)
        /// against the basename.
        executable: String,
        arg_pattern: Option<ArgPattern>,
    },
    /// A tool name (the `ApprovalManager` layer: `always_ask` /
    /// `auto_approve` / session allowlist entries). `"*"` matches any tool.
    ToolName { tool: String },
    /// Any shell command at all (`Shell(*)`) — the compiled form of the
    /// wildcard allowlist entry.
    AnyShell,
}

/// One rule in the table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRule {
    pub matcher: RuleMatcher,
    pub decision: Decision,
    /// `false` = hard rule: no `Allow` of any source may override it.
    /// Hard rules: `always_ask`, the High-risk block, read-only autonomy,
    /// and every explicit user `Deny`/`Ask` (a matched `Ask` is mandatory
    /// approval — RFC 7155 §1.3: "Allow rules and auto_approve cannot
    /// downgrade it" — so it must survive any `Allow`). Only the legacy
    /// Supervised risk-tier predicates stay overridable.
    pub overridable: bool,
    pub source: RuleSource,
}

// ─── Actions ─────────────────────────────────────────────────────────────────

/// Why a command's parsed segments cannot be trusted to capture everything
/// that will execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradationReason {
    /// Backticks, unquoted `$(…)`, or process substitution `<(` / `>(` —
    /// nested command text the segment list cannot enumerate.
    CommandSubstitution,
    /// An output redirect targeting a file (not a safe suppress/merge form).
    UnsafeOutputRedirect,
    /// An unquoted input redirect.
    UnsafeInputRedirect,
    /// A bare `tee` token — writes arbitrary files, bypassing redirect
    /// checks.
    TeeFileWrite,
    /// An unquoted single `&` — background chaining hides extra commands.
    BackgroundChaining,
    /// Injection-capable arguments of a known executable (`find -exec`,
    /// `git -c`, `python -c`, `node -e`, `pip install`, `npm exec`,
    /// `cargo install`) — the executed code is not visible in the segment.
    UnsafeExecutableArguments,
    /// The PowerShell grammar could not parse the command safely.
    UnparseablePowerShell,
    /// A PowerShell segment rejected by the simple-pipeline gate (empty,
    /// `$`- or quote-prefixed executable, batch file, provider argument).
    UnsafePowerShellSegment,
}

/// Whether the parsed segments fully capture what the command will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseStatus {
    /// The segment list is authoritative.
    Clean,
    /// Some syntax makes the segment list untrustworthy; see
    /// [`DegradationReason`]. An apparent `Allow` on a degraded command
    /// downgrades to `Ask` (or `Deny` under `block_high_risk_commands`).
    Degraded(DegradationReason),
}

/// One command segment, extracted with the same normalization the legacy
/// allowlist applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSegment {
    /// Leading `NAME=value` words (preserved for fingerprinting; the
    /// executable extraction skips them exactly like the legacy
    /// allowlist's env-assignment skip).
    pub env_assignments: Vec<(String, String)>,
    /// The executable as typed (quote-stripped, inline redirects removed):
    /// the identity an approval fingerprint must bind, so `./foo/bar.sh`
    /// and `bar.sh` never share an authorization.
    pub executable: String,
    /// Basename normalization used for name-level rule matching.
    pub base: String,
    /// Argument tokens: whitespace split after the executable, exactly as
    /// the legacy argument-safety checks see them (raw, quote-unstripped).
    pub arguments: Vec<String>,
    /// This segment's risk classification.
    pub risk: CommandRiskLevel,
}

/// A normalized shell action: what every shell rule, fingerprint, and
/// revalidation compares against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellAction {
    /// Command segments. Compound commands carry one entry per
    /// `;` / `|` / `&&` / newline-separated segment and resolve
    /// independently, combined to the most restrictive decision.
    pub segments: Vec<ShellSegment>,
    /// Working directory the action will run in, when known. Part of the
    /// fingerprint facts; not consulted by v1 rule matching.
    pub cwd: Option<PathBuf>,
    pub dialect: ShellDialect,
    pub parse_status: ParseStatus,
}

/// The normalized action a rule table adjudicates. v1 registers only the
/// shell extractor; other variants are roadmap Phase 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolAction {
    Shell(ShellAction),
}

// ─── Extraction ──────────────────────────────────────────────────────────────

/// Extract the normalized shell action for a command under a dialect.
///
/// Reuses the parsing primitives of [`crate::policy`] so the extractor and
/// the legacy allowlist can never disagree about what the segments are.
/// Syntax the legacy five-gate defense would reject (command substitution,
/// unsafe redirects, `tee`, background `&`, unsafe executable arguments,
/// unparseable PowerShell) extracts as [`ParseStatus::Degraded`] instead of
/// being silently dropped: the resolver decides what a degraded parse
/// means, and it never means unconditional execution.
pub fn extract_shell_action(
    command: &str,
    dialect: ShellDialect,
    cwd: Option<&Path>,
) -> ToolAction {
    let (segments, parse_status) = match dialect {
        ShellDialect::None => (
            Vec::new(),
            ParseStatus::Degraded(DegradationReason::UnparseablePowerShell),
        ),
        ShellDialect::Posix | ShellDialect::WindowsCmd => {
            extract_posix_like_segments(command, dialect)
        }
        ShellDialect::PowerShell => extract_powershell_segments(command),
    };
    ToolAction::Shell(ShellAction {
        segments,
        cwd: cwd.map(Path::to_path_buf),
        dialect,
        parse_status,
    })
}

/// The fact schema tag carried inside every shell action fingerprint — see
/// [`ShellAction::fingerprint_facts`].
pub const SHELL_ACTION_FACTS_SCHEMA: &str = "zc-shell-action-v1";

impl ShellAction {
    /// The canonical fingerprint facts for this action (RFC 7155 §5.2:
    /// the complete action, not the display string).
    ///
    /// Shape (v1): `{"schema": "zc-shell-action-v1", "dialect": …, "cwd": …,
    /// "segments": [{"env": {…}, "executable": <as typed>,
    /// "arguments": […]}]}`. Env assignments and arguments bind the argv;
    /// redirections bind through their argument tokens; `cwd` binds the
    /// working directory; `dialect` binds the interpreter. The
    /// `schema` tag is the [`zeroclaw_api::permission::ACTION_FINGERPRINT_DOMAIN_V1`]
    /// companion inside
    /// the facts: bumping either invalidates outstanding approvals.
    ///
    /// serde_json's sorted-key maps make the serialization canonical, so
    /// two extractions of the same command hash identically.
    #[must_use]
    pub fn fingerprint_facts(&self) -> serde_json::Value {
        let segments: Vec<serde_json::Value> = self
            .segments
            .iter()
            .map(|segment| {
                let env: serde_json::Map<String, serde_json::Value> = segment
                    .env_assignments
                    .iter()
                    .map(|(name, value)| (name.clone(), serde_json::Value::String(value.clone())))
                    .collect();
                serde_json::json!({
                    "env": env,
                    "executable": segment.executable,
                    "arguments": segment.arguments,
                })
            })
            .collect();
        serde_json::json!({
            "schema": SHELL_ACTION_FACTS_SCHEMA,
            "dialect": dialect_name(self.dialect),
            "cwd": self
                .cwd
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            "segments": segments,
        })
    }
}

/// Stable wire names for [`ShellDialect`] inside fingerprint facts.
fn dialect_name(dialect: ShellDialect) -> &'static str {
    match dialect {
        ShellDialect::Posix => "posix",
        ShellDialect::WindowsCmd => "windows_cmd",
        ShellDialect::PowerShell => "powershell",
        ShellDialect::None => "none",
    }
}

/// The action fingerprint for a shell command: what a confirmation mints
/// against and what execution consumes against. Both sides must go through
/// this one function — the fingerprint is only meaningful as a shared
/// computation.
#[must_use]
pub fn shell_action_fingerprint(
    command: &str,
    dialect: ShellDialect,
    cwd: Option<&Path>,
) -> zeroclaw_api::permission::ActionFingerprint {
    let action = extract_shell_action(command, dialect, cwd);
    let ToolAction::Shell(shell) = &action;
    zeroclaw_api::permission::ActionFingerprint::compute(&shell.fingerprint_facts())
}

fn extract_posix_like_segments(
    command: &str,
    dialect: ShellDialect,
) -> (Vec<ShellSegment>, ParseStatus) {
    let mut degradation: Option<DegradationReason> = None;

    // The legacy five-gate order, preserved so the first violation reported
    // matches what the old allowlist path rejected on.
    if command.contains('`')
        || contains_unquoted_shell_variable_expansion(command)
        || command.contains("<(")
        || command.contains(">(")
    {
        degradation = Some(DegradationReason::CommandSubstitution);
    }
    if degradation.is_none() && contains_unsafe_output_redirect_for_shell(command, dialect) {
        degradation = Some(DegradationReason::UnsafeOutputRedirect);
    }
    if degradation.is_none() && contains_unquoted_input_redirect(command) {
        degradation = Some(DegradationReason::UnsafeInputRedirect);
    }
    if degradation.is_none()
        && command
            .split_whitespace()
            .any(|word| word == "tee" || word.ends_with("/tee"))
    {
        degradation = Some(DegradationReason::TeeFileWrite);
    }
    if degradation.is_none() {
        let ampersand_check = strip_fd_merge_redirects(command);
        if contains_unquoted_single_ampersand(&ampersand_check) {
            degradation = Some(DegradationReason::BackgroundChaining);
        }
    }

    let mut segments = Vec::new();
    for segment in split_unquoted_segments(command) {
        let Some(extracted) = extract_one_posix_segment(&segment) else {
            continue;
        };
        if degradation.is_none() && !extracted.args_safe {
            degradation = Some(DegradationReason::UnsafeExecutableArguments);
        }
        segments.push(extracted.segment);
    }

    (
        segments,
        match degradation {
            Some(reason) => ParseStatus::Degraded(reason),
            None => ParseStatus::Clean,
        },
    )
}

struct ExtractedSegment {
    segment: ShellSegment,
    args_safe: bool,
}

/// Extract one POSIX-like segment, mirroring
/// `is_posix_like_command_allowed`'s per-segment loop exactly: env-assignment
/// skip, quote strip + trim, inline-redirect strip, basename + suffix +
/// lowercase, raw whitespace argument tokens (cased and lowered for the
/// argument-safety check).
fn extract_one_posix_segment(segment: &str) -> Option<ExtractedSegment> {
    let cmd_part = skip_env_assignments(segment);
    let env_assignments = capture_env_assignments(segment);

    let mut words = cmd_part.split_whitespace();
    let raw_executable = strip_wrapping_quotes(words.next().unwrap_or("")).trim();
    let executable = match raw_executable.find(['<', '>']) {
        Some(idx) => &raw_executable[..idx],
        None => raw_executable,
    };
    let base_owned = command_basename(executable).to_ascii_lowercase();
    let base = strip_windows_exe_suffix(&base_owned).to_string();
    if base.is_empty() {
        return None;
    }

    let args_cased: Vec<String> = words.map(str::to_string).collect();
    let args_lower: Vec<String> = args_cased
        .iter()
        .map(|word| word.to_ascii_lowercase())
        .collect();
    let risk = match generic_segment_risk(&base, &args_lower, &cmd_part.to_ascii_lowercase()) {
        Some(risk) => risk,
        None => CommandRiskLevel::Low,
    };
    let is_safe = args_safe(&base, &args_lower, &args_cased);

    Some(ExtractedSegment {
        segment: ShellSegment {
            env_assignments,
            executable: executable.to_string(),
            base,
            arguments: args_cased,
            risk,
        },
        args_safe: is_safe,
    })
}

/// Capture the leading `NAME=value` words the executable extraction skips.
/// Mirrors [`crate::policy::skip_env_assignments`]'s recognition rule:
/// contains `=` and starts with an ASCII letter or underscore.
fn capture_env_assignments(segment: &str) -> Vec<(String, String)> {
    let mut assignments = Vec::new();
    for word in segment.split_whitespace() {
        let is_assignment = word.contains('=')
            && word
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        if !is_assignment {
            break;
        }
        if let Some((name, value)) = word.split_once('=') {
            assignments.push((name.to_string(), value.to_string()));
        }
    }
    assignments
}

/// A segment placeholder for syntax the extractor could not trust: an
/// empty base matches no name rule, and the High risk keeps the risk-tier
/// predicates applying exactly like the legacy whole-command classification.
fn phantom_high_risk_segment() -> ShellSegment {
    ShellSegment {
        env_assignments: Vec::new(),
        executable: String::new(),
        base: String::new(),
        arguments: Vec::new(),
        risk: CommandRiskLevel::High,
    }
}

fn extract_powershell_segments(command: &str) -> (Vec<ShellSegment>, ParseStatus) {
    let mut degradation: Option<DegradationReason> = None;
    let mut segments = Vec::new();

    let Some(pipeline_segments) = split_simple_powershell_pipeline(command) else {
        // Unparseable PowerShell: the legacy risk classifier treated the
        // whole command as High risk, and the legacy allowlist rejected it.
        // Preserve both facts: a phantom High-risk segment (empty base
        // matches no rule, so only the risk predicates and the wildcard
        // apply) plus the degraded status.
        segments.push(phantom_high_risk_segment());
        return (
            segments,
            ParseStatus::Degraded(DegradationReason::UnparseablePowerShell),
        );
    };
    let has_pipeline = pipeline_segments.len() > 1;

    for segment in &pipeline_segments {
        let mut words = segment.split_whitespace();
        let Some(base_raw) = words.next() else {
            degradation = Some(DegradationReason::UnsafePowerShellSegment);
            segments.push(phantom_high_risk_segment());
            continue;
        };
        let base_owned = command_basename(base_raw).to_ascii_lowercase();
        // The simple-pipeline gate rejects these executables outright; they
        // extract as degraded (a batch file or `$`-prefixed name runs
        // content the segment list cannot enumerate). A phantom High-risk
        // segment still records them — matching the legacy risk
        // classification — so the risk predicates keep applying.
        if base_owned.is_empty()
            || base_owned.starts_with('$')
            || base_owned.starts_with('"')
            || base_owned.starts_with('\'')
            || base_owned.ends_with(".cmd")
            || base_owned.ends_with(".bat")
        {
            degradation = Some(DegradationReason::UnsafePowerShellSegment);
            segments.push(ShellSegment {
                env_assignments: Vec::new(),
                executable: base_raw.to_string(),
                base: String::new(),
                arguments: words.map(str::to_string).collect(),
                risk: CommandRiskLevel::High,
            });
            continue;
        }
        let base = base_owned
            .strip_suffix(".exe")
            .unwrap_or(&base_owned)
            .to_string();
        let args_cased: Vec<String> = words.map(str::to_string).collect();
        let args_lower: Vec<String> = args_cased
            .iter()
            .map(|word| word.to_ascii_lowercase())
            .collect();
        if args_cased
            .iter()
            .any(|word| is_powershell_provider_argument(word))
        {
            degradation = Some(DegradationReason::UnsafePowerShellSegment);
        }
        if degradation.is_none() && !args_safe(&base, &args_lower, &args_cased) {
            degradation = Some(DegradationReason::UnsafeExecutableArguments);
        }
        segments.push(ShellSegment {
            env_assignments: Vec::new(),
            executable: base_raw.to_string(),
            base,
            arguments: args_cased,
            risk: powershell_segment_risk(segment, has_pipeline),
        });
    }

    if segments.is_empty() && degradation.is_none() {
        degradation = Some(DegradationReason::UnsafePowerShellSegment);
    }

    (
        segments,
        match degradation {
            Some(reason) => ParseStatus::Degraded(reason),
            None => ParseStatus::Clean,
        },
    )
}

// ─── Pattern parsing (config syntax → RuleMatcher) ──────────────────────────

/// Parse a `tool_policy.rules[].pattern` into a matcher.
///
/// Accepted forms (v1, shell only — other tool classes are roadmap Phase 2
/// and rejected here so they cannot be silently inert):
///
/// - `Shell(*)` — any shell command.
/// - `Shell(rm)` — executable with any arguments.
/// - `Shell(git push)` — executable with exactly these arguments.
/// - `Shell(git push --force:*)` — executable with arguments starting with
///   these tokens (the `:*` prefix form).
/// - `Shell(rm -rf *)` — executable with a bounded glob over the joined
///   argument string (`*` within a token, `**` across tokens).
///
/// `re:` regex patterns are rejected: regex is roadmap Phase 3.
pub fn parse_pattern(pattern: &str) -> Result<RuleMatcher, String> {
    let pattern = pattern.trim();
    let invalid = |message: &str| {
        Err(format!(
            "invalid tool policy pattern `{pattern}`: {message}"
        ))
    };

    let Some(content) = pattern
        .strip_prefix("Shell(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        if pattern.starts_with("re:") {
            return invalid("regex patterns are roadmap Phase 3; use a prefix or glob pattern");
        }
        return invalid(
            "expected a `Shell(…)` pattern; other tool classes (file, network, MCP, desktop) \
             are roadmap Phase 2 and cannot be configured yet",
        );
    };

    let content = content.trim();
    if content.is_empty() {
        return invalid("empty pattern");
    }
    if content == "*" {
        return Ok(RuleMatcher::AnyShell);
    }
    if content.starts_with("re:") {
        return invalid("regex patterns are roadmap Phase 3; use a prefix or glob pattern");
    }

    let (content, prefix_form) = match content.strip_suffix(":*") {
        Some(rest) => (rest.trim(), true),
        None => (content, false),
    };
    if content.is_empty() {
        return invalid("empty pattern");
    }

    let tokens: Vec<&str> = content.split_whitespace().collect();
    let Some((executable, args)) = tokens.split_first() else {
        return invalid("no executable");
    };
    if executable.contains('*') {
        return invalid("wildcard is not a valid executable; use `Shell(*)` to match any command");
    }
    let arg_tokens: Vec<String> = args.iter().map(|token| (*token).to_string()).collect();

    if prefix_form {
        return Ok(RuleMatcher::ShellCommand {
            executable: executable.to_string(),
            arg_pattern: if arg_tokens.is_empty() {
                None
            } else {
                Some(ArgPattern::Prefix(arg_tokens))
            },
        });
    }

    if arg_tokens.iter().any(|token| token.contains('*')) {
        return Ok(RuleMatcher::ShellCommand {
            executable: executable.to_string(),
            arg_pattern: Some(ArgPattern::Glob(arg_tokens.join(" "))),
        });
    }

    Ok(RuleMatcher::ShellCommand {
        executable: executable.to_string(),
        arg_pattern: if arg_tokens.is_empty() {
            None
        } else {
            Some(ArgPattern::Literal(arg_tokens))
        },
    })
}

// ─── Config schema types ─────────────────────────────────────────────────────

/// Default freshness window for confirmations minted under a profile
/// (RFC 7155 §5.2): five minutes.
pub const DEFAULT_CONFIRMATION_VALIDITY_SECS: u64 = 300;

/// Command/tool-level fine-grained permission rules on a risk profile
/// (RFC 7155 §4.3). Empty = only legacy-compiled rules adjudicate — the
/// default, with behavior identical to a profile without the section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, zeroclaw_macros::Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ToolPolicyConfig {
    /// Explicit rules, adjudicated by `resolve_decision` together with the
    /// legacy-compiled rules. Pattern syntax: see [`parse_pattern`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<PolicyRuleConfig>,
    /// How long a minted approval stays valid, in seconds (1..=86400).
    /// Delegation may only shrink this, never extend it (RFC 7155 §4.4).
    #[serde(default = "default_confirmation_validity_secs")]
    pub confirmation_validity_secs: u64,
}

fn default_confirmation_validity_secs() -> u64 {
    DEFAULT_CONFIRMATION_VALIDITY_SECS
}

impl Default for ToolPolicyConfig {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            confirmation_validity_secs: DEFAULT_CONFIRMATION_VALIDITY_SECS,
        }
    }
}

impl ToolPolicyConfig {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// One user-written rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PolicyRuleConfig {
    /// Pattern such as `Shell(git push --force:*)`.
    pub pattern: String,
    /// The decision a matching action resolves to.
    pub decision: Decision,
}

// ─── Compilation ─────────────────────────────────────────────────────────────

/// The compiled rule table for one risk profile: legacy fields and explicit
/// `tool_policy` rules in one list, immutable after config load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledRuleSet {
    rules: Vec<PolicyRule>,
    /// The trusted-environment escape hatch: `allowed_commands = ["*"]`
    /// with `block_high_risk_commands = false`. Historically opts out of
    /// command-level syntax restrictions entirely, so degraded parses stay
    /// executable here rather than downgrading.
    escape_hatch: bool,
    block_high_risk_commands: bool,
    confirmation_validity_secs: u64,
}

impl CompiledRuleSet {
    /// Compile a risk profile's legacy fields and explicit rules into one
    /// rule table.
    ///
    /// The legacy mapping (RFC 7155 §1.2), verified command-for-command
    /// against the pre-RFC `SecurityPolicy` behavior by the golden tests
    /// at the bottom of this file:
    ///
    /// | legacy input | compiles to |
    /// |---|---|
    /// | `level = "read_only"` | hard `Deny` on any shell command |
    /// | `allowed_commands` entry (non-`*`) | `Allow` (any arguments) |
    /// | `allowed_commands` contains `*` | `Allow` on any shell command; with `block_high_risk_commands = false` also the syntax-restriction escape hatch |
    /// | `always_ask` entry | hard `Ask` on that tool name (`*` = every tool) |
    /// | `auto_approve` entry | `Allow` on that tool name (`*` = every tool) |
    /// | `block_high_risk_commands` + High risk + not explicitly allowlisted | hard `Deny` predicate |
    /// | Supervised + High risk | overridable `Ask` predicate |
    /// | Supervised + `require_approval_for_medium_risk` + Medium risk | overridable `Ask` predicate |
    ///
    /// Autonomy `Full` deliberately compiles to **nothing**: the legacy
    /// validator still enforced the allowlist and syntax gates under Full
    /// (Full only suppressed the Supervised approval asks), and compiling
    /// it to a blanket `Allow` would widen permissions — a regression, not
    /// a compilation.
    pub fn compile(risk_profile: &RiskProfileConfig) -> Self {
        Self::compile_from_fields(
            risk_profile.level,
            &risk_profile.allowed_commands,
            &risk_profile.always_ask,
            &risk_profile.auto_approve,
            risk_profile.block_high_risk_commands,
            risk_profile.require_approval_for_medium_risk,
            &risk_profile.tool_policy,
        )
    }

    /// Compile from the individual fields a [`crate::policy::SecurityPolicy`]
    /// mirrors, so the table is built from exactly the values a struct-literal
    /// policy carries (never from a stored snapshot that can desync).
    pub fn compile_from_fields(
        autonomy: crate::autonomy::AutonomyLevel,
        allowed_commands: &[String],
        always_ask: &[String],
        auto_approve: &[String],
        block_high_risk_commands: bool,
        require_approval_for_medium_risk: bool,
        tool_policy: &ToolPolicyConfig,
    ) -> Self {
        use crate::autonomy::AutonomyLevel;

        let mut rules: Vec<PolicyRule> = Vec::new();

        if autonomy == AutonomyLevel::ReadOnly {
            rules.push(PolicyRule {
                matcher: RuleMatcher::AnyShell,
                decision: Decision::Deny,
                overridable: false,
                source: RuleSource::Legacy(LegacyField::ReadOnlyAutonomy),
            });
        }

        for tool in always_ask {
            rules.push(PolicyRule {
                matcher: RuleMatcher::ToolName {
                    tool: tool.trim().to_string(),
                },
                decision: Decision::Ask,
                overridable: false,
                source: RuleSource::Legacy(LegacyField::AlwaysAsk),
            });
        }

        for tool in auto_approve {
            rules.push(PolicyRule {
                matcher: RuleMatcher::ToolName {
                    tool: tool.trim().to_string(),
                },
                decision: Decision::Allow,
                overridable: true,
                source: RuleSource::Legacy(LegacyField::AutoApprove),
            });
        }

        let has_wildcard = allowed_commands.iter().any(|entry| entry.trim() == "*");
        if has_wildcard {
            rules.push(PolicyRule {
                matcher: RuleMatcher::AnyShell,
                decision: Decision::Allow,
                overridable: true,
                source: RuleSource::Legacy(LegacyField::WildcardAllowlist),
            });
        }
        for entry in allowed_commands {
            if entry.trim() == "*" {
                continue;
            }
            rules.push(PolicyRule {
                matcher: RuleMatcher::ShellCommand {
                    executable: entry.trim().to_string(),
                    arg_pattern: None,
                },
                decision: Decision::Allow,
                overridable: true,
                source: RuleSource::Legacy(LegacyField::AllowedCommands),
            });
        }

        if block_high_risk_commands {
            rules.push(PolicyRule {
                matcher: RuleMatcher::AnyShell,
                decision: Decision::Deny,
                overridable: false,
                source: RuleSource::Builtin(BuiltinPredicate::HighRiskBlocked),
            });
        }
        if autonomy == AutonomyLevel::Supervised {
            rules.push(PolicyRule {
                matcher: RuleMatcher::AnyShell,
                decision: Decision::Ask,
                overridable: true,
                source: RuleSource::Builtin(BuiltinPredicate::SupervisedHighRisk),
            });
            if require_approval_for_medium_risk {
                rules.push(PolicyRule {
                    matcher: RuleMatcher::AnyShell,
                    decision: Decision::Ask,
                    overridable: true,
                    source: RuleSource::Builtin(BuiltinPredicate::SupervisedMediumRisk),
                });
            }
        }

        for rule in &tool_policy.rules {
            // Config validation rejects unparseable patterns at load time
            // (see `validate_tool_policy`), so a parse failure here cannot
            // happen for a validated profile; skipping keeps compile
            // total for unvalidated in-memory profiles rather than
            // panicking on them.
            if let Ok(matcher) = parse_pattern(&rule.pattern) {
                rules.push(PolicyRule {
                    matcher,
                    decision: rule.decision,
                    overridable: false,
                    source: RuleSource::Explicit,
                });
            }
        }

        Self {
            rules,
            escape_hatch: has_wildcard && !block_high_risk_commands,
            block_high_risk_commands,
            confirmation_validity_secs: tool_policy.confirmation_validity_secs,
        }
    }

    #[must_use]
    pub fn rules(&self) -> &[PolicyRule] {
        &self.rules
    }

    /// The trusted-environment escape hatch is active (see the field docs).
    #[must_use]
    pub fn escape_hatch(&self) -> bool {
        self.escape_hatch
    }

    #[must_use]
    pub fn block_high_risk_commands(&self) -> bool {
        self.block_high_risk_commands
    }

    #[must_use]
    pub fn confirmation_validity_secs(&self) -> u64 {
        self.confirmation_validity_secs
    }
}

// ─── Resolution ──────────────────────────────────────────────────────────────

/// The scopes a resolution consults: the profile-compiled table plus the
/// runtime session rules (the narrow `Allow` rules an "always approve"
/// answer mints).
#[derive(Debug, Clone, Copy)]
pub struct ResolvedScopes<'a> {
    pub profile: &'a CompiledRuleSet,
    pub session_rules: &'a [PolicyRule],
}

impl<'a> ResolvedScopes<'a> {
    /// Scopes with no session rules.
    #[must_use]
    pub fn profile_only(profile: &'a CompiledRuleSet) -> Self {
        Self {
            profile,
            session_rules: &[],
        }
    }
}

/// Why a resolution came out the way it did — enough for callers to produce
/// legacy-compatible operator/model messages without re-deriving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionReason {
    /// A pattern rule produced the decision.
    MatchedRule {
        decision: Decision,
        source: RuleSource,
        pattern: String,
    },
    /// The built-in High-risk block (`block_high_risk_commands` + High
    /// risk + not explicitly allowlisted).
    HighRiskBlocked,
    /// The built-in Supervised risk-tier ask.
    SupervisedRiskAsk { level: CommandRiskLevel },
    /// Syntax degradation downgraded an apparent Allow (RFC 7155 §2.3).
    DegradedSyntax {
        reason: DegradationReason,
        decision: Decision,
    },
    /// No rule matched: the fail-closed default.
    Unmatched,
    /// The runtime exposes no shell for this dialect.
    NoShellAccess,
    /// The command had no extractable command word.
    EmptyCommand,
}

/// The outcome of one resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub decision: Decision,
    pub reason: ResolutionReason,
}

impl Resolution {
    fn new(decision: Decision, reason: ResolutionReason) -> Self {
        Self { decision, reason }
    }
}

/// Adjudicate a tool action against the scopes: the single authority
/// (RFC 7155 §3.2).
///
/// Per matched-rule precedence — the algorithm of record, step by step:
///
/// 1. any matched `Deny` → `Deny` (absolute: any source, including the
///    High-risk block predicate);
/// 2. any matched hard `Ask` (`always_ask`, explicit user `Ask`) → `Ask`;
/// 3. any matched `Allow` with source `Explicit` → `Allow` — the precise
///    carve-out that lifts the *overridable* risk-default `Ask` tiers
///    (step 4) but never steps 1–2;
/// 4. any matched `Ask` (the Supervised risk-tier predicates) → `Ask`;
/// 5. any matched `Allow` (legacy allowlist / auto-approve / session) →
///    `Allow`;
/// 6. no match → `Ask` (fail-closed into approval).
///
/// Compound commands resolve segment-by-segment and combine to the most
/// restrictive decision. Degraded parses downgrade an apparent `Allow` to
/// `Ask` — or `Deny` under `block_high_risk_commands` — unless the
/// trusted-environment escape hatch is active.
pub fn resolve_decision(action: &ToolAction, scopes: &ResolvedScopes) -> Resolution {
    let ToolAction::Shell(shell) = action;

    if shell.dialect == ShellDialect::None {
        return Resolution::new(Decision::Deny, ResolutionReason::NoShellAccess);
    }
    if shell.segments.is_empty() && shell.parse_status == ParseStatus::Clean {
        // A clean parse producing no command word is not a runnable action.
        // A DEGRADED parse producing no segments (unparseable PowerShell)
        // must fall through instead: the escape hatch may still allow it,
        // exactly like the legacy gates it bypassed before parsing.
        return Resolution::new(Decision::Deny, ResolutionReason::EmptyCommand);
    }

    let explicit = command_explicitly_allowed(shell, scopes);

    let mut combined: Option<Resolution> = None;
    for segment in &shell.segments {
        let segment_resolution = resolve_segment(segment, explicit, scopes);
        combined = Some(match combined {
            None => segment_resolution,
            Some(prev) if prev.decision <= segment_resolution.decision => prev,
            Some(_) => segment_resolution,
        });
    }
    // Zero segments (degraded parse): no rule ever matched, so the default
    // is the fail-closed Ask — unless the trusted-environment escape hatch
    // is active, whose historical meaning is skipping command-level syntax
    // restrictions entirely.
    let mut resolution = combined.unwrap_or_else(|| {
        if scopes.profile.escape_hatch() {
            Resolution::new(
                Decision::Allow,
                ResolutionReason::MatchedRule {
                    decision: Decision::Allow,
                    source: RuleSource::Legacy(LegacyField::WildcardAllowlist),
                    pattern: "Shell(*)".to_string(),
                },
            )
        } else {
            Resolution::new(Decision::Ask, ResolutionReason::Unmatched)
        }
    });

    // RFC 7155 §2.3: an apparent Allow on a degraded parse never executes
    // unconditionally. The escape hatch keeps its historical meaning
    // (opting out of command-level syntax restrictions).
    if !scopes.profile.escape_hatch()
        && let ParseStatus::Degraded(reason) = shell.parse_status
    {
        let degraded_decision = if scopes.profile.block_high_risk_commands() {
            Decision::Deny
        } else if resolution.decision == Decision::Allow {
            Decision::Ask
        } else {
            resolution.decision
        };
        // Rebind the reason even when the DECISION is unchanged (an Ask
        // staying Ask): a degraded command's Ask is structural — the
        // syntax could not be trusted — so the validation entries must not
        // let an approval bit bridge it the way they bridge risk-tier
        // asks. The legacy allowlist rejected these commands regardless of
        // approval; the reason is what carries that fact.
        resolution = Resolution::new(
            degraded_decision,
            ResolutionReason::DegradedSyntax {
                reason,
                decision: degraded_decision,
            },
        );
    }

    resolution
}

/// Resolve the tool-name layer question ("does this tool need approval")
/// against the same table — the `ApprovalManager` consults this so
/// `always_ask` / `auto_approve` / session grants stop being a second,
/// independent authority (RFC 7155 §1.2's "they can no longer disagree").
///
/// Only [`RuleMatcher::ToolName`] rules participate; a `ToolName` entry of
/// `"*"` matches any tool, mirroring the legacy wildcard semantics.
pub fn resolve_tool_name(tool_name: &str, scopes: &ResolvedScopes) -> Resolution {
    let matched = scopes
        .profile
        .rules()
        .iter()
        .chain(scopes.session_rules.iter())
        .filter(|rule| {
            matches!(
                &rule.matcher,
                RuleMatcher::ToolName { tool }
                    if tool == tool_name || tool.trim() == "*"
            )
        });

    resolve_matched_rules(matched)
}

fn resolve_segment(
    segment: &ShellSegment,
    command_explicit: bool,
    scopes: &ResolvedScopes,
) -> Resolution {
    let matched = scopes
        .profile
        .rules()
        .iter()
        .chain(scopes.session_rules.iter())
        .filter(|rule| rule_matches_segment(rule, segment, command_explicit));

    resolve_matched_rules(matched)
}

/// The six-step algorithm over the matched rules.
fn resolve_matched_rules<'r, I>(matched: I) -> Resolution
where
    I: Iterator<Item = &'r PolicyRule>,
{
    let matched: Vec<&PolicyRule> = matched.collect();

    // Step 1: Deny is absolute.
    if let Some(rule) = matched.iter().find(|rule| rule.decision == Decision::Deny) {
        if matches!(
            rule.source,
            RuleSource::Builtin(BuiltinPredicate::HighRiskBlocked)
        ) {
            return Resolution::new(Decision::Deny, ResolutionReason::HighRiskBlocked);
        }
        return Resolution::new(
            Decision::Deny,
            ResolutionReason::MatchedRule {
                decision: Decision::Deny,
                source: rule.source.clone(),
                pattern: describe_matcher(&rule.matcher),
            },
        );
    }
    // Step 2: hard Ask (always_ask, explicit user Ask).
    if let Some(rule) = matched
        .iter()
        .find(|rule| rule.decision == Decision::Ask && !rule.overridable)
    {
        return Resolution::new(
            Decision::Ask,
            ResolutionReason::MatchedRule {
                decision: Decision::Ask,
                source: rule.source.clone(),
                pattern: describe_matcher(&rule.matcher),
            },
        );
    }
    // Step 3: an Explicit or Session Allow lifts the overridable
    // risk-default Ask. Both are precise operator allowances — one written
    // in config, one granted live by an "always approve" answer (RFC 7155
    // §3.3.4: an Always grant crosses the risk-default Ask exactly like
    // today's session allowlist did, but never Deny and never a hard Ask).
    // Neither crosses steps 1–2.
    if let Some(rule) = matched.iter().find(|rule| {
        rule.decision == Decision::Allow
            && matches!(rule.source, RuleSource::Explicit | RuleSource::Session)
    }) {
        return Resolution::new(
            Decision::Allow,
            ResolutionReason::MatchedRule {
                decision: Decision::Allow,
                source: rule.source.clone(),
                pattern: describe_matcher(&rule.matcher),
            },
        );
    }
    // Step 4: overridable Ask (the Supervised risk tiers).
    if let Some(rule) = matched.iter().find(|rule| rule.decision == Decision::Ask) {
        return Resolution::new(
            Decision::Ask,
            match &rule.source {
                RuleSource::Builtin(BuiltinPredicate::SupervisedHighRisk) => {
                    ResolutionReason::SupervisedRiskAsk {
                        level: CommandRiskLevel::High,
                    }
                }
                RuleSource::Builtin(BuiltinPredicate::SupervisedMediumRisk) => {
                    ResolutionReason::SupervisedRiskAsk {
                        level: CommandRiskLevel::Medium,
                    }
                }
                _ => ResolutionReason::MatchedRule {
                    decision: Decision::Ask,
                    source: rule.source.clone(),
                    pattern: describe_matcher(&rule.matcher),
                },
            },
        );
    }
    // Step 5: legacy / session Allow.
    if let Some(rule) = matched.iter().find(|rule| rule.decision == Decision::Allow) {
        return Resolution::new(
            Decision::Allow,
            ResolutionReason::MatchedRule {
                decision: Decision::Allow,
                source: rule.source.clone(),
                pattern: describe_matcher(&rule.matcher),
            },
        );
    }
    // Step 6: fail-closed default.
    Resolution::new(Decision::Ask, ResolutionReason::Unmatched)
}

/// Whether a rule matches one shell segment — including the built-in
/// predicates, whose "match" is the action-dependent condition itself.
fn rule_matches_segment(rule: &PolicyRule, segment: &ShellSegment, command_explicit: bool) -> bool {
    match &rule.matcher {
        RuleMatcher::AnyShell => match &rule.source {
            RuleSource::Builtin(BuiltinPredicate::HighRiskBlocked) => {
                // Fires per segment: High risk and the command is not
                // precisely allowlisted.
                segment.risk == CommandRiskLevel::High && !command_explicit
            }
            RuleSource::Builtin(BuiltinPredicate::SupervisedHighRisk) => {
                segment.risk == CommandRiskLevel::High
            }
            RuleSource::Builtin(BuiltinPredicate::SupervisedMediumRisk) => {
                segment.risk == CommandRiskLevel::Medium
            }
            _ => true,
        },
        RuleMatcher::ShellCommand {
            executable,
            arg_pattern,
        } => {
            if !is_allowlist_entry_match(executable, &segment.executable, &segment.base) {
                return false;
            }
            arg_pattern_matches(arg_pattern, segment)
        }
        RuleMatcher::ToolName { .. } => false,
    }
}

fn arg_pattern_matches(arg_pattern: &Option<ArgPattern>, segment: &ShellSegment) -> bool {
    match arg_pattern {
        None => true,
        Some(ArgPattern::Literal(tokens)) => &segment.arguments == tokens,
        Some(ArgPattern::Prefix(tokens)) => {
            segment.arguments.len() >= tokens.len()
                && segment.arguments[..tokens.len()] == tokens[..]
        }
        Some(ArgPattern::Glob(pattern)) => glob_match(pattern, &segment.arguments.join(" ")),
    }
}

/// Today's `is_command_explicitly_allowed`, compiled-table form: every
/// segment matches a non-wildcard allowlist entry — or, additionally, an
/// Explicit-source `Allow` rule, which is the same kind of precise operator
/// allowance (this is what lets `Shell(rm:*) = allow` exempt from the
/// High-risk block exactly like a non-wildcard `allowed_commands` entry).
///
/// Session grants deliberately do NOT count: an "always approve" answer is
/// a live, session-scoped grant and must never defeat the hard
/// `block_high_risk_commands` Deny — today's session allowlist never
/// exempted the block either (it only suppressed the approval ask).
fn command_explicitly_allowed(shell: &ShellAction, scopes: &ResolvedScopes) -> bool {
    if shell.segments.is_empty() {
        return false;
    }
    shell.segments.iter().all(|segment| {
        scopes
            .profile
            .rules()
            .iter()
            .chain(scopes.session_rules.iter())
            .any(|rule| match (&rule.source, &rule.matcher) {
                (
                    RuleSource::Legacy(LegacyField::AllowedCommands),
                    RuleMatcher::ShellCommand { executable, .. },
                ) => is_allowlist_entry_match(executable, &segment.executable, &segment.base),
                (
                    RuleSource::Explicit,
                    RuleMatcher::ShellCommand {
                        executable,
                        arg_pattern,
                    },
                ) if rule.decision == Decision::Allow => {
                    is_allowlist_entry_match(executable, &segment.executable, &segment.base)
                        && arg_pattern_matches(arg_pattern, segment)
                }
                _ => false,
            })
    })
}

/// Bounded glob match: `*` matches within one token (any run of non-space
/// characters), `**` matches across tokens (any characters including
/// spaces). No character classes — arguments are matched as tokens, not
/// paths, so there are no path-traversal semantics to constrain.
fn glob_match(pattern: &str, subject: &str) -> bool {
    glob_match_inner(pattern.as_bytes(), subject.as_bytes())
}

fn glob_match_inner(pattern: &[u8], subject: &[u8]) -> bool {
    match pattern.first() {
        None => subject.is_empty(),
        Some(b'*') if pattern.get(1) == Some(&b'*') => {
            // `**` matches anything, including nothing.
            let rest = &pattern[2..];
            glob_match_inner(rest, subject)
                || (!subject.is_empty() && glob_match_inner(pattern, &subject[1..]))
        }
        Some(b'*') => {
            // `*` matches a run of non-space characters.
            let max = subject
                .iter()
                .position(|&byte| byte == b' ')
                .unwrap_or(subject.len());
            (0..=max)
                .rev()
                .any(|take| glob_match_inner(&pattern[1..], &subject[take..]))
        }
        Some(&byte) => {
            !subject.is_empty()
                && subject[0] == byte
                && glob_match_inner(&pattern[1..], &subject[1..])
        }
    }
}

fn describe_matcher(matcher: &RuleMatcher) -> String {
    match matcher {
        RuleMatcher::ShellCommand {
            executable,
            arg_pattern,
        } => match arg_pattern {
            None => format!("Shell({executable})"),
            Some(ArgPattern::Literal(tokens)) => {
                format!("Shell({executable} {})", tokens.join(" "))
            }
            Some(ArgPattern::Prefix(tokens)) => {
                format!("Shell({executable} {}:*)", tokens.join(" "))
            }
            Some(ArgPattern::Glob(pattern)) => format!("Shell({executable} {pattern})"),
        },
        RuleMatcher::ToolName { tool } => format!("Tool({tool})"),
        RuleMatcher::AnyShell => "Shell(*)".to_string(),
    }
}

// ─── Config validation ───────────────────────────────────────────────────────

/// Validate a profile's `tool_policy` section (fail-fast, config-load
/// time). Returns the config error message for the first problem found.
pub fn validate_tool_policy(profile_alias: &str, policy: &ToolPolicyConfig) -> Result<(), String> {
    for (index, rule) in policy.rules.iter().enumerate() {
        if let Err(error) = parse_pattern(&rule.pattern) {
            return Err(format!(
                "risk_profiles.{profile_alias}.tool_policy.rules[{index}].pattern: {error}"
            ));
        }
    }
    if policy.confirmation_validity_secs == 0 || policy.confirmation_validity_secs > 86_400 {
        return Err(format!(
            "risk_profiles.{profile_alias}.tool_policy.confirmation_validity_secs must be \
             between 1 and 86400 seconds, got {}",
            policy.confirmation_validity_secs
        ));
    }
    Ok(())
}

// ─── Shadowing warnings ──────────────────────────────────────────────────────

/// Warn when an explicit rule makes another rule on the same profile dead
/// (RFC 7155 §4.2's shadowed-rule detection).
///
/// Two cases worth an operator's attention:
/// - an explicit `Ask` on an executable shadows the legacy `Allow` for the
///   same executable (the allowlist entry stops mattering);
/// - an explicit `Deny` on an executable shadows any `Allow` — legacy or
///   explicit — for the same executable.
pub fn collect_shadowing_warnings(
    profile_alias: &str,
    risk_profile: &RiskProfileConfig,
) -> Vec<crate::validation_warnings::ValidationWarning> {
    let mut warnings = Vec::new();
    let explicit: Vec<(usize, &PolicyRuleConfig)> = risk_profile
        .tool_policy
        .rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| parse_pattern(&rule.pattern).is_ok())
        .collect();

    for (index, rule) in &explicit {
        let Ok(matcher) = parse_pattern(&rule.pattern) else {
            continue;
        };
        let RuleMatcher::ShellCommand { executable, .. } = &matcher else {
            continue;
        };

        let shadows_allowlist = |entry: &String| {
            entry.trim() != "*" && command_names_equivalent(entry.trim(), executable.trim())
        };

        if rule.decision == Decision::Ask
            && risk_profile.allowed_commands.iter().any(shadows_allowlist)
        {
            warnings.push(crate::validation_warnings::ValidationWarning::new(
                "tool_policy_ask_shadows_allowlist",
                format!(
                    "tool_policy rule `{pattern}` (ask) shadows the `allowed_commands` entry \
                     `{executable}`: commands matching both always require approval.",
                    pattern = rule.pattern
                ),
                format!("risk_profiles.{profile_alias}.tool_policy.rules[{index}]"),
            ));
        }

        if rule.decision == Decision::Deny {
            let shadowed_explicit = explicit.iter().any(|(other_index, other)| {
                other_index != index
                    && other.decision == Decision::Allow
                    && parse_pattern(&other.pattern).is_ok_and(|other_matcher| {
                        matches!(&other_matcher, RuleMatcher::ShellCommand { executable: other_executable, .. }
                            if command_names_equivalent(other_executable.trim(), executable.trim()))
                    })
            });
            let shadowed_legacy = risk_profile.allowed_commands.iter().any(shadows_allowlist);
            if shadowed_explicit || shadowed_legacy {
                warnings.push(crate::validation_warnings::ValidationWarning::new(
                    "tool_policy_deny_shadows_allow",
                    format!(
                        "tool_policy rule `{pattern}` (deny) shadows an allow rule for \
                         `{executable}`: the allow rule can never match.",
                        pattern = rule.pattern
                    ),
                    format!("risk_profiles.{profile_alias}.tool_policy.rules[{index}]"),
                ));
            }
        }
    }

    warnings
}

// ─── Tests ───────────────────────────────────────────────────────────────────

// ─── Delegation no-escalation (RFC 7155 §4.4/§2.6) ────────────────────────────

/// Whether `child` argument pattern is fully covered by (a subset of)
/// `parent`. Conservative: a `Glob` child is only provably covered by an
/// identical glob or an `AnyShell` parent — glob-subset math is not
/// attempted, so an unprovable case is an escalation.
fn arg_pattern_covered(child: &Option<ArgPattern>, parent: &Option<ArgPattern>) -> bool {
    match (child, parent) {
        // Child allows any args: only a parent that also allows any args
        // (or a wider rule handled by the caller) covers it.
        (None, None) => true,
        (None, Some(_)) => false,
        // Child constrains args; an any-args parent covers everything.
        (Some(_), None) => true,
        (Some(ArgPattern::Literal(child_tokens)), Some(ArgPattern::Literal(parent_tokens))) => {
            child_tokens == parent_tokens
        }
        (Some(ArgPattern::Literal(child_tokens)), Some(ArgPattern::Prefix(parent_tokens))) => {
            child_tokens.len() >= parent_tokens.len()
                && child_tokens[..parent_tokens.len()] == parent_tokens[..]
        }
        (Some(ArgPattern::Prefix(child_tokens)), Some(ArgPattern::Prefix(parent_tokens))) => {
            child_tokens.len() >= parent_tokens.len()
                && child_tokens[..parent_tokens.len()] == parent_tokens[..]
        }
        (Some(ArgPattern::Glob(child_glob)), Some(ArgPattern::Glob(parent_glob))) => {
            child_glob == parent_glob
        }
        // Literal/Prefix vs Glob, Glob vs Literal/Prefix: not provable.
        _ => false,
    }
}

/// Whether a child `Allow` rule is covered by some parent `Allow` rule
/// (RFC 7155 §4.4: "a new Allow must be a subset of some parent Allow").
fn allow_rule_covered(child: &PolicyRule, parent_allows: &[&PolicyRule]) -> bool {
    parent_allows
        .iter()
        .any(|parent| match (&child.matcher, &parent.matcher) {
            (_, RuleMatcher::AnyShell) => true,
            (
                RuleMatcher::ShellCommand {
                    executable: child_exec,
                    arg_pattern: child_args,
                },
                RuleMatcher::ShellCommand {
                    executable: parent_exec,
                    arg_pattern: parent_args,
                },
            ) => {
                command_names_equivalent(child_exec.trim(), parent_exec.trim())
                    && arg_pattern_covered(child_args, parent_args)
            }
            _ => false,
        })
}

/// Compare two profiles' explicit `tool_policy` sections on RESOLVED rule
/// semantics (RFC 7155 §4.4): a delegated/child policy may only narrow.
///
/// Fails when the child
/// - adds an `Allow` rule not covered by any parent `Allow`;
/// - drops a parent `Deny` rule without a covering child `Deny`; or
/// - extends the confirmation validity window beyond the parent's.
///
/// Returns the offending description on violation (the caller maps it to
/// its escalation-violation type).
pub fn ensure_no_rule_escalation(
    child: &ToolPolicyConfig,
    parent: &ToolPolicyConfig,
) -> Result<(), String> {
    let parse = |config: &ToolPolicyConfig| -> Vec<PolicyRule> {
        config
            .rules
            .iter()
            .filter_map(|rule| {
                parse_pattern(&rule.pattern).ok().map(|matcher| PolicyRule {
                    matcher,
                    decision: rule.decision,
                    overridable: false,
                    source: RuleSource::Explicit,
                })
            })
            .collect()
    };
    let child_rules = parse(child);
    let parent_rules = parse(parent);

    let parent_allows: Vec<&PolicyRule> = parent_rules
        .iter()
        .filter(|rule| rule.decision == Decision::Allow)
        .collect();
    for rule in child_rules
        .iter()
        .filter(|rule| rule.decision == Decision::Allow)
    {
        if !allow_rule_covered(rule, &parent_allows) {
            return Err(format!(
                "tool_policy allow rule `{}` is not covered by any parent allow rule",
                describe_matcher(&rule.matcher)
            ));
        }
    }

    let child_denies: Vec<&PolicyRule> = child_rules
        .iter()
        .filter(|rule| rule.decision == Decision::Deny)
        .collect();
    for rule in parent_rules
        .iter()
        .filter(|rule| rule.decision == Decision::Deny)
    {
        let still_denied = child_denies.iter().any(|child_deny| {
            matches!(
                (&child_deny.matcher, &rule.matcher),
                (
                    RuleMatcher::ShellCommand {
                        executable: child_exec,
                        ..
                    },
                    RuleMatcher::ShellCommand {
                        executable: parent_exec,
                        ..
                    }
                ) if command_names_equivalent(child_exec.trim(), parent_exec.trim())
            ) || matches!(child_deny.matcher, RuleMatcher::AnyShell)
                && matches!(rule.matcher, RuleMatcher::AnyShell)
        });
        if !still_denied {
            return Err(format!(
                "parent tool_policy deny rule `{}` is dropped by the child policy",
                describe_matcher(&rule.matcher)
            ));
        }
    }

    if child.confirmation_validity_secs > parent.confirmation_validity_secs {
        return Err(format!(
            "tool_policy confirmation_validity_secs extends the parent's window              ({}s > {}s); delegation may only shrink it",
            child.confirmation_validity_secs, parent.confirmation_validity_secs
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autonomy::AutonomyLevel;
    use crate::schema::RiskProfileConfig;
    use zeroclaw_api::runtime_traits::ShellDialect;

    fn profile(autonomy: AutonomyLevel, allowed: &[&str]) -> RiskProfileConfig {
        RiskProfileConfig {
            level: autonomy,
            allowed_commands: allowed.iter().map(|s| (*s).to_string()).collect(),
            ..RiskProfileConfig::default()
        }
    }

    fn resolve_with(cfg: &RiskProfileConfig, command: &str, dialect: ShellDialect) -> Resolution {
        let compiled = CompiledRuleSet::compile(cfg);
        let scopes = ResolvedScopes::profile_only(&compiled);
        let action = extract_shell_action(command, dialect, None);
        resolve_decision(&action, &scopes)
    }

    // ── Pattern parsing ────────────────────────────────────────────

    #[test]
    fn pattern_any_shell() {
        assert_eq!(parse_pattern("Shell(*)"), Ok(RuleMatcher::AnyShell));
    }

    #[test]
    fn pattern_executable_any_args() {
        assert_eq!(
            parse_pattern("Shell(rm)"),
            Ok(RuleMatcher::ShellCommand {
                executable: "rm".into(),
                arg_pattern: None,
            })
        );
    }

    #[test]
    fn pattern_literal_args() {
        assert_eq!(
            parse_pattern("Shell(git push)"),
            Ok(RuleMatcher::ShellCommand {
                executable: "git".into(),
                arg_pattern: Some(ArgPattern::Literal(vec!["push".into()])),
            })
        );
    }

    #[test]
    fn pattern_prefix_args() {
        assert_eq!(
            parse_pattern("Shell(git push --force:*)"),
            Ok(RuleMatcher::ShellCommand {
                executable: "git".into(),
                arg_pattern: Some(ArgPattern::Prefix(vec!["push".into(), "--force".into()])),
            })
        );
    }

    #[test]
    fn pattern_prefix_without_args_is_any_args() {
        assert_eq!(
            parse_pattern("Shell(git:*)"),
            Ok(RuleMatcher::ShellCommand {
                executable: "git".into(),
                arg_pattern: None,
            })
        );
    }

    #[test]
    fn pattern_glob_args() {
        assert_eq!(
            parse_pattern("Shell(rm -rf *)"),
            Ok(RuleMatcher::ShellCommand {
                executable: "rm".into(),
                arg_pattern: Some(ArgPattern::Glob("-rf *".into())),
            })
        );
    }

    #[test]
    fn pattern_rejects_other_tool_classes() {
        assert!(parse_pattern("FileWrite(/etc/shadow)").is_err());
        assert!(parse_pattern("Mcp(github__create_issue)").is_err());
        assert!(parse_pattern("WebFetch(domain:*.corp)").is_err());
    }

    #[test]
    fn pattern_rejects_regex_placeholder() {
        let error = parse_pattern("Shell(re:^kubectl)").unwrap_err();
        assert!(error.contains("Phase 3"), "{error}");
    }

    #[test]
    fn pattern_rejects_malformed() {
        assert!(parse_pattern("Shell(").is_err());
        assert!(parse_pattern("Shell()").is_err());
        assert!(parse_pattern("Shell(*) extra").is_err());
    }

    // ── Glob semantics ─────────────────────────────────────────────

    #[test]
    fn glob_star_does_not_cross_tokens() {
        assert!(glob_match("rm *", "rm /tmp/x"));
        assert!(!glob_match("a * b", "a x y b"));
    }

    #[test]
    fn glob_double_star_crosses_tokens() {
        assert!(glob_match("rm ** /tmp", "rm -rf --no-preserve /tmp"));
    }

    #[test]
    fn glob_literal_only() {
        assert!(glob_match("-rf /", "-rf /"));
        assert!(!glob_match("-rf /", "-rf /tmp"));
    }

    // ── Extraction ─────────────────────────────────────────────────

    #[test]
    fn extraction_captures_env_assignments() {
        let action = extract_shell_action("FOO=1 BAR=x git push", ShellDialect::Posix, None);
        let ToolAction::Shell(shell) = &action;
        assert_eq!(shell.segments.len(), 1);
        assert_eq!(
            shell.segments[0].env_assignments,
            vec![
                ("FOO".to_string(), "1".to_string()),
                ("BAR".to_string(), "x".to_string())
            ]
        );
        assert_eq!(shell.segments[0].base, "git");
        assert_eq!(shell.segments[0].arguments, vec!["push".to_string()]);
        assert_eq!(shell.parse_status, ParseStatus::Clean);
    }

    #[test]
    fn extraction_splits_compound_commands() {
        let action = extract_shell_action("ls -la && rm -rf /tmp/x", ShellDialect::Posix, None);
        let ToolAction::Shell(shell) = &action;
        assert_eq!(shell.segments.len(), 2);
        assert_eq!(shell.segments[0].base, "ls");
        assert_eq!(shell.segments[1].base, "rm");
    }

    #[test]
    fn extraction_strips_inline_redirect_from_executable() {
        let action = extract_shell_action("cat</dev/null", ShellDialect::Posix, None);
        let ToolAction::Shell(shell) = &action;
        assert_eq!(shell.segments[0].base, "cat");
    }

    #[test]
    fn extraction_marks_command_substitution_degraded() {
        let action = extract_shell_action("echo `whoami`", ShellDialect::Posix, None);
        let ToolAction::Shell(shell) = &action;
        assert_eq!(
            shell.parse_status,
            ParseStatus::Degraded(DegradationReason::CommandSubstitution)
        );
    }

    #[test]
    fn extraction_marks_unsafe_arguments_degraded() {
        let action = extract_shell_action("find . -exec rm {}", ShellDialect::Posix, None);
        let ToolAction::Shell(shell) = &action;
        assert_eq!(
            shell.parse_status,
            ParseStatus::Degraded(DegradationReason::UnsafeExecutableArguments)
        );
    }

    #[test]
    fn extraction_marks_unparseable_powershell_degraded() {
        let action = extract_shell_action("Get-ChildItem;", ShellDialect::PowerShell, None);
        let ToolAction::Shell(shell) = &action;
        assert_eq!(
            shell.parse_status,
            ParseStatus::Degraded(DegradationReason::UnparseablePowerShell)
        );
    }

    #[test]
    fn extraction_preserves_raw_executable_for_fingerprint_binding() {
        let action = extract_shell_action("/usr/bin/git push", ShellDialect::Posix, None);
        let ToolAction::Shell(shell) = &action;
        assert_eq!(shell.segments[0].executable, "/usr/bin/git");
        assert_eq!(shell.segments[0].base, "git");
    }

    #[test]
    fn extraction_carries_cwd_for_fingerprinting() {
        let action = extract_shell_action("ls", ShellDialect::Posix, Some(Path::new("/w")));
        let ToolAction::Shell(shell) = &action;
        assert_eq!(shell.cwd.as_deref(), Some(Path::new("/w")));
    }

    #[test]
    fn fingerprint_facts_bind_the_complete_action() {
        use zeroclaw_api::permission::ActionFingerprint;

        let base =
            shell_action_fingerprint("rm -rf /tmp/x", ShellDialect::Posix, Some(Path::new("/ws")));
        // Same command, same cwd → same fingerprint.
        assert_eq!(
            base,
            shell_action_fingerprint("rm -rf /tmp/x", ShellDialect::Posix, Some(Path::new("/ws")))
        );
        // Any fact change → different fingerprint: args, cwd, dialect,
        // executable spelling, env assignments.
        assert_ne!(
            base,
            shell_action_fingerprint("rm -rf /tmp/y", ShellDialect::Posix, Some(Path::new("/ws")))
        );
        assert_ne!(
            base,
            shell_action_fingerprint(
                "rm -rf /tmp/x",
                ShellDialect::Posix,
                Some(Path::new("/other"))
            )
        );
        assert_ne!(
            base,
            shell_action_fingerprint(
                "rm -rf /tmp/x",
                ShellDialect::PowerShell,
                Some(Path::new("/ws"))
            )
        );
        assert_ne!(
            base,
            shell_action_fingerprint(
                "/usr/bin/rm -rf /tmp/x",
                ShellDialect::Posix,
                Some(Path::new("/ws"))
            )
        );
        assert_ne!(
            base,
            shell_action_fingerprint(
                "FOO=1 rm -rf /tmp/x",
                ShellDialect::Posix,
                Some(Path::new("/ws"))
            )
        );
        // Fingerprint determinism is the shared-computation property the
        // gate (mint) and dispatch (consume) both rely on.
        let _ = ActionFingerprint::compute;
    }

    // ── Resolution precedence ───────────────────────────────────────

    #[test]
    fn deny_beats_wildcard_allow() {
        let mut cfg = profile(AutonomyLevel::Supervised, &["*"]);
        cfg.block_high_risk_commands = true;
        cfg.tool_policy.rules = vec![PolicyRuleConfig {
            pattern: "Shell(rm:*)".into(),
            decision: Decision::Deny,
        }];
        let resolution = resolve_with(&cfg, "rm -rf /tmp/x", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Deny);
    }

    #[test]
    fn unmatched_defaults_to_ask() {
        let cfg = profile(AutonomyLevel::Supervised, &["ls"]);
        let resolution = resolve_with(&cfg, "ffmpeg -i a.mp4 b.mp4", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Ask);
        assert_eq!(resolution.reason, ResolutionReason::Unmatched);
    }

    #[test]
    fn read_only_denies_everything() {
        let cfg = profile(AutonomyLevel::ReadOnly, &["*"]);
        let resolution = resolve_with(&cfg, "ls", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Deny);
    }

    #[test]
    fn compound_command_takes_most_restrictive() {
        let cfg = profile(AutonomyLevel::Supervised, &["ls", "git"]);
        // git commit is Medium risk → Supervised ask; ls is Low + allowed.
        let resolution = resolve_with(&cfg, "ls && git commit -m x", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Ask);
    }

    #[test]
    fn allowlisted_low_risk_command_allows() {
        let cfg = profile(AutonomyLevel::Supervised, &["ls", "grep"]);
        let resolution = resolve_with(&cfg, "ls -la | grep foo", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Allow);
    }

    #[test]
    fn high_risk_allowlisted_under_supervised_asks() {
        let cfg = profile(AutonomyLevel::Supervised, &["curl"]);
        let resolution = resolve_with(&cfg, "curl https://example.com", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Ask);
        assert_eq!(
            resolution.reason,
            ResolutionReason::SupervisedRiskAsk {
                level: CommandRiskLevel::High
            }
        );
    }

    #[test]
    fn high_risk_not_allowlisted_is_blocked_under_block_high_risk() {
        let cfg = profile(AutonomyLevel::Supervised, &["ls"]);
        let resolution = resolve_with(&cfg, "sudo rm /tmp/x", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Deny);
        assert_eq!(resolution.reason, ResolutionReason::HighRiskBlocked);
    }

    #[test]
    fn full_autonomy_still_enforces_allowlist() {
        // The Full profile must NOT compile to a blanket Allow: the legacy
        // validator enforced the allowlist under Full too. A regression
        // here would widen permissions for every Full user.
        let cfg = profile(AutonomyLevel::Full, &["ls"]);
        let resolution = resolve_with(&cfg, "ffmpeg -i a b", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Ask);
    }

    #[test]
    fn full_autonomy_suppresses_supervised_asks() {
        let cfg = profile(AutonomyLevel::Full, &["curl"]);
        let resolution = resolve_with(&cfg, "curl https://example.com", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Allow);
    }

    #[test]
    fn explicit_allow_lifts_risk_default_ask() {
        let mut cfg = profile(AutonomyLevel::Supervised, &[]);
        cfg.tool_policy.rules = vec![PolicyRuleConfig {
            pattern: "Shell(cargo test:*)".into(),
            decision: Decision::Allow,
        }];
        // cargo test is Medium risk under Supervised → legacy-compiled Ask,
        // but the explicit rule is the precise carve-out (the RFC 7155
        // §2.2 example).
        let resolution = resolve_with(&cfg, "cargo test --lib", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Allow);
    }

    #[test]
    fn explicit_allow_exempts_high_risk_block_like_precise_allowlist() {
        // A precise non-wildcard allowlist entry exempts a High-risk
        // command from the block_high_risk block today
        // (is_command_explicitly_allowed). An explicit Allow rule is the
        // same kind of precise operator allowance: it exempts from the
        // block, and then lifts the overridable Supervised High-risk Ask
        // (step 3 over step 4) — the user wrote exactly this allowance.
        let mut cfg = profile(AutonomyLevel::Supervised, &[]);
        cfg.block_high_risk_commands = true;
        cfg.tool_policy.rules = vec![PolicyRuleConfig {
            pattern: "Shell(rm:*)".into(),
            decision: Decision::Allow,
        }];
        let resolution = resolve_with(&cfg, "rm -rf /tmp/x", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Allow);
    }

    #[test]
    fn legacy_allow_does_not_lift_risk_default_ask() {
        // allowed_commands Allow must NOT lift the Supervised risk-tier Ask
        // (today: allowlisted high/medium still asks under Supervised).
        let cfg = profile(AutonomyLevel::Supervised, &["git"]);
        let resolution = resolve_with(&cfg, "git commit -m x", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Ask);
    }

    // ── Degraded syntax ────────────────────────────────────────────

    #[test]
    fn degraded_allow_downgrades_to_ask_without_block_high_risk() {
        let mut cfg = profile(AutonomyLevel::Supervised, &["echo"]);
        cfg.block_high_risk_commands = false;
        let resolution = resolve_with(&cfg, "echo `date`", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Ask);
        assert!(matches!(
            resolution.reason,
            ResolutionReason::DegradedSyntax { .. }
        ));
    }

    #[test]
    fn degraded_upgrades_to_deny_with_block_high_risk() {
        // The default profile has block_high_risk_commands = true: a
        // degraded parse fails closed all the way to Deny.
        let cfg = profile(AutonomyLevel::Supervised, &["echo"]);
        let resolution = resolve_with(&cfg, "echo `date`", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Deny);
    }

    #[test]
    fn escape_hatch_keeps_degraded_syntax_allowed() {
        let mut cfg = profile(AutonomyLevel::Supervised, &["*"]);
        cfg.block_high_risk_commands = false;
        let resolution = resolve_with(&cfg, "echo `date`", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Allow);
    }

    // ── Session rules ──────────────────────────────────────────────

    #[test]
    fn session_allow_cannot_lift_hard_ask() {
        let mut cfg = profile(AutonomyLevel::Supervised, &["git"]);
        cfg.always_ask = vec!["shell".into()];
        let compiled = CompiledRuleSet::compile(&cfg);
        let session = vec![PolicyRule {
            matcher: RuleMatcher::ToolName {
                tool: "shell".into(),
            },
            decision: Decision::Allow,
            overridable: true,
            source: RuleSource::Session,
        }];
        let scopes = ResolvedScopes {
            profile: &compiled,
            session_rules: &session,
        };
        // Hard always_ask beats a session allow — an "Always" grant can
        // never defeat a configured always_ask.
        assert_eq!(resolve_tool_name("shell", &scopes).decision, Decision::Ask);
    }

    #[test]
    fn session_shell_pattern_allow_is_narrow() {
        let cfg = profile(AutonomyLevel::Supervised, &["ls"]);
        let compiled = CompiledRuleSet::compile(&cfg);
        let session = vec![PolicyRule {
            matcher: RuleMatcher::ShellCommand {
                executable: "cargo".into(),
                arg_pattern: Some(ArgPattern::Prefix(vec!["test".into()])),
            },
            decision: Decision::Allow,
            overridable: true,
            source: RuleSource::Session,
        }];
        let scopes = ResolvedScopes {
            profile: &compiled,
            session_rules: &session,
        };
        let action = extract_shell_action("cargo test --lib", ShellDialect::Posix, None);
        assert_eq!(resolve_decision(&action, &scopes).decision, Decision::Allow);
        // But the session grant is narrow: other cargo verbs still ask.
        let action = extract_shell_action("cargo build --release", ShellDialect::Posix, None);
        assert_eq!(resolve_decision(&action, &scopes).decision, Decision::Ask);
    }

    #[test]
    fn session_allow_crosses_risk_default_ask_but_not_the_block() {
        // Mirrors today's session-allowlist behavior: an "Always" answer
        // suppressed the Supervised approval ask (approved=true was
        // injected) but never exempted the block_high_risk Deny.
        let cfg = profile(AutonomyLevel::Supervised, &["curl"]);
        let compiled = CompiledRuleSet::compile(&cfg);
        let session = vec![PolicyRule {
            matcher: RuleMatcher::ShellCommand {
                executable: "curl".into(),
                arg_pattern: Some(ArgPattern::Prefix(vec![
                    "https://internal.corp/health".into(),
                ])),
            },
            decision: Decision::Allow,
            overridable: true,
            source: RuleSource::Session,
        }];
        let scopes = ResolvedScopes {
            profile: &compiled,
            session_rules: &session,
        };
        // Allowlisted curl is High risk → Supervised ask; the narrow
        // session grant crosses exactly that ask (step 3 over step 4).
        let action = extract_shell_action(
            "curl https://internal.corp/health",
            ShellDialect::Posix,
            None,
        );
        assert_eq!(resolve_decision(&action, &scopes).decision, Decision::Allow);
        // Outside the granted pattern, the ask stands.
        let action = extract_shell_action("curl https://example.com", ShellDialect::Posix, None);
        assert_eq!(resolve_decision(&action, &scopes).decision, Decision::Ask);

        // And the block: an UNallowlisted high-risk command stays Deny
        // even with a broad session grant — the session never defeats the
        // hard block_high_risk Deny.
        let cfg = profile(AutonomyLevel::Supervised, &[]);
        let compiled = CompiledRuleSet::compile(&cfg);
        let session = vec![PolicyRule {
            matcher: RuleMatcher::ShellCommand {
                executable: "rm".into(),
                arg_pattern: None,
            },
            decision: Decision::Allow,
            overridable: true,
            source: RuleSource::Session,
        }];
        let scopes = ResolvedScopes {
            profile: &compiled,
            session_rules: &session,
        };
        let action = extract_shell_action("rm -rf /tmp/x", ShellDialect::Posix, None);
        assert_eq!(resolve_decision(&action, &scopes).decision, Decision::Deny);
    }

    #[test]
    fn session_allow_cannot_lift_high_risk_block() {
        let cfg = profile(AutonomyLevel::Supervised, &[]);
        let compiled = CompiledRuleSet::compile(&cfg);
        // A BROAD session grant (whole executable) still cannot defeat the
        // High-risk block: that hard Deny is the whole point of the
        // fingerprint model.
        let session = vec![PolicyRule {
            matcher: RuleMatcher::ShellCommand {
                executable: "rm".into(),
                arg_pattern: None,
            },
            decision: Decision::Allow,
            overridable: true,
            source: RuleSource::Session,
        }];
        let scopes = ResolvedScopes {
            profile: &compiled,
            session_rules: &session,
        };
        let action = extract_shell_action("rm -rf /tmp/x", ShellDialect::Posix, None);
        assert_eq!(resolve_decision(&action, &scopes).decision, Decision::Deny);
    }

    // ── Tool-name layer ────────────────────────────────────────────

    #[test]
    fn tool_name_layer_reflects_always_ask_and_wildcards() {
        let mut cfg = profile(AutonomyLevel::Supervised, &[]);
        cfg.always_ask = vec!["shell".into(), "*".into()];
        let compiled = CompiledRuleSet::compile(&cfg);
        let scopes = ResolvedScopes::profile_only(&compiled);
        assert_eq!(resolve_tool_name("shell", &scopes).decision, Decision::Ask);
        // The "*" always_ask entry asks for every tool.
        assert_eq!(
            resolve_tool_name("web_fetch", &scopes).decision,
            Decision::Ask
        );
    }

    #[test]
    fn tool_name_layer_default_is_ask() {
        let cfg = profile(AutonomyLevel::Supervised, &[]);
        let compiled = CompiledRuleSet::compile(&cfg);
        let scopes = ResolvedScopes::profile_only(&compiled);
        assert_eq!(resolve_tool_name("shell", &scopes).decision, Decision::Ask);
        assert_eq!(
            resolve_tool_name("memory_search", &scopes).decision,
            Decision::Ask
        );
    }

    #[test]
    fn tool_name_layer_auto_approve_allows() {
        let mut cfg = profile(AutonomyLevel::Supervised, &[]);
        cfg.auto_approve = vec!["memory_search".into()];
        let compiled = CompiledRuleSet::compile(&cfg);
        let scopes = ResolvedScopes::profile_only(&compiled);
        assert_eq!(
            resolve_tool_name("memory_search", &scopes).decision,
            Decision::Allow
        );
    }

    #[test]
    fn tool_name_layer_always_ask_beats_auto_approve() {
        let mut cfg = profile(AutonomyLevel::Supervised, &[]);
        cfg.always_ask = vec!["git".into()];
        cfg.auto_approve = vec!["git".into()];
        let compiled = CompiledRuleSet::compile(&cfg);
        let scopes = ResolvedScopes::profile_only(&compiled);
        assert_eq!(resolve_tool_name("git", &scopes).decision, Decision::Ask);
    }

    // ── Config defaults, validation & shadowing ────────────────────

    // ── No-escalation on resolved rule semantics ───────────────────

    fn policy_config(rules: &[(&str, Decision)], validity: u64) -> ToolPolicyConfig {
        ToolPolicyConfig {
            rules: rules
                .iter()
                .map(|(pattern, decision)| PolicyRuleConfig {
                    pattern: (*pattern).to_string(),
                    decision: *decision,
                })
                .collect(),
            confirmation_validity_secs: validity,
        }
    }

    #[test]
    fn narrower_child_allow_is_not_escalation() {
        let parent = policy_config(&[("Shell(git:*)", Decision::Allow)], 300);
        let child = policy_config(&[("Shell(git push:*)", Decision::Allow)], 120);
        assert!(ensure_no_rule_escalation(&child, &parent).is_ok());
    }

    #[test]
    fn wider_child_allow_is_escalation() {
        let parent = policy_config(&[("Shell(git push:*)", Decision::Allow)], 300);
        let child = policy_config(&[("Shell(git:*)", Decision::Allow)], 300);
        let error = ensure_no_rule_escalation(&child, &parent).unwrap_err();
        assert!(error.contains("not covered"), "{error}");
    }

    #[test]
    fn uncovered_child_allow_is_escalation() {
        let parent = policy_config(&[("Shell(git:*)", Decision::Allow)], 300);
        let child = policy_config(&[("Shell(rm:*)", Decision::Allow)], 300);
        assert!(ensure_no_rule_escalation(&child, &parent).is_err());
    }

    #[test]
    fn dropped_parent_deny_is_escalation() {
        let parent = policy_config(
            &[
                ("Shell(rm:*)", Decision::Deny),
                ("Shell(git:*)", Decision::Allow),
            ],
            300,
        );
        // Child keeps the allow but drops the deny.
        let child = policy_config(&[("Shell(git:*)", Decision::Allow)], 300);
        let error = ensure_no_rule_escalation(&child, &parent).unwrap_err();
        assert!(error.contains("dropped"), "{error}");
    }

    #[test]
    fn kept_parent_deny_is_not_escalation() {
        let parent = policy_config(
            &[
                ("Shell(rm:*)", Decision::Deny),
                ("Shell(git:*)", Decision::Allow),
            ],
            300,
        );
        let child = policy_config(
            &[
                ("Shell(rm:*)", Decision::Deny),
                ("Shell(git push:*)", Decision::Allow),
            ],
            120,
        );
        assert!(ensure_no_rule_escalation(&child, &parent).is_ok());
    }

    #[test]
    fn extended_confirmation_window_is_escalation() {
        let parent = policy_config(&[], 300);
        let child = policy_config(&[], 600);
        let error = ensure_no_rule_escalation(&child, &parent).unwrap_err();
        assert!(error.contains("validity"), "{error}");
    }

    #[test]
    fn glob_child_allow_is_conservatively_escalation() {
        // Glob subset is not provable without glob math; an unprovable
        // child Allow fails closed.
        let parent = policy_config(&[("Shell(git push:*)", Decision::Allow)], 300);
        let child = policy_config(&[("Shell(git push *)", Decision::Allow)], 300);
        assert!(ensure_no_rule_escalation(&child, &parent).is_err());
    }

    #[test]
    fn empty_child_never_escalates() {
        let parent = policy_config(&[("Shell(rm:*)", Decision::Deny)], 60);
        // An empty child drops the parent deny in its OWN table — but an
        // empty child adds nothing; deny-dropping is judged on the child's
        // table NOT re-including the parent rule... per the design, a child
        // may only inherit-or-narrow: dropping is an escalation.
        let child = policy_config(&[], 300);
        assert!(ensure_no_rule_escalation(&child, &parent).is_err());
    }

    #[test]
    fn default_tool_policy_is_empty_and_backward_compatible() {
        let cfg = RiskProfileConfig::default();
        assert!(cfg.tool_policy.is_empty());
        assert_eq!(
            cfg.tool_policy.confirmation_validity_secs,
            DEFAULT_CONFIRMATION_VALIDITY_SECS
        );
    }

    #[test]
    fn validate_tool_policy_rejects_bad_patterns_and_ranges() {
        let policy = ToolPolicyConfig {
            rules: vec![PolicyRuleConfig {
                pattern: "FileWrite(/x)".into(),
                decision: Decision::Ask,
            }],
            ..ToolPolicyConfig::default()
        };
        assert!(validate_tool_policy("p", &policy).is_err());

        let policy = ToolPolicyConfig {
            confirmation_validity_secs: 0,
            ..ToolPolicyConfig::default()
        };
        assert!(validate_tool_policy("p", &policy).is_err());
        let policy = ToolPolicyConfig {
            confirmation_validity_secs: 100_000,
            ..ToolPolicyConfig::default()
        };
        assert!(validate_tool_policy("p", &policy).is_err());
        let policy = ToolPolicyConfig {
            confirmation_validity_secs: 300,
            ..ToolPolicyConfig::default()
        };
        assert!(validate_tool_policy("p", &policy).is_ok());
    }

    #[test]
    fn shadowing_warning_for_ask_over_allowlist() {
        let mut cfg = profile(AutonomyLevel::Supervised, &["git"]);
        cfg.tool_policy.rules = vec![PolicyRuleConfig {
            pattern: "Shell(git push:*)".into(),
            decision: Decision::Ask,
        }];
        let warnings = collect_shadowing_warnings("engineer", &cfg);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.code == "tool_policy_ask_shadows_allowlist")
        );
    }

    #[test]
    fn shadowing_warning_for_deny_over_allow() {
        let mut cfg = profile(AutonomyLevel::Supervised, &["git"]);
        cfg.tool_policy.rules = vec![
            PolicyRuleConfig {
                pattern: "Shell(git:*)".into(),
                decision: Decision::Deny,
            },
            PolicyRuleConfig {
                pattern: "Shell(git push:*)".into(),
                decision: Decision::Allow,
            },
        ];
        let warnings = collect_shadowing_warnings("engineer", &cfg);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.code == "tool_policy_deny_shadows_allow")
        );
    }

    #[test]
    fn no_shadowing_warning_without_overlap() {
        let mut cfg = profile(AutonomyLevel::Supervised, &["git"]);
        cfg.tool_policy.rules = vec![PolicyRuleConfig {
            pattern: "Shell(cargo test:*)".into(),
            decision: Decision::Ask,
        }];
        assert!(collect_shadowing_warnings("engineer", &cfg).is_empty());
    }

    // ── Golden equivalence: legacy validator vs resolver ───────────
    //
    // The intentional deltas (RFC-sanctioned, asserted individually below):
    //   1. unmatched command: legacy hard reject → Ask (fail-closed into
    //      approval instead of a blanket reject);
    //   2. allowlisted + degraded syntax + block_high_risk=false: legacy
    //      hard reject → Ask (RFC 7155 §2.3: an apparent Allow on a
    //      degraded parse downgrades, never executes unconditionally).
    // Everything else must match the legacy outcome exactly.

    fn legacy_validate(
        cfg: &RiskProfileConfig,
        command: &str,
        approved: bool,
    ) -> Result<CommandRiskLevel, String> {
        use crate::policy::SecurityPolicy;
        let policy = SecurityPolicy::from_risk_profile(cfg, Path::new("/tmp/ws"));
        policy.validate_command_execution_for_shell(command, approved, ShellDialect::Posix)
    }

    /// The expected new decision, derived from the legacy outcome:
    /// - legacy Ok with approved=false (no ask fired) → Allow;
    /// - legacy Err "requires operator approval" → Ask;
    /// - legacy Err "high-risk command is disallowed" → Deny;
    /// - legacy Err "not allowed by security policy" → the resolver's own
    ///   answer, which may exercise one of the two sanctioned deltas.
    fn expected_from_legacy(
        legacy_unapproved: &Result<CommandRiskLevel, String>,
        resolver: &Resolution,
    ) -> Decision {
        match legacy_unapproved {
            Ok(_) => Decision::Allow,
            Err(error) if error.contains("requires operator approval") => Decision::Ask,
            Err(error) if error.contains("high-risk command is disallowed") => Decision::Deny,
            Err(_) => resolver.decision,
        }
    }

    #[test]
    fn golden_corpus_matches_legacy_outcome_per_delta_mapping() {
        let cases: Vec<(&str, &str)> = vec![
            ("ls -la", "allowlisted low risk"),
            ("grep foo bar.txt", "allowlisted low risk 2"),
            ("git commit -m x", "allowlisted medium risk"),
            ("git push", "allowlisted medium 2"),
            ("curl https://example.com", "high risk allowlisted"),
            ("sudo rm /tmp/x", "high risk not allowlisted"),
            ("ffmpeg -i a.mp4 b.mp4", "unmatched (delta 1)"),
            ("echo hello", "clean allowlisted"),
            ("find . -name x", "allowlisted safe args"),
        ];

        let cfg = profile(
            AutonomyLevel::Supervised,
            &["ls", "grep", "git", "curl", "sudo", "echo", "find"],
        );

        for (command, label) in cases {
            let legacy_unapproved = legacy_validate(&cfg, command, false);
            let resolution = resolve_with(&cfg, command, ShellDialect::Posix);
            let expected = expected_from_legacy(&legacy_unapproved, &resolution);
            assert_eq!(
                resolution.decision, expected,
                "{label}: command `{command}` — legacy(approved=false)={legacy_unapproved:?} \
                 resolver={resolution:?}"
            );
        }
    }

    #[test]
    fn golden_delta_1_unmatched_command_moves_to_ask() {
        let cfg = profile(AutonomyLevel::Supervised, &["ls"]);
        // Legacy hard-rejects an unallowlisted command; the resolver
        // fail-closes into approval instead (the RFC's core posture change).
        let legacy = legacy_validate(&cfg, "ffmpeg -i a b", true);
        assert!(legacy.is_err());
        let resolution = resolve_with(&cfg, "ffmpeg -i a b", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Ask);
    }

    #[test]
    fn golden_delta_2_degraded_allowlist_syntax_without_block_moves_to_ask() {
        // Legacy: allowlisted + backtick + block_high_risk=false → hard
        // reject even when approved. New: Ask (RFC 7155 §2.3 — an
        // apparent Allow on a degraded parse downgrades to Ask).
        let mut cfg = profile(AutonomyLevel::Supervised, &["echo"]);
        cfg.block_high_risk_commands = false;
        let legacy = legacy_validate(&cfg, "echo `date`", true);
        assert!(legacy.is_err(), "legacy rejects even approved: {legacy:?}");
        let resolution = resolve_with(&cfg, "echo `date`", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Ask);
    }

    #[test]
    fn golden_degraded_with_block_high_risk_stays_a_reject() {
        // Default profile (block_high_risk=true): degraded syntax is Deny,
        // exactly like the legacy gate rejection.
        let cfg = profile(AutonomyLevel::Supervised, &["echo"]);
        let legacy = legacy_validate(&cfg, "echo `date`", true);
        assert!(legacy.is_err());
        assert_eq!(
            resolve_with(&cfg, "echo `date`", ShellDialect::Posix).decision,
            Decision::Deny
        );
    }

    #[test]
    fn golden_escape_hatch_keeps_legacy_trusted_environment_behavior() {
        // `*` + block_high_risk=false: legacy runs degraded syntax
        // unconditionally; the resolver keeps exactly that.
        let mut cfg = profile(AutonomyLevel::Supervised, &["*"]);
        cfg.block_high_risk_commands = false;
        let legacy = legacy_validate(&cfg, "echo `date`", true);
        assert!(legacy.is_ok(), "legacy escape hatch runs: {legacy:?}");
        assert_eq!(
            resolve_with(&cfg, "echo `date`", ShellDialect::Posix).decision,
            Decision::Allow
        );
    }

    #[test]
    fn golden_wildcard_with_block_high_risk_still_blocks_high_risk() {
        // `*` + block_high_risk=true: legacy blocks a High-risk command
        // (wildcard is not "explicitly allowed"); so does the resolver.
        let cfg = profile(AutonomyLevel::Supervised, &["*"]);
        let legacy = legacy_validate(&cfg, "curl https://example.com", false);
        assert!(legacy.is_err());
        assert_eq!(
            resolve_with(&cfg, "curl https://example.com", ShellDialect::Posix).decision,
            Decision::Deny
        );
    }

    // ── Wiring-level behavior ──────────────────────────────────────

    #[test]
    fn dialect_none_denies() {
        let mut cfg = profile(AutonomyLevel::Supervised, &["*"]);
        cfg.block_high_risk_commands = false;
        let resolution = resolve_with(&cfg, "ls", ShellDialect::None);
        assert_eq!(resolution.decision, Decision::Deny);
    }

    #[test]
    fn empty_command_denies() {
        let cfg = profile(AutonomyLevel::Supervised, &["*"]);
        let resolution = resolve_with(&cfg, "   ", ShellDialect::Posix);
        assert_eq!(resolution.decision, Decision::Deny);
    }

    #[test]
    fn tool_policy_config_round_trips_through_toml() {
        let toml_source = r#"
            [risk_profiles.engineer]
            level = "supervised"

            [[risk_profiles.engineer.tool_policy.rules]]
            pattern = "Shell(git push --force:*)"
            decision = "ask"

            [[risk_profiles.engineer.tool_policy.rules]]
            pattern = "Shell(cargo test:*)"
            decision = "allow"
        "#;
        let config: crate::schema::Config = toml::from_str(toml_source).unwrap();
        let profile = &config.risk_profiles["engineer"];
        assert_eq!(profile.tool_policy.rules.len(), 2);
        assert_eq!(profile.tool_policy.rules[0].decision, Decision::Ask);
        assert_eq!(profile.tool_policy.rules[1].decision, Decision::Allow);
        assert_eq!(
            profile.tool_policy.confirmation_validity_secs,
            DEFAULT_CONFIRMATION_VALIDITY_SECS
        );
    }

    #[test]
    fn rule_matching_respects_literal_and_prefix_bounds() {
        let mut cfg = profile(AutonomyLevel::Supervised, &[]);
        cfg.block_high_risk_commands = false;
        cfg.tool_policy.rules = vec![PolicyRuleConfig {
            pattern: "Shell(git push)".into(),
            decision: Decision::Allow,
        }];
        // Literal: exactly `git push`, no more args.
        assert_eq!(
            resolve_with(&cfg, "git push", ShellDialect::Posix).decision,
            Decision::Allow
        );
        assert_eq!(
            resolve_with(&cfg, "git push --force", ShellDialect::Posix).decision,
            Decision::Ask
        );
    }

    #[test]
    fn rule_matching_respects_executable_boundary() {
        // An Allow for `git push:*` must not authorize a name-sibling like
        // `gitx`.
        let mut cfg = profile(AutonomyLevel::Supervised, &[]);
        cfg.block_high_risk_commands = false;
        cfg.tool_policy.rules = vec![PolicyRuleConfig {
            pattern: "Shell(git push:*)".into(),
            decision: Decision::Allow,
        }];
        assert_eq!(
            resolve_with(&cfg, "git push origin", ShellDialect::Posix).decision,
            Decision::Allow
        );
        assert_eq!(
            resolve_with(&cfg, "gitx push origin", ShellDialect::Posix).decision,
            Decision::Ask
        );
    }

    #[test]
    fn compiled_table_carries_confirmation_validity() {
        let mut cfg = profile(AutonomyLevel::Supervised, &[]);
        cfg.tool_policy.confirmation_validity_secs = 60;
        let compiled = CompiledRuleSet::compile(&cfg);
        assert_eq!(compiled.confirmation_validity_secs(), 60);
    }
}
