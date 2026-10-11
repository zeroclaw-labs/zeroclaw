//! Commit authorization edits made by `zeroclaw config` and `zeroclaw user`
//! through the running daemon.
//!
//! The daemon compiles the policy it enforces from the `[users]`,
//! `[permission_profiles]` and `[oidc]` sections and
//! `security.trust_daemon_uid` (`zeroclaw_runtime::rpc::auth::auth_inputs`).
//! An edit that only rewrites config.toml leaves a running daemon enforcing
//! the previous policy while the CLI reports success.
//!
//! A daemon for this configuration is running when the heartbeat beside
//! config.toml is recent and names a process that is alive. When no
//! heartbeat shows one, the CLI probes the configuration's ownership lock
//! once, which every daemon takes before it loads its configuration: while
//! another process holds it, the edit is saved and reported as pending,
//! since that process may be a daemon that is still starting. While no
//! daemon runs and the lock shows none, every edit keeps the direct save and
//! today's output, and no endpoint is contacted, whatever `ZEROCLAW_SOCKET`
//! names.
//!
//! While one runs, an edit that writes the authorization inputs, or changes
//! them, goes to the daemon (`config/set` or `config/set-many`, or for a
//! roster entry's removal or a cleared secret `config/map-key-delete` or
//! `config/delete`), which validates, saves, swaps and publishes it as one
//! step. A write that
//! re-asserts the value already in the file goes to the daemon too: the file
//! is not what the daemon enforces. When the daemon cannot be reached, is not
//! the process the heartbeat names, runs another release or refuses this
//! caller at the handshake, binding no principal, or when the CLI does not
//! verify which process serves its endpoint on this platform, the CLI saves
//! the edit locally and reports it as pending until the daemon reloads. A
//! refusal at the handshake is how a daemon enforcing a deny-all policy
//! answers, and how one answers a caller its roster does not map, so the
//! local save is what lets an operator repair a lockout. When the daemon
//! binds a principal and then refuses it the edit, rejects the edit, or is
//! asked and never answers, the command fails and saves nothing: saving
//! locally would hand the file an edit the daemon refused, install a policy
//! it rejected, or race an edit it may already have applied.
//!
//! A write the daemon's config methods do not take, and `config init`, which
//! has no daemon path for an authorization entry, are saved locally and
//! reported as pending the same way. Every local save reported as pending is
//! compile-checked before it is made. An edit that turns a policy that
//! compiles into one that does not is refused and nothing is saved: the
//! daemon could never take it, and would install a deny-all policy in its
//! place at its next reload. An edit to a policy that already does not
//! compile is saved, so a lockout can be repaired one field at a time.

use std::cell::OnceCell;
use std::fmt;
use std::path::PathBuf;
#[cfg(unix)]
use std::time::Duration;

use serde_json::Value;
use zeroclaw_runtime::live_config_authority::{ConfigOwnershipError, ConfigOwnershipGuard};
use zeroclaw_runtime::rpc::auth::{auth_inputs, is_auth_input_path, validate_accepted_auth_config};
use zeroclaw_runtime::rpc::types::{
    ConfigDeleteParams, ConfigMapKeyDeleteParams, ConfigMapKeysParams, ConfigMapKeysResult,
    ConfigSetManyParams, ConfigSetParams,
};

use crate::config::Config;
#[cfg(unix)]
use crate::daemon_rpc::CLI_VERSION;
use crate::daemon_rpc::{self, DaemonCallError};

/// How long to keep asking a running daemon whose endpoint does not accept
/// connections.
#[cfg(unix)]
const RECONNECT_WINDOW: Duration = Duration::from_secs(3);
#[cfg(unix)]
const RECONNECT_INTERVAL: Duration = Duration::from_millis(250);

/// The authorization inputs as they stood before a command staged its edit,
/// and the configuration they came from, which tells whether the policy they
/// made compiled.
pub(crate) struct AuthSnapshot {
    inputs: Value,
    config: Box<Config>,
    /// Whether `config`'s policy compiled. Compiling it builds a provider,
    /// and an HTTP client, for every OIDC entry, so it is judged only when an
    /// edit needs the answer, and at most once.
    compiled: OnceCell<bool>,
}

impl AuthSnapshot {
    pub(crate) fn capture(config: &Config) -> anyhow::Result<Self> {
        Ok(Self {
            inputs: auth_inputs(config)?,
            config: Box::new(config.clone()),
            compiled: OnceCell::new(),
        })
    }

    /// Whether the policy of the configuration this snapshot was captured
    /// from compiled.
    fn compiles(&self) -> bool {
        *self
            .compiled
            .get_or_init(|| validate_accepted_auth_config(&self.config).is_ok())
    }

    /// Whether `staged` compiles to a different authorization policy than the
    /// configuration this snapshot was captured from.
    pub(crate) fn changed_by(&self, staged: &Config) -> anyhow::Result<bool> {
        Ok(auth_inputs(staged)? != self.inputs)
    }

    /// Refuse `staged` when it turns a policy that compiled into one that does
    /// not. A policy that did not compile before the edit may still not
    /// compile after it: a daemon enforcing deny-all in its place can then be
    /// repaired one field at a time.
    fn refuse_breaking(&self, staged: &Config, suggest_patch: bool) -> Result<(), CommitFailure> {
        if !self.compiles() {
            return Ok(());
        }
        validate_accepted_auth_config(staged).map_err(|error| {
            CommitFailure::PolicyWouldNotCompile {
                error: error.to_string(),
                suggest_patch,
            }
        })
    }
}

/// Why an authorization edit was saved without reaching the running daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingReason {
    /// The daemon refused this caller at the handshake, binding no principal
    /// to it: a caller its roster does not map, or any caller while it
    /// enforces a deny-all policy.
    #[cfg(unix)]
    Refused,
    /// The daemon runs another release than this CLI.
    #[cfg(unix)]
    VersionMismatch,
    /// The daemon answered, but the handshake failed.
    #[cfg(unix)]
    Handshake,
    /// The endpoint of the running daemon could not be reached.
    #[cfg(unix)]
    Unreachable,
    /// The process serving the endpoint is not the daemon this
    /// configuration's heartbeat names, so it was sent nothing.
    #[cfg(unix)]
    OtherDaemon,
    /// The CLI does not verify which process serves the daemon's named pipe,
    /// so the edit was not sent.
    #[cfg(not(unix))]
    UnverifiedEndpoint,
    /// No heartbeat confirms a daemon, but another process holds this
    /// configuration's ownership lock, which a daemon takes before it loads
    /// its configuration: it may be a daemon that is still starting. Without
    /// a heartbeat there is no pid to check an endpoint against, so the edit
    /// was not sent.
    OwnerUnconfirmed,
    /// The command has no daemon path for this write and wrote config.toml
    /// itself.
    OfflineCommand,
    /// The daemon's config methods do not take this write (clearing or
    /// masking a secret, creating a keyed list row, an oversized batch), so
    /// the CLI wrote config.toml itself.
    NotReplayable,
}

impl PendingReason {
    /// Stable name for machine-readable output.
    pub(crate) fn code(self) -> &'static str {
        match self {
            #[cfg(unix)]
            Self::Refused => "refused",
            #[cfg(unix)]
            Self::VersionMismatch => "version_mismatch",
            #[cfg(unix)]
            Self::Handshake => "handshake",
            #[cfg(unix)]
            Self::Unreachable => "unreachable",
            #[cfg(unix)]
            Self::OtherDaemon => "other_daemon",
            #[cfg(not(unix))]
            Self::UnverifiedEndpoint => "unverified_endpoint",
            Self::OwnerUnconfirmed => "owner_unconfirmed",
            Self::OfflineCommand => "offline_command",
            Self::NotReplayable => "not_replayable",
        }
    }
}

/// What became of an edit's authorization inputs.
#[derive(Debug)]
pub(crate) enum Publication {
    /// The edit neither writes nor changes the authorization inputs; the
    /// daemon was not contacted.
    NotAuthorizationEdit,
    /// No daemon for this configuration is running, and none was contacted.
    /// Save locally; output unchanged.
    NoDaemon,
    /// The daemon validated, saved, swapped and published the edit. The
    /// caller must NOT save locally.
    Applied,
    /// The daemon took the request but its live configuration already
    /// lacked what the edit removes, so it changed and saved nothing: an
    /// entry the file carries that the daemon never loaded, such as one
    /// added by hand since it started. Save locally, so the file stops
    /// carrying it too; the daemon has nothing to apply.
    DaemonUnchanged,
    /// A daemon runs, or may be starting, but did not take the edit: it could
    /// not be asked, is not this configuration's, refused the caller at the
    /// handshake, or was never asked because the write has no daemon path, no
    /// heartbeat confirms it, or the CLI does not verify its endpoint on this
    /// platform. Save locally and report pending reload.
    ///
    /// `detail` is the evidence behind `reason`: the daemon's refusal, the
    /// handshake failure, the identity mismatch or the connect error. For
    /// `VersionMismatch` it is the daemon's version; for `OwnerUnconfirmed`,
    /// the ownership lock another process holds; for `OfflineCommand`,
    /// `NotReplayable` and `UnverifiedEndpoint` it is empty.
    Pending {
        reason: PendingReason,
        detail: String,
    },
}

impl Publication {
    /// `Some(json!({"applied": true}))`, `Some(json!({"applied": false,
    /// "pending_reload": true, "reason": code}))`, or `None` when the daemon
    /// has nothing to apply.
    pub(crate) fn envelope_field(&self) -> Option<Value> {
        match self {
            Self::NotAuthorizationEdit | Self::NoDaemon | Self::DaemonUnchanged => None,
            Self::Applied => Some(serde_json::json!({ "applied": true })),
            Self::Pending { reason, .. } => Some(serde_json::json!({
                "applied": false,
                "pending_reload": true,
                "reason": reason.code(),
            })),
        }
    }

    /// Print the human notice to stderr (nothing for `NotAuthorizationEdit`,
    /// `NoDaemon` or `DaemonUnchanged`). With `json` output the envelope's `daemon` member
    /// carries the outcome instead, so stderr stays clean for the machine
    /// reading it.
    pub(crate) fn report(&self, json: bool) {
        if json {
            return;
        }
        if let Some(notice) = self.notice() {
            eprintln!("{notice}");
        }
    }

    fn notice(&self) -> Option<String> {
        match self {
            Self::NotAuthorizationEdit | Self::NoDaemon | Self::DaemonUnchanged => None,
            Self::Applied => Some(crate::t(
                "cli-config-auth-applied",
                "Authorization change applied to the running daemon.",
            )),
            Self::Pending { reason, detail } => {
                let reason = match reason {
                    #[cfg(unix)]
                    PendingReason::Refused => crate::ta(
                        "cli-config-auth-reason-refused",
                        &[("detail", detail)],
                        "the daemon refused this caller",
                    ),
                    #[cfg(unix)]
                    PendingReason::VersionMismatch => crate::ta(
                        "cli-config-auth-reason-version",
                        &[("daemon", detail), ("cli", CLI_VERSION)],
                        "the running daemon is another version; restart the daemon",
                    ),
                    #[cfg(unix)]
                    PendingReason::Handshake => crate::ta(
                        "cli-config-auth-reason-handshake",
                        &[("detail", detail)],
                        "the daemon handshake failed",
                    ),
                    #[cfg(unix)]
                    PendingReason::Unreachable => crate::ta(
                        "cli-config-auth-reason-unreachable",
                        &[("detail", detail)],
                        "a daemon may be running, but its endpoint could not be reached",
                    ),
                    #[cfg(unix)]
                    PendingReason::OtherDaemon => crate::ta(
                        "cli-config-auth-reason-other-daemon",
                        &[("detail", detail)],
                        "the daemon answering at the socket is not the one for this configuration",
                    ),
                    #[cfg(not(unix))]
                    PendingReason::UnverifiedEndpoint => crate::t(
                        "cli-config-auth-reason-unverified",
                        "the CLI does not verify which process serves the daemon's named pipe, so it did not send the edit",
                    ),
                    PendingReason::OwnerUnconfirmed => crate::ta(
                        "cli-config-auth-reason-owner-unconfirmed",
                        &[("detail", detail)],
                        "another ZeroClaw process holds this configuration ({$detail}) but no daemon heartbeat confirms it; a daemon that is still starting may not see this edit until its next reload",
                    ),
                    PendingReason::OfflineCommand => crate::t(
                        "cli-config-auth-reason-offline",
                        "this command writes config.toml directly",
                    ),
                    PendingReason::NotReplayable => crate::t(
                        "cli-config-auth-reason-not-replayable",
                        "this write is one the daemon's config methods do not take, so the CLI wrote config.toml itself",
                    ),
                };
                Some(crate::ta(
                    "cli-config-auth-pending",
                    &[("reason", &reason)],
                    "Saved to config.toml, but the running daemon still enforces the previous authorization policy.",
                ))
            }
        }
    }
}

/// An authorization edit that failed in a way local saving must not paper
/// over: the caller exits non-zero and saves nothing.
#[derive(Debug)]
pub(crate) enum CommitFailure {
    /// The daemon examined the edit and refused it; nothing was saved.
    /// `code` is the JSON-RPC error code of the verdict (`INVALID_PARAMS` for
    /// an invalid edit). `suggest_patch` is set for a single-field edit, which
    /// may have failed only because it needs another field set with it.
    #[cfg(unix)]
    Rejected {
        message: String,
        code: i64,
        suggest_patch: bool,
    },
    /// The daemon bound this caller to a principal at the handshake, then
    /// refused that principal the edit (AUTH_REQUIRED or FORBIDDEN); nothing
    /// was saved. A local save would put an edit the daemon refused this
    /// caller into the file its next reload installs. `message` is the
    /// daemon's reason.
    #[cfg(unix)]
    Forbidden { message: String },
    /// A daemon runs but the edit is to be saved locally, whether the daemon
    /// did not take it or was never offered it, and saving it would turn a
    /// policy that compiles into one that does not, which the daemon would
    /// replace with deny-all at its next reload; nothing was saved.
    /// `suggest_patch` as for `Rejected`.
    PolicyWouldNotCompile { error: String, suggest_patch: bool },
    /// The request reached the daemon but no answer came back, so the edit may
    /// or may not have been applied. `path` is a property to check.
    #[cfg(unix)]
    Unknown { path: String },
    /// A `config patch` batch the running daemon would have committed
    /// carries a `test` op on the authorization input `path`. The op was
    /// checked against the CLI's copy of config.toml, while the batch's
    /// writes would land on the daemon's live configuration, so the daemon
    /// was not contacted and nothing was saved.
    UncheckableTest { path: String },
}

impl fmt::Display for CommitFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (text, suggest_patch) = match self {
            #[cfg(unix)]
            Self::Rejected {
                message,
                suggest_patch,
                ..
            } => (
                crate::ta(
                    "cli-config-auth-rejected",
                    &[("reason", message)],
                    "The running daemon rejected this edit; nothing was saved.",
                ),
                *suggest_patch,
            ),
            // A principal the daemon refused an edit is not helped by
            // setting fields together, so there is no patch hint.
            #[cfg(unix)]
            Self::Forbidden { message } => {
                return f.write_str(&crate::ta(
                    "cli-config-auth-forbidden",
                    &[("reason", message)],
                    "The running daemon identified you and refused this edit; nothing was saved: {$reason}",
                ));
            }
            Self::PolicyWouldNotCompile {
                error,
                suggest_patch,
            } => (
                crate::ta(
                    "cli-config-auth-policy-invalid",
                    &[("error", error)],
                    "Nothing was saved: the authorization policy this edit leaves would not compile, so the running daemon's next reload would refuse every principal.",
                ),
                *suggest_patch,
            ),
            #[cfg(unix)]
            Self::Unknown { path } => {
                return f.write_str(&crate::ta(
                    "cli-config-auth-unknown",
                    &[("path", path)],
                    "The running daemon did not answer; the edit may or may not have been applied.",
                ));
            }
            Self::UncheckableTest { path } => {
                return f.write_str(&crate::ta(
                    "cli-config-auth-patch-test-op",
                    &[("path", path)],
                    "A `test` op on `{$path}` cannot be checked against the running daemon's live configuration, so the patch was not applied; remove the `test` op, or check the value with `zeroclaw config get` first",
                ));
            }
        };
        f.write_str(&text)?;
        if suggest_patch {
            write!(
                f,
                "\n{}",
                crate::t(
                    "cli-config-auth-rejected-hint",
                    "Use `zeroclaw config patch` to set fields that are only valid together.",
                )
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for CommitFailure {}

/// Commit one staged `config set` edit through the daemon when it writes or
/// changes the authorization inputs. `staged` is the CLI's config with the
/// edit already applied in memory; `value` is the raw string the CLI staged.
/// `holds_config_ownership` is set when the command itself holds the
/// configuration's ownership lock, as an offline agent mutation does.
pub(crate) async fn commit_set(
    before: &AuthSnapshot,
    staged: &Config,
    path: &str,
    value: &str,
    comment: Option<&str>,
    holds_config_ownership: bool,
) -> anyhow::Result<Publication> {
    if !(before.changed_by(staged)? || is_auth_input_path(path)) {
        return Ok(Publication::NotAuthorizationEdit);
    }
    let params = serde_json::to_value(ConfigSetParams {
        prop: path.to_owned(),
        value: Value::String(value.to_owned()),
        // A daemon that applies the edit writes the comment too, under the
        // config write lock it saved under, so the CLI does not touch
        // config.toml after it. The CLI writes the comment itself only when
        // it saves the edit itself.
        comment: comment
            .filter(|comment| !comment.is_empty())
            .map(str::to_owned),
    })?;
    commit(
        before,
        staged,
        "config/set",
        params,
        path,
        true,
        holds_config_ownership,
        None,
        |_| true,
    )
    .await
}

/// Same for a `config patch` batch, through `config/set-many` with
/// `{"sets":[{"prop","value"}...]}` (values are JSON strings). `tested` are
/// the properties the patch's `test` ops checked on `staged`: a running
/// daemon that would take the batch applies its writes to its live
/// configuration instead, so a `test` op on an authorization input then
/// fails the batch with `CommitFailure::UncheckableTest` before the daemon is
/// contacted.
pub(crate) async fn commit_set_many(
    before: &AuthSnapshot,
    staged: &Config,
    sets: &[(String, String)],
    tested: &[String],
    holds_config_ownership: bool,
) -> anyhow::Result<Publication> {
    if !(before.changed_by(staged)? || sets.iter().any(|(prop, _)| is_auth_input_path(prop))) {
        return Ok(Publication::NotAuthorizationEdit);
    }
    let params = serde_json::to_value(ConfigSetManyParams {
        sets: sets
            .iter()
            .map(|(prop, value)| ConfigSetParams {
                prop: prop.clone(),
                value: Value::String(value.clone()),
                comment: None,
            })
            .collect(),
    })?;
    let uncheckable_test = tested
        .iter()
        .map(String::as_str)
        .find(|path| is_auth_input_path(path));
    commit(
        before,
        staged,
        "config/set-many",
        params,
        batch_check_path(sets),
        false,
        holds_config_ownership,
        uncheckable_test,
        |_| true,
    )
    .await
}

/// Same for clearing the property `path` through `config/delete`, which
/// writes it empty. `config/set` refuses to overwrite a secret with an empty
/// value, so this is how the daemon takes the clearing of one, as when
/// `zeroclaw user disable-password` removes a roster password.
pub(crate) async fn commit_delete(
    before: &AuthSnapshot,
    staged: &Config,
    path: &str,
) -> anyhow::Result<Publication> {
    if !(before.changed_by(staged)? || is_auth_input_path(path)) {
        return Ok(Publication::NotAuthorizationEdit);
    }
    let params = serde_json::to_value(ConfigDeleteParams {
        prop: path.to_owned(),
    })?;
    commit(
        before,
        staged,
        "config/delete",
        params,
        path,
        false,
        false,
        None,
        |_| true,
    )
    .await
}

/// Same for removing the entry `key` from the map section `section` through
/// `config/map-key-delete`, as when `zeroclaw user remove` deletes a roster
/// entry. A daemon that applies it ends the entry's bindings for established
/// and new connections at once. One whose live configuration has no such
/// entry answers that it deleted nothing, which is
/// `Publication::DaemonUnchanged`.
pub(crate) async fn commit_map_key_delete(
    before: &AuthSnapshot,
    staged: &Config,
    section: &str,
    key: &str,
) -> anyhow::Result<Publication> {
    let path = format!("{section}.{key}");
    if !(before.changed_by(staged)? || is_auth_input_path(&path)) {
        return Ok(Publication::NotAuthorizationEdit);
    }
    let params = serde_json::to_value(ConfigMapKeyDeleteParams {
        path: section.to_owned(),
        key: key.to_owned(),
    })?;
    commit(
        before,
        staged,
        "config/map-key-delete",
        params,
        &path,
        false,
        false,
        None,
        |answer| answer.get("deleted").and_then(Value::as_bool) != Some(false),
    )
    .await
}

/// The entry names under the map section `section` that the daemon serving
/// `config` holds live, when a recent heartbeat names a running daemon and it
/// answers this caller. `None` when no heartbeat names one, or it could not
/// be asked or refused the read; a commit that follows then reports why.
/// Unlike a commit, this never probes the ownership lock.
pub(crate) async fn live_map_keys(config: &Config, section: &str) -> Option<Vec<String>> {
    let pid = running_daemon(config)?;
    let params = serde_json::to_value(ConfigMapKeysParams {
        path: section.to_owned(),
    })
    .ok()?;
    let answer = send(config, pid, "config/map-keys", params).await.ok()?;
    serde_json::from_value::<ConfigMapKeysResult>(answer)
        .ok()
        .map(|result| result.keys)
}

/// The property a notice that a batch's outcome is unknown points at. The
/// daemon applies a batch as one unit, so any of its properties shows the
/// outcome: the first authorization input it writes, else its first property.
fn batch_check_path(sets: &[(String, String)]) -> &str {
    sets.iter()
        .map(|(prop, _)| prop.as_str())
        .find(|prop| is_auth_input_path(prop))
        .or_else(|| sets.first().map(|(prop, _)| prop.as_str()))
        .unwrap_or_default()
}

/// Classify a write the CLI saves itself (`config init`, or a `config set` or
/// `config patch` the daemon's config methods do not take) before it is
/// saved. `staged` is the config with the write applied in memory, `touched`
/// the properties it wrote.
///
/// `NotAuthorizationEdit` when the write neither changes the authorization
/// inputs nor writes one of them; `NoDaemon` when no daemon for this
/// configuration is running and its ownership lock shows none either;
/// `Pending { reason: OwnerUnconfirmed }` when no heartbeat confirms a daemon
/// but another process holds that lock; else `Pending { reason }`. While a
/// daemon runs or may be starting, a write that turns a policy that compiles
/// into one that does not fails with `CommitFailure::PolicyWouldNotCompile`,
/// and the caller saves nothing. `holds_config_ownership` as for
/// `commit_set`.
pub(crate) fn classify_local_save(
    before: &AuthSnapshot,
    staged: &Config,
    touched: &[String],
    reason: PendingReason,
    suggest_patch: bool,
    holds_config_ownership: bool,
) -> anyhow::Result<Publication> {
    if !(before.changed_by(staged)? || touched.iter().any(|path| is_auth_input_path(path))) {
        return Ok(Publication::NotAuthorizationEdit);
    }
    let (reason, detail) = match daemon_presence(staged, holds_config_ownership) {
        DaemonPresence::Running(_) => (reason, String::new()),
        DaemonPresence::OwnerUnconfirmed { lock } => {
            (PendingReason::OwnerUnconfirmed, lock.display().to_string())
        }
        DaemonPresence::Absent => return Ok(Publication::NoDaemon),
    };
    before.refuse_breaking(staged, suggest_patch)?;
    Ok(Publication::Pending { reason, detail })
}

/// What the CLI can tell, without contacting anything, about the daemon
/// serving a configuration.
#[derive(Debug)]
enum DaemonPresence {
    /// The heartbeat is recent and names this live process.
    Running(u32),
    /// No heartbeat confirms a daemon, and the ownership lock shows none: it
    /// was free, this command holds it, or it cannot be taken at all.
    Absent,
    /// No heartbeat confirms a daemon, but another process holds the
    /// ownership lock at `lock`.
    OwnerUnconfirmed { lock: PathBuf },
}

/// Look for the daemon serving `config`: the heartbeat first, and only when
/// it shows none, the configuration's ownership lock. A command looks once,
/// so the lock is probed at most once per command.
///
/// Every daemon takes the ownership lock before it loads its configuration
/// and holds it for as long as it runs, so a lock another process holds while
/// no heartbeat shows a daemon may be a daemon that is still starting. The
/// probe holds the lock only for the instant of the check. A daemon starting
/// at that same instant fails its own acquire, as it does against any
/// offline CLI mutation that holds the lock.
///
/// `holds_config_ownership` is set when this command already holds the lock
/// itself. No daemon can run meanwhile, and a second acquire in this process
/// would conflict with the command's own guard, so the lock is not probed.
fn daemon_presence(config: &Config, holds_config_ownership: bool) -> DaemonPresence {
    if let Some(pid) = running_daemon(config) {
        return DaemonPresence::Running(pid);
    }
    if holds_config_ownership {
        return DaemonPresence::Absent;
    }
    match ConfigOwnershipGuard::acquire(&config.data_dir) {
        Ok(guard) => {
            drop(guard);
            DaemonPresence::Absent
        }
        Err(ConfigOwnershipError::AlreadyOwned { path }) => {
            DaemonPresence::OwnerUnconfirmed { lock: path }
        }
        // A daemon takes the same lock before it loads its configuration, so
        // one that cannot be taken here could not be taken by a daemon either.
        Err(ConfigOwnershipError::Unavailable(_)) => DaemonPresence::Absent,
    }
}

/// The pid of the daemon serving `config`'s directory, when one is running:
/// the heartbeat beside config.toml is recent and the process it names is
/// alive. A daemon that stops leaves its last heartbeat behind, so the stamp
/// alone would call it running for a few seconds after it exits, and a
/// heartbeat that names no process is no evidence of one.
///
/// A torn heartbeat read blocks this thread briefly. The CLI's commands run
/// on the thread that drives its runtime, not on a worker, so the wait holds
/// up only the command itself.
fn running_daemon(config: &Config) -> Option<u32> {
    let recorded = zeroclaw_runtime::daemon::recorded_daemon(config)?;
    if !recorded.is_recent(chrono::Utc::now()) {
        return None;
    }
    recorded.pid.filter(|pid| process_is_alive(*pid))
}

/// Signal 0 delivers nothing and only reports whether `pid` exists. EPERM
/// means it exists under another account, which still counts as running.
/// Pid 0 names no daemon: `kill(0, 0)` would probe this process's own group.
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid == 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 performs no action on the target; it only
    // reports whether the process exists and may be signalled.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Without a cheap existence probe here, a recent heartbeat is the liveness
/// signal. Pid 0 still names no daemon.
#[cfg(not(unix))]
fn process_is_alive(pid: u32) -> bool {
    pid != 0
}

/// Send an edit to the daemon the command found running as `pid`.
///
/// A running daemon whose endpoint accepts no connection is usually between
/// generations: a reload rebinds its endpoint within moments, so it is asked
/// again briefly before it counts as unreachable. Each retry reads the
/// heartbeat afresh and asks the daemon it names then, so a successor is
/// asked under its own pid. A heartbeat that stops naming a running daemon
/// does not end the wait: the daemon seen running may be restarting, and its
/// successor may load the file before this edit reaches it. A wait that ends
/// without an answer returns the last connect failure, so the edit is
/// reported pending rather than saved as if no daemon ran. The wait reads
/// only the heartbeat: the ownership lock was probed, if at all, when the
/// command first looked for the daemon.
#[cfg(unix)]
async fn send(
    config: &Config,
    mut pid: u32,
    method: &str,
    params: Value,
) -> Result<Value, DaemonCallError> {
    let deadline = tokio::time::Instant::now() + RECONNECT_WINDOW;
    loop {
        // Why this attempt reached nothing at the daemon's endpoint.
        let unreachable = match daemon_rpc::call(config, pid, method, params.clone()).await {
            Err(error) if nothing_listening(&error) => error,
            outcome => return outcome,
        };
        pid = loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(unreachable);
            }
            tokio::time::sleep(RECONNECT_INTERVAL).await;
            if let Some(next) = running_daemon(config) {
                break next;
            }
        };
    }
}

/// Send an edit to the daemon the command found running as `pid`. The
/// daemon client opens nothing on this platform, so there is no endpoint to
/// wait for.
#[cfg(not(unix))]
async fn send(
    config: &Config,
    pid: u32,
    method: &str,
    params: Value,
) -> Result<Value, DaemonCallError> {
    daemon_rpc::call(config, pid, method, params).await
}

/// Whether a call failed because nothing listens at the endpoint: no socket
/// there, or one that refuses connections. Any other connect failure leaves
/// open that the daemon is there and could not be reached.
#[cfg(unix)]
fn nothing_listening(error: &DaemonCallError) -> bool {
    matches!(
        error,
        DaemonCallError::Unavailable { source, .. }
            if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            )
    )
}

/// Send an authorization edit to the daemon and classify the outcome.
/// `config` is the CLI's config with the edit staged; `check_path` is the
/// property a caller should inspect when the outcome is unknown;
/// `uncheckable_test` is the authorization input a batch's `test` op checked
/// on `config`, if any; `changed` tells from the daemon's answer whether it
/// changed anything, and an answer that it did not is
/// `Publication::DaemonUnchanged`.
async fn commit(
    before: &AuthSnapshot,
    config: &Config,
    method: &str,
    params: Value,
    check_path: &str,
    suggest_patch: bool,
    holds_config_ownership: bool,
    uncheckable_test: Option<&str>,
    changed: fn(&Value) -> bool,
) -> anyhow::Result<Publication> {
    let pid = match daemon_presence(config, holds_config_ownership) {
        DaemonPresence::Running(pid) => pid,
        DaemonPresence::Absent => return Ok(Publication::NoDaemon),
        // Without a heartbeat there is no pid to check the process at the
        // endpoint against, so nothing is sent, and the edit is saved like
        // any other a daemon did not take.
        DaemonPresence::OwnerUnconfirmed { lock } => {
            before.refuse_breaking(config, suggest_patch)?;
            return Ok(Publication::Pending {
                reason: PendingReason::OwnerUnconfirmed,
                detail: lock.display().to_string(),
            });
        }
    };
    // The daemon would apply the batch to its live configuration, not to the
    // copy its `test` ops were checked against. Only the Unix client sends a
    // running daemon anything; elsewhere the batch is saved to that copy, so
    // its `test` ops hold as checked.
    if cfg!(unix)
        && let Some(path) = uncheckable_test
    {
        return Err(CommitFailure::UncheckableTest {
            path: path.to_owned(),
        }
        .into());
    }
    let (reason, detail) = match send(config, pid, method, params).await {
        Ok(answer) if changed(&answer) => return Ok(Publication::Applied),
        Ok(_) => return Ok(Publication::DaemonUnchanged),
        #[cfg(not(unix))]
        Err(DaemonCallError::UnverifiableEndpoint) => {
            (PendingReason::UnverifiedEndpoint, String::new())
        }
        #[cfg(unix)]
        Err(DaemonCallError::Unavailable { path, source }) => (
            PendingReason::Unreachable,
            format!("{}: {source}", path.display()),
        ),
        #[cfg(unix)]
        Err(DaemonCallError::OtherDaemon { detail }) => (PendingReason::OtherDaemon, detail),
        #[cfg(unix)]
        Err(DaemonCallError::VersionMismatch { daemon }) => {
            (PendingReason::VersionMismatch, daemon)
        }
        #[cfg(unix)]
        Err(DaemonCallError::Handshake { detail }) => (PendingReason::Handshake, detail),
        // No principal was bound, which is how a daemon enforcing deny-all
        // answers every caller: the local save is what repairs a lockout.
        #[cfg(unix)]
        Err(DaemonCallError::HandshakeRefused { message, .. }) => (PendingReason::Refused, message),
        #[cfg(unix)]
        Err(DaemonCallError::RequestRefused { message, .. }) => {
            return Err(CommitFailure::Forbidden { message }.into());
        }
        #[cfg(unix)]
        Err(DaemonCallError::Rejected { code, message }) => {
            return Err(CommitFailure::Rejected {
                message,
                code,
                suggest_patch,
            }
            .into());
        }
        #[cfg(unix)]
        Err(DaemonCallError::NoAnswer { detail }) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "method": method,
                        "path": check_path,
                        "detail": detail,
                    })),
                "the daemon did not answer an authorization edit; its outcome is unknown"
            );
            return Err(CommitFailure::Unknown {
                path: check_path.to_owned(),
            }
            .into());
        }
    };
    // The caller saves a pending edit itself while a daemon runs. A policy
    // that does not compile would never reach that daemon, and its next
    // reload or restart would install a deny-all policy in its place, so an
    // edit that breaks a policy that compiles is refused here the way the
    // daemon would refuse it.
    before.refuse_breaking(config, suggest_patch)?;
    Ok(Publication::Pending { reason, detail })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    #[cfg(unix)]
    use zeroclaw_api::jsonrpc::error_codes::INVALID_PARAMS;
    use zeroclaw_config::schema::{PermissionProfileConfig, UserConfig};

    /// A config whose heartbeat and endpoint live in a fresh, empty directory.
    /// On Unix the directory sits under /tmp because macOS caps a socket path
    /// at 104 bytes, and a longer one fails to connect for another reason
    /// than nothing listening.
    fn scratch_config() -> (tempfile::TempDir, Config) {
        assert!(
            std::env::var_os("ZEROCLAW_SOCKET").is_none(),
            "ZEROCLAW_SOCKET would point these tests at a live daemon; unset it"
        );
        #[cfg(unix)]
        let dir = tempfile::Builder::new().prefix("zc").tempdir_in("/tmp");
        #[cfg(not(unix))]
        let dir = tempfile::tempdir();
        let dir = dir.expect("a scratch directory");
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            config_path: dir.path().join("config.toml"),
            ..Config::default()
        };
        (dir, config)
    }

    /// Record `pid` as `config`'s daemon, stamped `written_at`, where the
    /// daemon's heartbeat goes.
    fn write_heartbeat_of(config: &Config, written_at: chrono::DateTime<chrono::Utc>, pid: u32) {
        let path = zeroclaw_runtime::daemon::state_file_path(config);
        std::fs::create_dir_all(path.parent().expect("the state file has a directory"))
            .expect("the state directory is writable");
        let state = serde_json::json!({ "pid": pid, "written_at": written_at.to_rfc3339() });
        std::fs::write(path, state.to_string()).expect("the state file is writable");
    }

    /// Record this test process as `config`'s running daemon: a fresh stamp
    /// and a pid that is alive.
    fn record_running_daemon(config: &Config) {
        write_heartbeat_of(config, chrono::Utc::now(), std::process::id());
    }

    /// The pid of a process that has already exited, so nothing runs under it.
    #[cfg(unix)]
    fn exited_pid() -> u32 {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("`true` is available");
        child.wait().expect("`true` exits");
        child.id()
    }

    fn with_trust_flipped(config: &Config) -> Config {
        let mut staged = config.clone();
        staged.security.trust_daemon_uid = !staged.security.trust_daemon_uid;
        staged
    }

    /// `config` with a roster entry that has a uid and no permission profile,
    /// which no authorization policy compiles.
    fn with_profileless_user(config: &Config) -> Config {
        let mut staged = config.clone();
        staged.users.insert(
            "bob".into(),
            UserConfig {
                principal_id: None,
                uid: Some(4242),
                password_hash: None,
                permission_profiles: Vec::new(),
            },
        );
        staged
    }

    /// `config` with its endpoint at a path too long for a unix socket, so a
    /// connect fails for another reason than nothing listening there.
    #[cfg(unix)]
    fn with_unconnectable_endpoint(config: &Config) -> Config {
        let mut staged = config.clone();
        staged.data_dir = config.data_dir.join("d".repeat(120));
        staged
    }

    fn assert_would_not_compile(failure: &anyhow::Error, suggest: bool) {
        match failure.downcast_ref::<CommitFailure>() {
            Some(CommitFailure::PolicyWouldNotCompile {
                error,
                suggest_patch,
            }) => {
                assert!(error.contains("users.bob"), "{error}");
                assert_eq!(*suggest_patch, suggest);
            }
            other => panic!("expected PolicyWouldNotCompile, got {other:?}"),
        }
    }

    #[test]
    fn auth_snapshot_sees_only_authorization_edits() {
        let config = Config::default();
        let before = AuthSnapshot::capture(&config).expect("the default config encodes");
        assert!(before.compiles(), "the default policy compiles");

        let mut unrelated = config.clone();
        unrelated.gateway.host = "0.0.0.0".into();
        assert!(!before.changed_by(&unrelated).expect("encodes"));

        assert!(
            before
                .changed_by(&with_trust_flipped(&config))
                .expect("encodes")
        );

        let mut profiled = config.clone();
        profiled
            .permission_profiles
            .insert("operator".into(), PermissionProfileConfig::default());
        assert!(before.changed_by(&profiled).expect("encodes"));

        let broken = AuthSnapshot::capture(&with_profileless_user(&config)).expect("encodes");
        assert!(
            !broken.compiles(),
            "a profileless roster entry does not compile"
        );
    }

    #[test]
    fn envelope_field_reports_whether_the_daemon_applied_the_edit() {
        assert_eq!(Publication::NotAuthorizationEdit.envelope_field(), None);
        assert_eq!(Publication::NoDaemon.envelope_field(), None);
        assert_eq!(Publication::DaemonUnchanged.envelope_field(), None);
        assert_eq!(Publication::DaemonUnchanged.notice(), None);
        assert_eq!(
            Publication::Applied.envelope_field(),
            Some(serde_json::json!({ "applied": true }))
        );
        for (reason, code) in [
            #[cfg(unix)]
            (PendingReason::Refused, "refused"),
            #[cfg(unix)]
            (PendingReason::VersionMismatch, "version_mismatch"),
            #[cfg(unix)]
            (PendingReason::Handshake, "handshake"),
            #[cfg(unix)]
            (PendingReason::Unreachable, "unreachable"),
            #[cfg(unix)]
            (PendingReason::OtherDaemon, "other_daemon"),
            #[cfg(not(unix))]
            (PendingReason::UnverifiedEndpoint, "unverified_endpoint"),
            (PendingReason::OwnerUnconfirmed, "owner_unconfirmed"),
            (PendingReason::OfflineCommand, "offline_command"),
            (PendingReason::NotReplayable, "not_replayable"),
        ] {
            let pending = Publication::Pending {
                reason,
                detail: "evidence".into(),
            };
            assert_eq!(
                pending.envelope_field(),
                Some(serde_json::json!({
                    "applied": false,
                    "pending_reload": true,
                    "reason": code,
                }))
            );
        }
    }

    /// The notices name the evidence the operator needs, and every Fluent key
    /// resolves (a missing key renders as `{key}`).
    #[test]
    fn notices_carry_their_evidence() {
        assert!(Publication::NoDaemon.notice().is_none());
        assert!(Publication::NotAuthorizationEdit.notice().is_none());
        let applied = Publication::Applied.notice().expect("applied has a notice");
        assert!(!applied.contains("{cli-"), "{applied}");

        #[cfg(unix)]
        {
            let version = Publication::Pending {
                reason: PendingReason::VersionMismatch,
                detail: "0.0.1".into(),
            }
            .notice()
            .expect("pending has a notice");
            assert!(version.contains("0.0.1"), "{version}");
            assert!(version.contains(CLI_VERSION), "{version}");
            assert!(!version.contains("{cli-"), "{version}");

            for reason in [
                PendingReason::Refused,
                PendingReason::Handshake,
                PendingReason::Unreachable,
                PendingReason::OtherDaemon,
            ] {
                let notice = Publication::Pending {
                    reason,
                    detail: "the daemon's own words".into(),
                }
                .notice()
                .expect("pending has a notice");
                assert!(notice.contains("the daemon's own words"), "{notice}");
                assert!(!notice.contains("{cli-"), "{notice}");
            }
        }
        let unconfirmed = Publication::Pending {
            reason: PendingReason::OwnerUnconfirmed,
            detail: "/data/config-lifecycle.lock".into(),
        }
        .notice()
        .expect("pending has a notice");
        assert!(
            unconfirmed.contains("/data/config-lifecycle.lock"),
            "the notice names the lock another process holds: {unconfirmed}"
        );
        assert!(!unconfirmed.contains("{cli-"), "{unconfirmed}");
        let mut local_notices = BTreeSet::new();
        for reason in [
            #[cfg(not(unix))]
            PendingReason::UnverifiedEndpoint,
            PendingReason::OfflineCommand,
            PendingReason::NotReplayable,
        ] {
            let notice = Publication::Pending {
                reason,
                detail: String::new(),
            }
            .notice()
            .expect("pending has a notice");
            assert!(!notice.contains("{cli-"), "{notice}");
            local_notices.insert(notice);
        }
        assert_eq!(
            local_notices.len(),
            if cfg!(unix) { 2 } else { 3 },
            "each reason without evidence says why: {local_notices:?}"
        );
    }

    #[test]
    fn commit_failures_name_the_reason_and_the_property_to_check() {
        #[cfg(unix)]
        {
            let rejected = |suggest_patch| {
                CommitFailure::Rejected {
                    message: "users.alice.permission_profiles is required".into(),
                    code: i64::from(INVALID_PARAMS),
                    suggest_patch,
                }
                .to_string()
            };
            let plain = rejected(false);
            assert!(
                plain.contains("users.alice.permission_profiles is required"),
                "{plain}"
            );
            let hinted = rejected(true);
            assert!(
                hinted.starts_with(&plain) && hinted.len() > plain.len(),
                "a single-field edit adds the patch hint: {hinted}"
            );
            assert!(!hinted.contains("{cli-"), "{hinted}");

            let unknown = CommitFailure::Unknown {
                path: "users.alice.uid".into(),
            }
            .to_string();
            assert!(unknown.contains("users.alice.uid"), "{unknown}");
            assert!(!unknown.contains("{cli-"), "{unknown}");

            let forbidden = CommitFailure::Forbidden {
                message: "Principal is not granted config:update".into(),
            }
            .to_string();
            assert!(
                forbidden.contains("Principal is not granted config:update")
                    && forbidden.contains("nothing was saved"),
                "{forbidden}"
            );
            assert!(!forbidden.contains("{cli-"), "{forbidden}");
        }

        let uncheckable = CommitFailure::UncheckableTest {
            path: "users.alice.uid".into(),
        }
        .to_string();
        assert!(
            uncheckable.contains("`users.alice.uid`")
                && uncheckable.contains("zeroclaw config get"),
            "the failure names the property and how to check it: {uncheckable}"
        );
        assert!(!uncheckable.contains("{cli-"), "{uncheckable}");

        let uncompilable = |suggest_patch| {
            CommitFailure::PolicyWouldNotCompile {
                error: "users.alice.permission_profiles is required".into(),
                suggest_patch,
            }
            .to_string()
        };
        let plain = uncompilable(false);
        assert!(
            plain.contains("users.alice.permission_profiles is required"),
            "{plain}"
        );
        assert!(!plain.contains("{cli-"), "{plain}");
        let hinted = uncompilable(true);
        assert!(
            hinted.starts_with(&plain) && hinted.len() > plain.len(),
            "a single-field edit adds the patch hint: {hinted}"
        );
    }

    /// An unknown outcome of a batch points at the first authorization input
    /// it writes, which is what the operator needs to check.
    #[test]
    fn a_batch_points_an_unknown_outcome_at_its_first_authorization_input() {
        let sets = |props: &[&str]| -> Vec<(String, String)> {
            props
                .iter()
                .map(|prop| ((*prop).to_owned(), String::new()))
                .collect()
        };
        assert_eq!(
            batch_check_path(&sets(&[
                "gateway.host",
                "users.bob.uid",
                "oidc.corp.issuer"
            ])),
            "users.bob.uid"
        );
        assert_eq!(
            batch_check_path(&sets(&["gateway.host", "gateway.port"])),
            "gateway.host",
            "a batch without one points at its first property"
        );
        assert_eq!(batch_check_path(&[]), "");
    }

    #[test]
    fn a_daemon_runs_while_its_heartbeat_is_recent_and_names_a_live_process() {
        let (_dir, config) = scratch_config();
        assert_eq!(running_daemon(&config), None, "no heartbeat");

        let now = chrono::Utc::now();
        let own = std::process::id();
        write_heartbeat_of(&config, now, own);
        assert_eq!(running_daemon(&config), Some(own));

        write_heartbeat_of(&config, now - chrono::TimeDelta::seconds(60), own);
        assert_eq!(running_daemon(&config), None, "a stale heartbeat");

        write_heartbeat_of(&config, now + chrono::TimeDelta::seconds(2), own);
        assert_eq!(
            running_daemon(&config),
            Some(own),
            "a stamp slightly ahead of this clock is still recent"
        );

        #[cfg(unix)]
        {
            write_heartbeat_of(&config, now, exited_pid());
            assert_eq!(
                running_daemon(&config),
                None,
                "a fresh stamp left behind by a process that exited"
            );
            write_heartbeat_of(&config, now, 1);
            assert_eq!(
                running_daemon(&config),
                Some(1),
                "a process this account may not signal still counts as running"
            );
        }
        write_heartbeat_of(&config, now, 0);
        assert_eq!(
            running_daemon(&config),
            None,
            "pid 0 names no daemon, only this process's own group"
        );

        let unstamped = serde_json::json!({ "written_at": now.to_rfc3339() });
        std::fs::write(
            zeroclaw_runtime::daemon::state_file_path(&config),
            unstamped.to_string(),
        )
        .expect("writable");
        assert_eq!(
            running_daemon(&config),
            None,
            "a heartbeat that names no process"
        );
        std::fs::write(zeroclaw_runtime::daemon::state_file_path(&config), "{").expect("writable");
        assert_eq!(running_daemon(&config), None, "an unreadable heartbeat");
    }

    /// With no daemon running the direct save is unchanged, even one whose
    /// policy would not compile: nothing is contacted and nothing checked.
    #[test]
    fn classify_local_save_checks_nothing_without_a_running_daemon() {
        let (_dir, config) = scratch_config();
        let before = AuthSnapshot::capture(&config).expect("encodes");
        let broken = with_profileless_user(&config);
        let touched = ["users.bob.uid".to_owned()];

        let publication = classify_local_save(
            &before,
            &broken,
            &touched,
            PendingReason::OfflineCommand,
            true,
            false,
        )
        .expect("no daemon, no compile check");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "{publication:?}"
        );

        write_heartbeat_of(
            &broken,
            chrono::Utc::now() - chrono::TimeDelta::seconds(60),
            std::process::id(),
        );
        let publication = classify_local_save(
            &before,
            &broken,
            &touched,
            PendingReason::NotReplayable,
            true,
            false,
        )
        .expect("a stale heartbeat is no daemon");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "{publication:?}"
        );

        let unrelated = ["gateway.host".to_owned()];
        let publication = classify_local_save(
            &before,
            &config,
            &unrelated,
            PendingReason::OfflineCommand,
            true,
            false,
        )
        .expect("classifies");
        assert!(
            matches!(publication, Publication::NotAuthorizationEdit),
            "{publication:?}"
        );
    }

    /// While a daemon runs, a local save that would break a policy that
    /// compiles is refused, and one that keeps it compiling is pending.
    #[test]
    fn classify_local_save_refuses_to_break_a_compiling_policy_while_a_daemon_runs() {
        let (_dir, config) = scratch_config();
        let before = AuthSnapshot::capture(&config).expect("encodes");
        record_running_daemon(&config);
        let touched = ["users.bob.uid".to_owned()];

        for suggest_patch in [true, false] {
            let failure = classify_local_save(
                &before,
                &with_profileless_user(&config),
                &touched,
                PendingReason::OfflineCommand,
                suggest_patch,
                false,
            )
            .expect_err("a policy that stops compiling is refused");
            assert_would_not_compile(&failure, suggest_patch);
        }

        for reason in [PendingReason::OfflineCommand, PendingReason::NotReplayable] {
            let publication = classify_local_save(
                &before,
                &with_trust_flipped(&config),
                &["security.trust_daemon_uid".to_owned()],
                reason,
                true,
                false,
            )
            .expect("a policy that still compiles is saved");
            assert!(
                matches!(
                    publication,
                    Publication::Pending { reason: pending, ref detail }
                        if pending == reason && detail.is_empty()
                ),
                "a running daemon still enforces the old policy: {publication:?}"
            );
        }
    }

    /// A file whose policy already does not compile leaves the daemon
    /// enforcing deny-all; an edit to it is saved and pending, so the lockout
    /// can be repaired one field at a time.
    #[test]
    fn classify_local_save_allows_an_edit_to_a_policy_that_already_did_not_compile() {
        let (_dir, config) = scratch_config();
        let broken = with_profileless_user(&config);
        let before = AuthSnapshot::capture(&broken).expect("encodes");
        record_running_daemon(&broken);

        let publication = classify_local_save(
            &before,
            &with_trust_flipped(&broken),
            &["security.trust_daemon_uid".to_owned()],
            PendingReason::NotReplayable,
            true,
            false,
        )
        .expect("a policy that did not compile before may still not");
        assert!(
            matches!(
                publication,
                Publication::Pending {
                    reason: PendingReason::NotReplayable,
                    ..
                }
            ),
            "{publication:?}"
        );
    }

    /// A write to an authorization input counts even when it leaves the
    /// inputs as they were on disk: the daemon may enforce something else.
    #[test]
    fn classify_local_save_counts_an_authorization_path_that_changed_nothing() {
        let (_dir, config) = scratch_config();
        let before = AuthSnapshot::capture(&config).expect("encodes");
        record_running_daemon(&config);

        let publication = classify_local_save(
            &before,
            &config,
            &["users.me.permission_profiles".to_owned()],
            PendingReason::NotReplayable,
            true,
            false,
        )
        .expect("classifies");
        assert!(
            matches!(
                publication,
                Publication::Pending {
                    reason: PendingReason::NotReplayable,
                    ..
                }
            ),
            "{publication:?}"
        );
    }

    #[tokio::test]
    async fn commits_contact_no_daemon_unless_one_is_running() {
        let (_dir, config) = scratch_config();
        let before = AuthSnapshot::capture(&config).expect("encodes");

        let mut unrelated = config.clone();
        unrelated.gateway.host = "0.0.0.0".into();
        let unrelated_sets = [("gateway.host".to_owned(), "0.0.0.0".to_owned())];
        let publication = commit_set(&before, &unrelated, "gateway.host", "0.0.0.0", None, false)
            .await
            .expect("classifies");
        assert!(
            matches!(publication, Publication::NotAuthorizationEdit),
            "{publication:?}"
        );
        let publication = commit_set_many(&before, &unrelated, &unrelated_sets, &[], false)
            .await
            .expect("classifies");
        assert!(
            matches!(publication, Publication::NotAuthorizationEdit),
            "{publication:?}"
        );

        let staged = with_trust_flipped(&config);
        let value = staged.security.trust_daemon_uid.to_string();
        let sets = [("security.trust_daemon_uid".to_owned(), value.clone())];
        let publication = commit_set(
            &before,
            &staged,
            "security.trust_daemon_uid",
            &value,
            None,
            false,
        )
        .await
        .expect("classifies");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "no heartbeat: {publication:?}"
        );
        let publication = commit_set_many(&before, &staged, &sets, &[], false)
            .await
            .expect("classifies");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "no heartbeat: {publication:?}"
        );

        // Re-asserting the value already on disk still looks for the daemon,
        // which finds none here.
        let value = config.security.trust_daemon_uid.to_string();
        let publication = commit_set(
            &before,
            &config,
            "security.trust_daemon_uid",
            &value,
            None,
            false,
        )
        .await
        .expect("classifies");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "an authorization path is committed even when nothing changed: {publication:?}"
        );
        let publication = commit_set_many(
            &before,
            &config,
            &[
                ("gateway.host".to_owned(), config.gateway.host.clone()),
                ("security.trust_daemon_uid".to_owned(), value),
            ],
            &[],
            false,
        )
        .await
        .expect("classifies");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "a batch with an authorization path is committed even when nothing changed: {publication:?}"
        );

        // With no daemon the edit keeps today's direct save, even one whose
        // policy would not compile.
        let publication = commit_set(
            &before,
            &with_profileless_user(&config),
            "users.bob.uid",
            "4242",
            None,
            false,
        )
        .await
        .expect("classifies");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "{publication:?}"
        );

        // A heartbeat that went stale names no running daemon either.
        write_heartbeat_of(
            &config,
            chrono::Utc::now() - chrono::TimeDelta::seconds(60),
            std::process::id(),
        );
        let publication = commit_set(
            &before,
            &staged,
            "security.trust_daemon_uid",
            "true",
            None,
            false,
        )
        .await
        .expect("classifies");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "a stale heartbeat: {publication:?}"
        );
    }

    /// Without a running daemon, a delete is classified like a write: one
    /// under the authorization inputs looks for the daemon, and finds none,
    /// even when it changes nothing, and one outside them never looks.
    #[tokio::test]
    async fn deletes_are_classified_like_writes_without_a_daemon() {
        let (_dir, config) = scratch_config();
        let mut with_bob = config.clone();
        with_bob.users.insert(
            "bob".into(),
            UserConfig {
                principal_id: None,
                uid: Some(4242),
                password_hash: None,
                permission_profiles: vec!["operator".into()],
            },
        );
        let before = AuthSnapshot::capture(&with_bob).expect("encodes");

        let publication = commit_map_key_delete(&before, &config, "users", "bob")
            .await
            .expect("classifies");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "removing a roster entry with no daemon: {publication:?}"
        );
        let publication = commit_delete(&before, &with_bob, "users.bob.password_hash")
            .await
            .expect("classifies");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "clearing a roster secret that changes nothing still looks: {publication:?}"
        );

        let publication = commit_delete(&before, &with_bob, "gateway.host")
            .await
            .expect("classifies");
        assert!(
            matches!(publication, Publication::NotAuthorizationEdit),
            "{publication:?}"
        );
        let publication = commit_map_key_delete(&before, &with_bob, "agents", "helper")
            .await
            .expect("classifies");
        assert!(
            matches!(publication, Publication::NotAuthorizationEdit),
            "{publication:?}"
        );
        assert!(
            live_map_keys(&with_bob, "users").await.is_none(),
            "no heartbeat names a daemon to ask"
        );
    }

    /// Listen where `config`'s daemon endpoint is, so a test can tell whether
    /// anything connected to it.
    #[cfg(unix)]
    fn listen_at_the_endpoint(config: &Config) -> std::os::unix::net::UnixListener {
        let endpoint = zeroclaw_runtime::rpc::local::socket_path(config);
        let listener = std::os::unix::net::UnixListener::bind(&endpoint)
            .unwrap_or_else(|error| panic!("bind {}: {error}", endpoint.display()));
        listener
            .set_nonblocking(true)
            .expect("the listener can be made nonblocking");
        listener
    }

    /// A connection the CLI made would wait in the accept queue.
    #[cfg(unix)]
    fn assert_nothing_connected(listener: &std::os::unix::net::UnixListener) {
        match listener.accept() {
            Err(error) => assert_eq!(
                error.kind(),
                std::io::ErrorKind::WouldBlock,
                "accept must find the queue empty, not fail: {error}"
            ),
            Ok((_, peer)) => panic!("something connected to the endpoint: {peer:?}"),
        }
    }

    /// With no heartbeat, the ownership lock is probed and released: a free
    /// lock is no daemon, and it is free again afterwards.
    #[tokio::test]
    async fn a_free_ownership_lock_without_a_heartbeat_is_no_daemon() {
        let (_dir, config) = scratch_config();
        let before = AuthSnapshot::capture(&config).expect("encodes");
        let staged = with_trust_flipped(&config);
        let value = staged.security.trust_daemon_uid.to_string();

        let publication = classify_local_save(
            &before,
            &staged,
            &["security.trust_daemon_uid".to_owned()],
            PendingReason::NotReplayable,
            true,
            false,
        )
        .expect("classifies");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "{publication:?}"
        );
        let publication = commit_set(
            &before,
            &staged,
            "security.trust_daemon_uid",
            &value,
            None,
            false,
        )
        .await
        .expect("classifies");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "{publication:?}"
        );
        ConfigOwnershipGuard::acquire(&config.data_dir)
            .expect("the probe holds the lock only for the instant of the check");
    }

    /// Another process holding the ownership lock while no heartbeat shows a
    /// daemon may be a daemon that is still starting: the edit is saved and
    /// pending, nothing is contacted, and the compile guard still applies.
    /// The lock this test holds is a separate open of the lock file, which
    /// conflicts with the probe the way another process's would.
    #[tokio::test]
    async fn a_lock_another_process_holds_without_a_heartbeat_leaves_the_edit_pending() {
        let (_dir, config) = scratch_config();
        let before = AuthSnapshot::capture(&config).expect("encodes");
        let owner =
            ConfigOwnershipGuard::acquire(&config.data_dir).expect("the test takes the lock");
        #[cfg(unix)]
        let listener = listen_at_the_endpoint(&config);
        let staged = with_trust_flipped(&config);
        let value = staged.security.trust_daemon_uid.to_string();
        let lock = config
            .data_dir
            .join("config-lifecycle.lock")
            .display()
            .to_string();
        let unconfirmed = |publication: &Publication| {
            matches!(
                publication,
                Publication::Pending {
                    reason: PendingReason::OwnerUnconfirmed,
                    detail,
                } if *detail == lock
            )
        };

        let publication = classify_local_save(
            &before,
            &staged,
            &["security.trust_daemon_uid".to_owned()],
            PendingReason::NotReplayable,
            true,
            false,
        )
        .expect("classifies");
        assert!(unconfirmed(&publication), "{publication:?}");
        let publication = commit_set(
            &before,
            &staged,
            "security.trust_daemon_uid",
            &value,
            None,
            false,
        )
        .await
        .expect("classifies");
        assert!(unconfirmed(&publication), "{publication:?}");
        // The batch is saved to the copy its `test` op was checked against,
        // so the op keeps its meaning.
        let publication = commit_set_many(
            &before,
            &staged,
            &[("security.trust_daemon_uid".to_owned(), value)],
            &["users.me.permission_profiles".to_owned()],
            false,
        )
        .await
        .expect("classifies");
        assert!(unconfirmed(&publication), "{publication:?}");

        let broken = with_profileless_user(&config);
        let failure = classify_local_save(
            &before,
            &broken,
            &["users.bob.uid".to_owned()],
            PendingReason::NotReplayable,
            true,
            false,
        )
        .expect_err("a policy that stops compiling is refused");
        assert_would_not_compile(&failure, true);
        let failure = commit_set(&before, &broken, "users.bob.uid", "4242", None, false)
            .await
            .expect_err("a policy that stops compiling is refused");
        assert_would_not_compile(&failure, true);

        #[cfg(unix)]
        assert_nothing_connected(&listener);
        drop(owner);
    }

    /// A command that holds the ownership lock itself, as an offline agent
    /// mutation does, knows no daemon runs, and does not probe the lock: a
    /// second acquire in its own process would conflict with its own guard.
    #[tokio::test]
    async fn a_command_that_holds_the_ownership_lock_finds_no_daemon() {
        let (_dir, config) = scratch_config();
        let before = AuthSnapshot::capture(&config).expect("encodes");
        let held = ConfigOwnershipGuard::acquire(&config.data_dir).expect("the command's lock");
        let staged = with_trust_flipped(&config);
        let value = staged.security.trust_daemon_uid.to_string();

        let publication = classify_local_save(
            &before,
            &staged,
            &["security.trust_daemon_uid".to_owned()],
            PendingReason::OfflineCommand,
            true,
            true,
        )
        .expect("classifies");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "{publication:?}"
        );
        let publication = commit_set(
            &before,
            &staged,
            "security.trust_daemon_uid",
            &value,
            None,
            true,
        )
        .await
        .expect("classifies");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "{publication:?}"
        );
        let publication = commit_set_many(
            &before,
            &staged,
            &[("security.trust_daemon_uid".to_owned(), value)],
            &["users.me.permission_profiles".to_owned()],
            true,
        )
        .await
        .expect("classifies");
        assert!(
            matches!(publication, Publication::NoDaemon),
            "{publication:?}"
        );
        drop(held);
    }

    /// A batch a running daemon would commit applies its writes to the
    /// daemon's live configuration, so a `test` op the CLI checked on its own
    /// copy of an authorization input fails the batch before the daemon is
    /// contacted. A `test` op elsewhere does not stop the batch.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_batch_testing_an_authorization_input_is_not_sent_to_a_running_daemon() {
        let (_dir, config) = scratch_config();
        let before = AuthSnapshot::capture(&config).expect("encodes");
        record_running_daemon(&config);
        let listener = listen_at_the_endpoint(&config);
        let staged = with_trust_flipped(&config);
        let sets = [(
            "security.trust_daemon_uid".to_owned(),
            staged.security.trust_daemon_uid.to_string(),
        )];

        let failure = commit_set_many(
            &before,
            &staged,
            &sets,
            &[
                "gateway.host".to_owned(),
                "users.me.permission_profiles".to_owned(),
            ],
            false,
        )
        .await
        .expect_err("the batch is refused");
        match failure.downcast_ref::<CommitFailure>() {
            Some(CommitFailure::UncheckableTest { path }) => {
                assert_eq!(path, "users.me.permission_profiles");
            }
            other => panic!("expected UncheckableTest, got {other:?}"),
        }
        assert_nothing_connected(&listener);

        // An endpoint that fails at once shows the batch was offered to the
        // daemon when its only `test` op is outside the authorization inputs.
        let unconnectable = with_unconnectable_endpoint(&config);
        record_running_daemon(&unconnectable);
        let staged = with_trust_flipped(&unconnectable);
        let publication =
            commit_set_many(&before, &staged, &sets, &["gateway.host".to_owned()], false)
                .await
                .expect("classifies");
        assert!(
            matches!(
                publication,
                Publication::Pending {
                    reason: PendingReason::Unreachable,
                    ..
                }
            ),
            "{publication:?}"
        );
    }

    /// A connect failure other than nothing listening, while a daemon runs,
    /// leaves open that the daemon is there, so the edit is pending rather
    /// than unremarked, and only if it does not break a policy that compiles.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_endpoint_that_fails_otherwise_is_unreachable_not_absent() {
        let (_dir, config) = scratch_config();
        let config = with_unconnectable_endpoint(&config);
        let before = AuthSnapshot::capture(&config).expect("encodes");
        record_running_daemon(&config);

        let staged = with_trust_flipped(&config);
        let value = staged.security.trust_daemon_uid.to_string();
        let publication = commit_set(
            &before,
            &staged,
            "security.trust_daemon_uid",
            &value,
            None,
            false,
        )
        .await
        .expect("classifies");
        match publication {
            Publication::Pending {
                reason: PendingReason::Unreachable,
                detail,
            } => assert!(detail.contains("daemon.sock"), "{detail}"),
            other => panic!("expected Pending(Unreachable), got {other:?}"),
        }

        let staged = with_profileless_user(&config);
        let failure = commit_set(&before, &staged, "users.bob.uid", "4242", None, false)
            .await
            .expect_err("a policy that stops compiling is refused");
        assert_would_not_compile(&failure, true);
        let text = failure.to_string();
        assert!(text.contains("would not compile"), "{text}");
        assert!(
            !text.contains("rejected"),
            "the daemon never examined this edit, so the CLI must not say it rejected it: {text}"
        );
    }

    /// A daemon seen running whose heartbeat then disappears may be
    /// restarting, and its successor may load the file before the edit
    /// reaches it: the edit is left pending as unreachable, never saved as if
    /// no daemon ran.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_daemon_whose_heartbeat_disappears_while_asked_leaves_the_edit_unreachable() {
        let (_dir, config) = scratch_config();
        let before = AuthSnapshot::capture(&config).expect("encodes");
        record_running_daemon(&config);
        let heartbeat = zeroclaw_runtime::daemon::state_file_path(&config);
        let stop = zeroclaw_spawn::spawn!(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            std::fs::remove_file(heartbeat).expect("the heartbeat is removable");
        });

        let staged = with_trust_flipped(&config);
        let value = staged.security.trust_daemon_uid.to_string();
        let publication = commit_set(
            &before,
            &staged,
            "security.trust_daemon_uid",
            &value,
            None,
            false,
        )
        .await
        .expect("classifies");
        stop.await.expect("the heartbeat was removed");
        assert!(
            matches!(
                publication,
                Publication::Pending {
                    reason: PendingReason::Unreachable,
                    ..
                }
            ),
            "a daemon seen running is never taken for none: {publication:?}"
        );
    }

    /// A daemon enforcing deny-all in place of a policy that does not compile
    /// refuses every caller, so each repair is saved locally; an endpoint
    /// that cannot be reached stands in for that refusal here. A step that
    /// still leaves the policy uncompiled is saved like the last one.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_policy_that_did_not_compile_can_be_repaired_one_field_at_a_time() {
        let (_dir, config) = scratch_config();
        let broken = with_unconnectable_endpoint(&with_profileless_user(&config));
        let before = AuthSnapshot::capture(&broken).expect("encodes");
        record_running_daemon(&broken);

        let staged = with_trust_flipped(&broken);
        let value = staged.security.trust_daemon_uid.to_string();
        let publication = commit_set(
            &before,
            &staged,
            "security.trust_daemon_uid",
            &value,
            None,
            false,
        )
        .await
        .expect("an edit to a policy that did not compile is saved");
        assert!(
            matches!(
                publication,
                Publication::Pending {
                    reason: PendingReason::Unreachable,
                    ..
                }
            ),
            "{publication:?}"
        );
    }

    /// Here the CLI does not verify which process serves the daemon's named
    /// pipe, so a running daemon is sent nothing: the edit is saved locally
    /// and pending, unless it would break a policy that compiles.
    #[cfg(not(unix))]
    #[tokio::test]
    async fn a_running_daemon_is_sent_nothing_on_this_platform() {
        let (_dir, config) = scratch_config();
        let before = AuthSnapshot::capture(&config).expect("encodes");
        record_running_daemon(&config);

        let staged = with_trust_flipped(&config);
        let value = staged.security.trust_daemon_uid.to_string();
        let publication = commit_set(
            &before,
            &staged,
            "security.trust_daemon_uid",
            &value,
            None,
            false,
        )
        .await
        .expect("classifies");
        assert!(
            matches!(
                publication,
                Publication::Pending {
                    reason: PendingReason::UnverifiedEndpoint,
                    ref detail,
                } if detail.is_empty()
            ),
            "{publication:?}"
        );

        // The batch is saved to the copy its `test` op was checked against,
        // so the op keeps its meaning.
        let publication = commit_set_many(
            &before,
            &staged,
            &[("security.trust_daemon_uid".to_owned(), value)],
            &["users.me.permission_profiles".to_owned()],
            false,
        )
        .await
        .expect("classifies");
        assert!(
            matches!(
                publication,
                Publication::Pending {
                    reason: PendingReason::UnverifiedEndpoint,
                    ..
                }
            ),
            "{publication:?}"
        );

        let failure = commit_set(
            &before,
            &with_profileless_user(&config),
            "users.bob.uid",
            "4242",
            None,
            false,
        )
        .await
        .expect_err("a policy that stops compiling is refused");
        assert_would_not_compile(&failure, true);
    }
}
