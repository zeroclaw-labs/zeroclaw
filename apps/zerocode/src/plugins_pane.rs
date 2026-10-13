//! `plugins` sub-tab of the Config pane.
//!
//! Renders the daemon's `plugins/list` catalog, the same body `GET
//! /api/plugins` serves: one row per package in the daemon's order, with the
//! installed record and the cached-registry record kept apart. It never merges
//! or re-sorts catalog rows, and never claims that a package is loaded,
//! running or healthy, because the catalog carries no runtime evidence.
//!
//! A package's detail also lists the plugin channel instances configured for
//! it (`[channels.plugin.<alias>]`, read with `config/list`) and can flip one
//! instance's `enabled` setting with `config/set`. That is the pane's only
//! write, made only to an instance it has just reread; it never installs or
//! removes anything, and it reports the change as configuration intent that a
//! daemon reload applies, never as a channel that started or stopped.

use std::collections::BTreeMap;
use std::sync::Arc;

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Wrap},
};
use tokio::task::JoinHandle;
use zeroclaw_api::jsonrpc::error_codes;

use crate::client::{RpcCallError, RpcCallTimeout, RpcClient};
use crate::i18n::{t, t_args};
use crate::theme;
use crate::wire::{
    ConfigFieldEntry, PluginCatalogEntry, PluginCatalogIssue, PluginCatalogIssueCode,
    PluginCatalogIssueSource, PluginsListResult,
};

/// Longest daemon-provided string kept for display, in characters.
const MAX_DISPLAY_CHARS: usize = 512;

/// Most raw characters [`display_safe`] reads from one string.
const MAX_SCANNED_CHARS: usize = 4 * MAX_DISPLAY_CHARS;

/// Most items shown from one capability or permission list; the rest are
/// counted rather than listed.
const MAX_LIST_ITEMS: usize = 32;

/// Width of the left filter and host column, matching the zeroclaw split.
const LEFT_COLUMN_WIDTH: u16 = 30;

/// Cells the list highlight gutter (`"› "`) takes from every row.
const HIGHLIGHT_GUTTER: usize = 2;

const DETAIL_SCROLL_LINES: u16 = 3;

/// Config path of the plugin channel instances, `[channels.plugin.<alias>]`.
const INSTANCE_PREFIX: &str = "channels.plugin";

/// The capability an installed package declares when it provides a channel.
const CHANNEL_CAPABILITY: &str = "channel";

/// Most instance rows the block shows at once; a longer list scrolls with
/// its selection.
const MAX_INSTANCE_ROWS: u16 = 6;

/// Rows the package detail keeps above the instance block, borders included.
const MIN_DETAIL_ROWS: u16 = 5;

/// Make one daemon-provided string safe to render on a single terminal row.
///
/// Registry and manifest text is untrusted, so every string the daemon sends
/// passes through here before it is laid out. Newline, carriage return and
/// tab become a space. Every other control character (C0, DEL and C1), every
/// Unicode format, bidi or zero-width control, and every other invisible
/// default-ignorable character (variation selectors, the combining grapheme
/// joiner, Hangul fillers) is replaced with U+FFFD, never silently removed,
/// so a hidden character stays visible instead of changing how a name reads.
/// That includes emoji presentation selectors in descriptions, the same way
/// the zero-width joiner is treated. Other whitespace becomes a space, runs of
/// whitespace collapse to one, the ends are trimmed, and the result is capped
/// at [`MAX_DISPLAY_CHARS`] characters ending in an ellipsis. At most
/// [`MAX_SCANNED_CHARS`] raw characters are read, so a value padded with long
/// whitespace runs still costs a bounded scan on every draw.
fn display_safe(raw: &str) -> String {
    let mut kept: Vec<char> = Vec::new();
    let mut pending_space = false;
    let mut chars = raw.chars();
    for c in chars.by_ref().take(MAX_SCANNED_CHARS) {
        let c = match c {
            '\n' | '\r' | '\t' => ' ',
            c if is_hidden(c) => '\u{FFFD}',
            c if c.is_whitespace() => ' ',
            c => c,
        };
        if c == ' ' {
            pending_space = !kept.is_empty();
            continue;
        }
        if pending_space {
            kept.push(' ');
            pending_space = false;
        }
        kept.push(c);
        if kept.len() > MAX_DISPLAY_CHARS {
            break;
        }
    }
    if kept.len() > MAX_DISPLAY_CHARS || chars.next().is_some() {
        kept.truncate(MAX_DISPLAY_CHARS - 1);
        while kept.last() == Some(&' ') {
            kept.pop();
        }
        kept.push('\u{2026}');
    }
    kept.into_iter().collect()
}

/// A character [`display_safe`] replaces because it would not show on a
/// terminal: a control, a format control, or an invisible filler.
fn is_hidden(c: char) -> bool {
    c.is_control() || crate::osc_status::is_format_control(c) || is_invisible_filler(c)
}

/// Characters outside the format-control denylist that render as nothing or
/// as blank space, and so could make two different names look the same: the
/// default-ignorable combining grapheme joiner, Hangul fillers, Khmer
/// inherent vowels, Mongolian variation selectors, variation selectors,
/// reserved code points and the tag and supplementary selector plane, plus
/// the blank braille pattern, a symbol that draws as an empty cell.
fn is_invisible_filler(c: char) -> bool {
    matches!(
        c as u32,
        0x034F
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x2065
            | 0x2800
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFFA0
            | 0xFFF0..=0xFFF8
            | 0xE0000..=0xE0FFF
    )
}

// ── Fetch errors ─────────────────────────────────────────────────

/// Why the catalog could not be loaded. Each kind renders its own message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CatalogError {
    /// JSON-RPC method not found: the daemon predates `plugins/list`.
    Unsupported,
    /// JSON-RPC forbidden, holding the display-safe daemon reason. The
    /// usual cause is a missing `plugins:read` grant, but the daemon also
    /// answers forbidden when its auth policy is misconfigured, so the
    /// message shows the daemon's reason instead of naming a cause.
    Forbidden(String),
    /// No response within the client's `plugins/list` budget.
    TimedOut,
    /// Any other failure, holding the display-safe daemon or client message.
    Other(String),
}

impl CatalogError {
    /// Classify a failed `plugins/list` call by its typed error, never by
    /// matching the rendered text.
    fn from_call(err: &anyhow::Error) -> Self {
        if let Some(rpc) = err.downcast_ref::<RpcCallError>() {
            return match rpc.code {
                error_codes::METHOD_NOT_FOUND => Self::Unsupported,
                error_codes::FORBIDDEN => Self::Forbidden(display_safe(&rpc.message)),
                _ => Self::Other(display_safe(&rpc.message)),
            };
        }
        if err.downcast_ref::<RpcCallTimeout>().is_some() {
            return Self::TimedOut;
        }
        Self::Other(display_safe(&format!("{err:#}")))
    }

    fn message(&self) -> String {
        match self {
            Self::Unsupported => t("zc-plugins-error-unsupported"),
            Self::Forbidden(detail) => {
                t_args("zc-plugins-error-forbidden", &[("error", detail.as_str())])
            }
            Self::TimedOut => t("zc-plugins-error-timeout"),
            Self::Other(detail) => t_args("zc-plugins-error-other", &[("error", detail.as_str())]),
        }
    }
}

// ── Channel instances ────────────────────────────────────────────

/// Why a channel-instance read or write failed, classified by its typed
/// error like [`CatalogError`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum CallFailure {
    /// JSON-RPC forbidden, holding the display-safe daemon reason.
    Forbidden(String),
    /// No response within the client's budget.
    TimedOut,
    /// Any other failure, holding the display-safe daemon or client message.
    Other(String),
}

impl CallFailure {
    fn from_call(err: &anyhow::Error) -> Self {
        if let Some(rpc) = err.downcast_ref::<RpcCallError>() {
            return match rpc.code {
                error_codes::FORBIDDEN => Self::Forbidden(display_safe(&rpc.message)),
                _ => Self::Other(display_safe(&rpc.message)),
            };
        }
        if err.downcast_ref::<RpcCallTimeout>().is_some() {
            return Self::TimedOut;
        }
        Self::Other(display_safe(&format!("{err:#}")))
    }

    /// The message for an instance list the daemon would not give.
    fn instances_message(&self) -> String {
        match self {
            Self::Forbidden(detail) => t_args(
                "zc-plugins-instances-error-forbidden",
                &[("error", detail.as_str())],
            ),
            Self::TimedOut => t("zc-plugins-instances-error-timeout"),
            Self::Other(detail) => t_args(
                "zc-plugins-instances-error-other",
                &[("error", detail.as_str())],
            ),
        }
    }

    /// The failure as a clause inside a toggle status.
    fn detail(&self) -> String {
        match self {
            Self::Forbidden(detail) | Self::Other(detail) => detail.clone(),
            Self::TimedOut => t("zc-plugins-toggle-no-answer"),
        }
    }
}

/// One `[channels.plugin.<alias>]` declaration as `config/list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChannelInstance {
    alias: String,
    /// The raw `package` value, or `None` when the daemon sent none.
    package: Option<String>,
    /// `None` when the value is missing or unreadable: unknown, never false.
    enabled: Option<bool>,
}

/// Parse `config/list` rows into channel instances sorted by alias. The
/// daemon lists `channels.plugin.<alias>.package` and `.enabled` for every
/// alias in no stable order, and every value as a JSON string. Rows outside
/// the instance table, and fields other than those two, are ignored.
fn parse_instances(entries: &[ConfigFieldEntry]) -> Vec<ChannelInstance> {
    let mut by_alias: BTreeMap<&str, ChannelInstance> = BTreeMap::new();
    for entry in entries {
        let Some(rest) = entry
            .path
            .strip_prefix(INSTANCE_PREFIX)
            .and_then(|rest| rest.strip_prefix('.'))
        else {
            continue;
        };
        let Some((alias, field)) = rest.rsplit_once('.') else {
            continue;
        };
        if alias.is_empty() || !matches!(field, "package" | "enabled") {
            continue;
        }
        let value = entry.value.as_ref().and_then(serde_json::Value::as_str);
        let instance = by_alias.entry(alias).or_insert_with(|| ChannelInstance {
            alias: alias.to_string(),
            package: None,
            enabled: None,
        });
        if field == "package" {
            instance.package = value.map(str::to_string);
        } else {
            instance.enabled = match value {
                Some("true") => Some(true),
                Some("false") => Some(false),
                _ => None,
            };
        }
    }
    by_alias.into_values().collect()
}

/// Whether an alias fits the daemon's alias grammar: 1 to 63 lowercase ASCII
/// letters, digits and single underscores, starting and ending with a letter
/// or digit. Only a hand-edited config can hold any other alias, and the
/// daemon resolves a write path by its first segment, so writing to such an
/// alias (for example `a.b`) would make the daemon create a second,
/// package-less declaration.
fn is_grammar_alias(raw: &str) -> bool {
    const MAX_ALIAS_LEN: usize = 63;
    let bytes = raw.as_bytes();
    let edge = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    !bytes.is_empty()
        && bytes.len() <= MAX_ALIAS_LEN
        && bytes.first().is_some_and(edge)
        && bytes.last().is_some_and(edge)
        && !raw.contains("__")
        && bytes.iter().all(|byte| edge(byte) || *byte == b'_')
}

/// An alias as shown in the instance list and the status line: as is when it
/// fits the alias grammar, otherwise escaped and quoted like an invalid
/// package name.
fn display_alias(raw: &str) -> String {
    if is_grammar_alias(raw) {
        raw.to_string()
    } else {
        quoted(raw)
    }
}

/// What the catalog fetch read: the catalog, then the channel instances.
struct FetchResult {
    catalog: Result<PluginsListResult, CatalogError>,
    /// `None` when the catalog failed and the instances were not read.
    instances: Option<Result<Vec<ChannelInstance>, CallFailure>>,
}

type CatalogFetch = JoinHandle<FetchResult>;

/// The channel instances as last read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InstanceState {
    /// Not read yet, or dropped with a catalog that failed to load.
    NotLoaded,
    Loaded(Vec<ChannelInstance>),
    /// The read failed; the catalog may still have loaded.
    Failed(CallFailure),
}

/// The instance a toggle acts on and the value the pane showed for it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToggleTarget {
    alias: String,
    /// The raw name of the package whose detail showed the instance.
    package: String,
    shown: bool,
}

impl ToggleTarget {
    /// Whether `current` is still the declaration the pane showed: present,
    /// naming the same package, with the same value. Anything else means the
    /// config changed since it was read, and writing would act on a
    /// declaration the user never saw (or re-create a removed one).
    fn still_matches(&self, current: Option<&ChannelInstance>) -> bool {
        current.is_some_and(|current| {
            current.package.as_deref() == Some(self.package.as_str())
                && current.enabled == Some(self.shown)
        })
    }
}

/// Why a toggle left the config as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NotChanged {
    /// The declaration changed since it was read, so nothing was written.
    Stale,
    /// The alias is outside the alias grammar, so nothing was written.
    InvalidAlias,
    Failed(CallFailure),
}

/// How a toggle ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToggleOutcome {
    /// The daemon saved the write; the value read back, or `None` when the
    /// read-back could not tell.
    Saved(Option<bool>),
    NotChanged(NotChanged),
    /// The write failed in a way that leaves its effect unknown, such as no
    /// answer in time; `stored` is the value read back, if any.
    Unconfirmed {
        failure: CallFailure,
        stored: Option<bool>,
    },
}

type ToggleTask = JoinHandle<ToggleOutcome>;

/// The toggle status line of one package's instance block.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StatusKind {
    Saving,
    Done(ToggleOutcome),
    /// Enter on an instance whose value is unknown: nothing to flip.
    UnknownValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToggleStatus {
    /// The raw package name; the line shows only in that package's detail.
    package: String,
    alias: String,
    kind: StatusKind,
}

/// The sentence telling the user how to refresh, or nothing when the
/// refresh action has no key.
fn refresh_hint() -> Option<String> {
    let keys = first_chord(crate::keymap::ConfigTabAction::Refresh);
    (!keys.is_empty()).then(|| t_args("zc-plugins-refresh-hint", &[("keys", &keys)]))
}

/// The sentence saying a saved change waits for a daemon reload, naming the
/// live reload chord.
fn reload_hint() -> String {
    let keys = first_chord(crate::keymap::GlobalAction::ReloadDaemon);
    if keys.is_empty() {
        t("zc-plugins-reload-hint-unbound")
    } else {
        t_args("zc-plugins-reload-hint", &[("keys", &keys)])
    }
}

impl ToggleStatus {
    /// Whether the line reports a problem rather than progress or success.
    fn is_warning(&self) -> bool {
        !matches!(
            self.kind,
            StatusKind::Saving | StatusKind::Done(ToggleOutcome::Saved(Some(_)))
        )
    }

    /// The status sentence. It names configuration state only: a saved
    /// change is "in config" and waits for a daemon reload, and nothing here
    /// says a channel started, stopped, or is running.
    fn text(&self) -> String {
        let alias = display_alias(&self.alias);
        let args = [("alias", alias.as_str())];
        let mut sentences = Vec::new();
        match &self.kind {
            StatusKind::Saving => sentences.push(t_args("zc-plugins-toggle-saving", &args)),
            StatusKind::UnknownValue => {
                sentences.push(t_args("zc-plugins-toggle-unknown", &args));
                sentences.extend(refresh_hint());
            }
            StatusKind::Done(ToggleOutcome::Saved(stored)) => {
                let key = match stored {
                    Some(true) => "zc-plugins-toggle-saved-enabled",
                    Some(false) => "zc-plugins-toggle-saved-disabled",
                    None => "zc-plugins-toggle-saved-unread",
                };
                sentences.push(t_args(key, &args));
                if stored.is_none() {
                    sentences.extend(refresh_hint());
                }
                sentences.push(reload_hint());
            }
            StatusKind::Done(ToggleOutcome::NotChanged(NotChanged::Stale)) => {
                sentences.push(t_args("zc-plugins-toggle-stale", &args));
                sentences.extend(refresh_hint());
            }
            StatusKind::Done(ToggleOutcome::NotChanged(NotChanged::InvalidAlias)) => {
                sentences.push(t_args("zc-plugins-toggle-invalid-alias", &args));
            }
            StatusKind::Done(ToggleOutcome::NotChanged(NotChanged::Failed(failure))) => {
                let detail = failure.detail();
                let args = [("alias", alias.as_str()), ("error", detail.as_str())];
                let key = match failure {
                    CallFailure::Forbidden(_) => "zc-plugins-toggle-forbidden",
                    CallFailure::TimedOut | CallFailure::Other(_) => "zc-plugins-toggle-failed",
                };
                sentences.push(t_args(key, &args));
            }
            StatusKind::Done(ToggleOutcome::Unconfirmed { failure, .. }) => {
                let detail = failure.detail();
                sentences.push(t_args(
                    "zc-plugins-toggle-unconfirmed",
                    &[("alias", alias.as_str()), ("error", detail.as_str())],
                ));
                sentences.extend(refresh_hint());
            }
        }
        sentences.join(" ")
    }
}

/// Read one alias's declaration back, `None` when the daemon lists no such
/// alias. The prefix ends at the alias, and the daemon matches prefixes on
/// whole path segments, so a longer alias that starts the same is never
/// mistaken for this one.
async fn read_instance(
    rpc: &RpcClient,
    alias: &str,
) -> Result<Option<ChannelInstance>, CallFailure> {
    let prefix = format!("{INSTANCE_PREFIX}.{alias}");
    let entries = rpc
        .config_list(Some(&prefix))
        .await
        .map_err(|err| CallFailure::from_call(&err))?;
    Ok(parse_instances(&entries)
        .into_iter()
        .find(|instance| instance.alias == alias))
}

/// Flip one instance's `enabled` setting: re-read it and stop if it changed
/// since the pane read it, write the opposite of the shown value as a JSON
/// bool, then read back what the daemon stored. `config/set` creates a
/// missing alias instead of failing, so the re-read is what keeps a toggle
/// from re-creating a declaration removed before it ran; the daemon has no
/// conditional write, so a removal landing between the re-read and the write
/// is still re-created. An alias outside the alias grammar is never written.
async fn run_toggle(rpc: Arc<RpcClient>, target: ToggleTarget) -> ToggleOutcome {
    if !is_grammar_alias(&target.alias) {
        return ToggleOutcome::NotChanged(NotChanged::InvalidAlias);
    }
    let current = match read_instance(&rpc, &target.alias).await {
        Ok(current) => current,
        Err(failure) => return ToggleOutcome::NotChanged(NotChanged::Failed(failure)),
    };
    if !target.still_matches(current.as_ref()) {
        return ToggleOutcome::NotChanged(NotChanged::Stale);
    }
    let prop = format!("{INSTANCE_PREFIX}.{}.enabled", target.alias);
    let failure = match rpc
        .config_set(&prop, serde_json::Value::Bool(!target.shown))
        .await
    {
        Ok(()) => {
            let stored = read_instance(&rpc, &target.alias).await;
            return ToggleOutcome::Saved(stored.ok().flatten().and_then(|i| i.enabled));
        }
        Err(err) => CallFailure::from_call(&err),
    };
    match failure {
        // The daemon checks write authority before it stages anything.
        CallFailure::Forbidden(_) => ToggleOutcome::NotChanged(NotChanged::Failed(failure)),
        // The daemon may still be waiting for its config lock, so a read now
        // could show the old value of a write that lands later.
        CallFailure::TimedOut => ToggleOutcome::Unconfirmed {
            failure,
            stored: None,
        },
        // Other failures include a dropped connection, so the read-back
        // decides whether the value is known to be unchanged.
        CallFailure::Other(_) => match read_instance(&rpc, &target.alias).await {
            Ok(Some(stored)) if stored.enabled == Some(target.shown) => {
                ToggleOutcome::NotChanged(NotChanged::Failed(failure))
            }
            Ok(Some(stored)) => ToggleOutcome::Unconfirmed {
                failure,
                stored: stored.enabled,
            },
            Ok(None) | Err(_) => ToggleOutcome::Unconfirmed {
                failure,
                stored: None,
            },
        },
    }
}

// ── Filters and projection ───────────────────────────────────────

/// Source filter. Filters only hide rows; they never reorder or merge them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogFilter {
    All,
    Installed,
    Registry,
}

const FILTERS: [CatalogFilter; 3] = [
    CatalogFilter::All,
    CatalogFilter::Installed,
    CatalogFilter::Registry,
];

impl CatalogFilter {
    fn allows(self, entry: &PluginCatalogEntry) -> bool {
        match self {
            Self::All => true,
            Self::Installed => entry.installed.is_some(),
            Self::Registry => entry.available.is_some(),
        }
    }

    fn fluent_key(self) -> &'static str {
        match self {
            Self::All => "zc-plugins-filter-all",
            Self::Installed => "zc-plugins-filter-installed",
            Self::Registry => "zc-plugins-filter-registry",
        }
    }

    fn count(self, plugins: &[PluginCatalogEntry]) -> usize {
        plugins.iter().filter(|entry| self.allows(entry)).count()
    }

    /// Whether the rows this filter shows depend on a source the daemon could
    /// not read, so an empty or short list says nothing about that source.
    fn is_unknown(self, unreadable: Unreadable) -> bool {
        match self {
            Self::All => false,
            Self::Installed => unreadable.installed,
            Self::Registry => unreadable.registry,
        }
    }

    /// The filter row with its count. A filter over an unreadable source has
    /// no count to show, and the total is only a lower bound while any source
    /// is unreadable.
    fn counted_label(self, plugins: &[PluginCatalogEntry], unreadable: Unreadable) -> String {
        let label = t(self.fluent_key());
        let count = self.count(plugins).to_string();
        let args = [("label", label.as_str()), ("count", count.as_str())];
        if self.is_unknown(unreadable) {
            t_args("zc-plugins-filter-count-unknown", &args)
        } else if self == Self::All && unreadable.any() {
            t_args("zc-plugins-filter-count-at-least", &args)
        } else {
            t_args("zc-plugins-filter-count", &args)
        }
    }
}

/// Catalog sources the daemon reported it could not read. A record missing
/// from an unreadable source is unknown, not absent: the daemon answers such
/// a source as empty and reports it only through an issue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Unreadable {
    installed: bool,
    registry: bool,
}

impl Unreadable {
    /// Keyed on the source alone, so an issue code this build does not know
    /// still marks its source; an unknown source marks both.
    fn from_issues(issues: &[PluginCatalogIssue]) -> Self {
        issues
            .iter()
            .fold(Self::default(), |sources, issue| match issue.source {
                PluginCatalogIssueSource::Installed => Self {
                    installed: true,
                    ..sources
                },
                PluginCatalogIssueSource::Registry => Self {
                    registry: true,
                    ..sources
                },
                PluginCatalogIssueSource::Unknown => Self {
                    installed: true,
                    registry: true,
                },
            })
    }

    fn any(self) -> bool {
        self.installed || self.registry
    }
}

/// Display-safe projection of one package row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageRow {
    has_installed: bool,
    /// No installed record, but the installed packages could not be read.
    installed_unknown: bool,
    name: String,
    versions: String,
}

/// Display-safe projection of one package's detail. The installed and
/// registry records stay in their own sections; their capability lists are
/// never combined.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageDetail {
    name: String,
    installed: Option<InstalledView>,
    registry: Option<RegistryView>,
    /// A missing record whose source could not be read: unknown, not absent.
    installed_unknown: bool,
    registry_unknown: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstalledView {
    version: String,
    description: Option<String>,
    capabilities: Vec<String>,
    permissions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegistryView {
    version: String,
    description: Option<String>,
    capabilities: Vec<String>,
    install_source: String,
}

fn safe_list(items: &[String]) -> Vec<String> {
    let mut shown: Vec<String> = items
        .iter()
        .take(MAX_LIST_ITEMS)
        .map(|item| display_safe(item))
        .collect();
    if items.len() > MAX_LIST_ITEMS {
        let rest = (items.len() - MAX_LIST_ITEMS).to_string();
        shown.push(t_args(
            "zc-plugins-detail-more",
            &[("count", rest.as_str())],
        ));
    }
    shown
}

/// A description that is empty once sanitized reads as "none provided".
fn safe_description(description: Option<&str>) -> Option<String> {
    description
        .map(display_safe)
        .filter(|description| !description.is_empty())
}

/// Quote an untrusted name or version with every character [`display_safe`]
/// would change escaped: newline, carriage return and tab as `\n`, `\r` and
/// `\t`, every other whitespace character (the plain space included), every
/// hidden one, and every character Rust's debug escaping would not print
/// as is (unassigned, private-use and combining code points) as `\u{..}`, and
/// the backslash and quotes themselves. The escaping is one-to-one and leaves
/// only printable, non-space characters, so it needs no second sanitizing
/// pass that could collapse two strings into one.
fn escape_untrusted(raw: &str) -> Vec<char> {
    let mut out = vec!['"'];
    for c in raw.chars() {
        match c {
            '\n' | '\r' | '\t' | '\\' | '"' | '\'' => out.extend(c.escape_debug()),
            c if c.is_whitespace() || is_hidden(c) || c.escape_debug().len() > 1 => {
                out.extend(c.escape_unicode());
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A package name as shown in the list, the panel title and the detail. A
/// name that fits the package-name grammar is shown as is; it needs no
/// sanitizing. Any other name (the cached registry is not validated) is
/// shown escaped and quoted, capped at [`MAX_DISPLAY_CHARS`], so a name like
/// `"calendar "` never reads as the installed `calendar`. No valid name
/// starts with a quote, so the two forms never collide.
fn display_name(raw: &str) -> String {
    // The grammar caps names at this length, and checking it first keeps the
    // validator from copying an oversized invalid name into its error.
    const MAX_PACKAGE_NAME_BYTES: usize = 128;
    if raw.len() <= MAX_PACKAGE_NAME_BYTES
        && zeroclaw_api::plugin::validate_plugin_package_name(raw).is_ok()
    {
        return raw.to_string();
    }
    quoted(raw)
}

/// A version or `name@version` install identity as shown in the row and the
/// detail. The row interpolates versions into a sentence, and an identity
/// names a package, so only an allowlisted token is shown as is: non-empty
/// printable ASCII other than a quote or backslash, within
/// [`MAX_DISPLAY_CHARS`], which covers SemVer versions and valid identities.
/// Anything else is shown escaped and quoted, so an unvalidated registry value
/// can never add words such as "installed" to the row, pass for another
/// package's identity, or look like the quoted form of a different value.
fn display_token(raw: &str) -> String {
    let token = !raw.is_empty()
        && raw.len() <= MAX_DISPLAY_CHARS
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\'));
    if token { raw.to_string() } else { quoted(raw) }
}

/// [`escape_untrusted`] capped at [`MAX_DISPLAY_CHARS`], ending in an
/// ellipsis when cut. The input is cut to the cap before it is escaped.
fn quoted(raw: &str) -> String {
    let head: String = raw.chars().take(MAX_DISPLAY_CHARS).collect();
    let escaped = escape_untrusted(&head);
    if head.len() < raw.len() || escaped.len() > MAX_DISPLAY_CHARS {
        let mut text: String = escaped.into_iter().take(MAX_DISPLAY_CHARS - 1).collect();
        text.push('\u{2026}');
        text
    } else {
        escaped.into_iter().collect()
    }
}

/// The versions column. The wording follows exact equality of the daemon's
/// strings, so "same version" is never shown for different values. Versions
/// are never ordered, so the pane never implies that one is newer or that an
/// upgrade exists.
fn versions_text(entry: &PluginCatalogEntry) -> String {
    match (&entry.installed, &entry.available) {
        (Some(installed), Some(available)) if installed.version == available.version => t_args(
            "zc-plugins-row-same-version",
            &[("version", &display_token(&installed.version))],
        ),
        (Some(installed), Some(available)) => t_args(
            "zc-plugins-row-other-version",
            &[
                ("installed", &display_token(&installed.version)),
                ("registry", &display_token(&available.version)),
            ],
        ),
        (Some(installed), None) => t_args(
            "zc-plugins-row-installed-only",
            &[("version", &display_token(&installed.version))],
        ),
        (None, Some(available)) => t_args(
            "zc-plugins-row-registry-only",
            &[("version", &display_token(&available.version))],
        ),
        (None, None) => t("zc-plugins-row-no-record"),
    }
}

fn project_row(entry: &PluginCatalogEntry, unreadable: Unreadable) -> PackageRow {
    PackageRow {
        has_installed: entry.installed.is_some(),
        installed_unknown: entry.installed.is_none() && unreadable.installed,
        name: display_name(&entry.name),
        versions: versions_text(entry),
    }
}

fn project_detail(entry: &PluginCatalogEntry, unreadable: Unreadable) -> PackageDetail {
    PackageDetail {
        name: display_name(&entry.name),
        installed_unknown: entry.installed.is_none() && unreadable.installed,
        registry_unknown: entry.available.is_none() && unreadable.registry,
        installed: entry.installed.as_ref().map(|installed| InstalledView {
            version: display_token(&installed.version),
            description: safe_description(installed.description.as_deref()),
            capabilities: safe_list(&installed.capabilities),
            permissions: safe_list(&installed.permissions),
        }),
        registry: entry.available.as_ref().map(|available| RegistryView {
            version: display_token(&available.version),
            description: safe_description(available.description.as_deref()),
            capabilities: safe_list(&available.capabilities),
            install_source: display_token(&available.install_source),
        }),
    }
}

/// Cells the marker and name keep when the versions column takes the rest
/// of the row: the marker, a space, four name cells and an ellipsis.
const MIN_LEAD: usize = 7;

/// Fit a package row into `width` cells: the marker and name first, then the
/// versions column. The versions take priority, since the two versions of a
/// package are what its row exists to tell apart and the detail repeats the
/// full name: the name gives up room down to [`MIN_LEAD`] cells so the
/// versions fit whole. Only when the versions alone are too wide does the
/// name keep half the row and the versions get cut. Cut parts end in an
/// ellipsis, and truncation is grapheme and wide-character safe.
///
/// The marker is filled for a package with an installed record, hollow for
/// one known to be only in the cached registry, and a neutral `?` when it has
/// no installed record but the installed packages could not be read.
fn fit_row(row: &PackageRow, width: usize) -> (String, String) {
    let marker = if row.has_installed {
        "\u{25cf}"
    } else if row.installed_unknown {
        "?"
    } else {
        "\u{25cb}"
    };
    let lead_cap = if row.versions.is_empty() {
        width
    } else {
        let reserve = crate::display_width::display_width(&row.versions) + 2;
        if reserve + MIN_LEAD <= width {
            width - reserve
        } else {
            width / 2
        }
    };
    let lead = crate::widgets::truncate_to_width(&format!("{marker} {}", row.name), lead_cap);
    let room = width.saturating_sub(crate::display_width::display_width(&lead));
    let versions = if room > 2 && !row.versions.is_empty() {
        format!(
            "  {}",
            crate::widgets::truncate_to_width(&row.versions, room - 2)
        )
    } else {
        String::new()
    };
    (lead, versions)
}

fn issue_text(issue: &PluginCatalogIssue) -> String {
    match (issue.source, issue.code) {
        (PluginCatalogIssueSource::Installed, PluginCatalogIssueCode::DiscoveryFailed) => {
            t("zc-plugins-issue-installed")
        }
        (PluginCatalogIssueSource::Registry, PluginCatalogIssueCode::CacheReadFailed) => {
            t("zc-plugins-issue-registry")
        }
        _ => t("zc-plugins-issue-unknown"),
    }
}

fn join_list(items: &[String]) -> String {
    if items.is_empty() {
        t("zc-plugins-detail-none")
    } else {
        items.join(", ")
    }
}

fn detail_item(text: String) -> Line<'static> {
    Line::from(Span::styled(format!("  {text}"), theme::body_style()))
}

fn description_item(description: Option<&str>) -> Line<'static> {
    match description {
        Some(description) => detail_item(t_args(
            "zc-plugins-detail-description",
            &[("description", description)],
        )),
        None => Line::from(Span::styled(
            format!("  {}", t("zc-plugins-detail-no-description")),
            theme::dim_style(),
        )),
    }
}

fn detail_lines(detail: &PackageDetail) -> Vec<Line<'static>> {
    let heading = |key: &str| Line::from(Span::styled(t(key), theme::heading_style()));
    let absent = |key: &str| Line::from(Span::styled(format!("  {}", t(key)), theme::dim_style()));

    // The full name, since the list row and the panel title may cut it.
    let mut lines = vec![
        Line::from(Span::styled(
            t_args("zc-plugins-detail-name", &[("name", &detail.name)]),
            theme::body_style(),
        )),
        Line::from(""),
        heading("zc-plugins-detail-installed"),
    ];
    match &detail.installed {
        Some(installed) => {
            lines.push(detail_item(t_args(
                "zc-plugins-detail-version",
                &[("version", &installed.version)],
            )));
            lines.push(description_item(installed.description.as_deref()));
            lines.push(detail_item(t_args(
                "zc-plugins-detail-capabilities",
                &[("list", &join_list(&installed.capabilities))],
            )));
            lines.push(detail_item(t_args(
                "zc-plugins-detail-permissions",
                &[("list", &join_list(&installed.permissions))],
            )));
        }
        None if detail.installed_unknown => {
            lines.push(absent("zc-plugins-detail-installed-unknown"))
        }
        None => lines.push(absent("zc-plugins-detail-not-installed")),
    }
    lines.push(Line::from(""));
    lines.push(heading("zc-plugins-detail-registry"));
    match &detail.registry {
        Some(registry) => {
            lines.push(detail_item(t_args(
                "zc-plugins-detail-version",
                &[("version", &registry.version)],
            )));
            lines.push(description_item(registry.description.as_deref()));
            lines.push(detail_item(t_args(
                "zc-plugins-detail-capabilities",
                &[("list", &join_list(&registry.capabilities))],
            )));
            lines.push(detail_item(t_args(
                "zc-plugins-detail-install-identity",
                &[("identity", &registry.install_source)],
            )));
        }
        None if detail.registry_unknown => lines.push(absent("zc-plugins-detail-registry-unknown")),
        None => lines.push(absent("zc-plugins-detail-not-in-registry")),
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        t("zc-plugins-detail-footnote"),
        theme::dim_style(),
    )));
    lines
}

/// Source issues, then host facts as label and value rows so each label fits
/// the narrow left column. The issues come first because the block does not
/// scroll: on a short terminal a long plugin directory is what gets cut, never
/// an issue the right pane points at. `[plugins].enabled` is shown as
/// configuration intent only.
fn host_lines(data: &PluginsListResult) -> Vec<Line<'static>> {
    let label = |key: &str| Line::from(Span::styled(t(key), theme::dim_style()));
    let value = |text: String| Line::from(Span::styled(format!("  {text}"), theme::body_style()));
    let wasm = if data.wasm_plugins_available {
        t("zc-plugins-host-wasm-built-in")
    } else {
        t("zc-plugins-host-wasm-missing")
    };
    let enabled = if data.plugins_enabled {
        t("zc-plugins-yes")
    } else {
        t("zc-plugins-no")
    };
    let mut lines: Vec<Line<'static>> = data
        .issues
        .iter()
        .map(|issue| Line::from(Span::styled(issue_text(issue), theme::warn_style())))
        .collect();
    if !lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines.extend([
        label("zc-plugins-host-wasm-label"),
        value(wasm),
        label("zc-plugins-host-enabled-label"),
        value(enabled),
        label("zc-plugins-host-dir-label"),
        value(display_safe(&data.plugins_dir)),
    ]);
    lines
}

/// Display labels for an action's live chords.
fn chord_labels<A: crate::keymap::RebindableActions>(actions: &[A]) -> Vec<String> {
    actions
        .iter()
        .flat_map(|action| crate::keymap::action_key_labels(*action))
        .collect()
}

fn first_chord<A: crate::keymap::RebindableActions>(action: A) -> String {
    crate::keymap::action_key_labels(action)
        .into_iter()
        .next()
        .unwrap_or_default()
}

// ── Pane ─────────────────────────────────────────────────────────

/// Which list the cursor drives: the filter list on the left, the package
/// list on the right, the package detail that replaces it, or the channel
/// instance list under that detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Filters,
    Packages,
    Detail,
    Instances,
}

/// What the Config manager does after the pane saw a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginsKeyOutcome {
    Consumed,
    /// Back at the filter list: the manager crosses to the previous sub-tab.
    NotConsumed,
    /// The refresh chord: the manager starts a fetch with its live client.
    RefreshRequested,
    /// Enter on a channel instance: the manager starts the toggle the pane
    /// holds, with its live client.
    ToggleRequested,
}

/// Display-safe content of one package's channel instance block.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstanceBlock {
    /// Alias and config state of each instance naming the package.
    rows: Vec<(String, Option<bool>)>,
    /// Why the instances could not be read, shown instead of rows.
    error: Option<String>,
    /// The latest toggle status for this package, and whether it warns.
    status: Option<(String, bool)>,
}

pub(crate) struct PluginsPane {
    data: Option<PluginsListResult>,
    error: Option<CatalogError>,
    /// A fetch in flight. Loading is exactly "a task is present".
    refresh_task: Option<CatalogFetch>,
    instances: InstanceState,
    /// Selection in the open package's instance list.
    instance_state: ListState,
    /// The toggle Enter asked for, until the manager starts it.
    pending_toggle: Option<ToggleTarget>,
    /// The toggle in flight, if any. At most one runs at a time.
    toggle_task: Option<(ToggleTarget, ToggleTask)>,
    /// Kept until the next toggle or refresh.
    toggle_status: Option<ToggleStatus>,
    focus: Focus,
    filter: CatalogFilter,
    list_state: ListState,
    detail_scroll: u16,
    last_filter_area: Option<Rect>,
    /// Scroll offset of the filter list at last draw; it scrolls only when
    /// the terminal is too short to show all three rows.
    last_filter_offset: usize,
    last_list_area: Option<Rect>,
    last_detail_area: Option<Rect>,
    last_instances_area: Option<Rect>,
    double_click: crate::mouse::DoubleClickTracker,
}

impl PluginsPane {
    /// No RPC here: the catalog loads on first entry into the sub-tab.
    pub(crate) fn new() -> Self {
        Self {
            data: None,
            error: None,
            refresh_task: None,
            instances: InstanceState::NotLoaded,
            instance_state: ListState::default(),
            pending_toggle: None,
            toggle_task: None,
            toggle_status: None,
            focus: Focus::Filters,
            filter: CatalogFilter::All,
            list_state: ListState::default(),
            detail_scroll: 0,
            last_filter_area: None,
            last_filter_offset: 0,
            last_list_area: None,
            last_detail_area: None,
            last_instances_area: None,
            double_click: crate::mouse::DoubleClickTracker::new(),
        }
    }

    // ── Fetch lifecycle ──────────────────────────────────────────

    /// Start the first fetch: only when nothing is loaded, no error is held
    /// and none is running. A held error waits for an explicit refresh.
    pub(crate) fn refresh_if_inactive(&mut self, rpc: &Arc<RpcClient>) {
        if self.data.is_none() && self.error.is_none() && !self.is_busy() {
            self.start_refresh(rpc);
        }
    }

    /// Refetch on request. Ignored while a fetch or a toggle is running, so
    /// a read that started before a write can never land after it.
    pub(crate) fn refresh(&mut self, rpc: &Arc<RpcClient>) {
        if !self.is_busy() {
            self.start_refresh(rpc);
        }
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.refresh_task.is_some()
    }

    pub(crate) fn is_toggling(&self) -> bool {
        self.toggle_task.is_some()
    }

    fn is_busy(&self) -> bool {
        self.is_loading() || self.is_toggling()
    }

    /// One task reads the catalog, then the channel instances. The two are
    /// independent: an instance read failure is kept beside a loaded catalog.
    /// A catalog that fails has no package to show instances under, so the
    /// instances are not read.
    fn start_refresh(&mut self, rpc: &Arc<RpcClient>) {
        // Loaded data stays on screen, marked as refreshing, until the result
        // lands; a held error gives way to the loading state.
        self.error = None;
        self.toggle_status = None;
        let rpc = Arc::clone(rpc);
        self.refresh_task = Some(tokio::spawn(async move {
            let catalog = rpc
                .plugins_list()
                .await
                .map_err(|err| CatalogError::from_call(&err));
            let instances = match &catalog {
                Ok(_) => Some(
                    rpc.config_list(Some(INSTANCE_PREFIX))
                        .await
                        .map(|entries| parse_instances(&entries))
                        .map_err(|err| CallFailure::from_call(&err)),
                ),
                Err(_) => None,
            };
            FetchResult { catalog, instances }
        }));
    }

    /// Apply a finished fetch. Never waits on one still in flight, so the
    /// draw loop is never blocked.
    pub(crate) async fn poll_refresh(&mut self) {
        let Some(task) = self.refresh_task.take_if(|task| task.is_finished()) else {
            return;
        };
        let fetch = match task.await {
            Ok(fetch) => fetch,
            Err(join_error) => FetchResult {
                catalog: Err(CatalogError::Other(display_safe(&format!(
                    "plugin catalog request task failed: {join_error}"
                )))),
                instances: None,
            },
        };
        self.apply_fetch(fetch);
    }

    /// The instances replace the earlier ones before the catalog lands, so
    /// the focus checks after it see the new list. The instance cursor
    /// follows its alias, as the package cursor follows its name.
    fn apply_fetch(&mut self, fetch: FetchResult) {
        let anchor = self
            .selected_instance()
            .map(|instance| instance.alias.clone());
        self.instances = match fetch.instances {
            None => InstanceState::NotLoaded,
            Some(Ok(instances)) => InstanceState::Loaded(instances),
            Some(Err(failure)) => InstanceState::Failed(failure),
        };
        self.apply_result(fetch.catalog);
        if let Some(row) = anchor.and_then(|alias| {
            self.selected_instances()
                .iter()
                .position(|instance| instance.alias == alias)
        }) {
            self.instance_state.select(Some(row));
        }
        self.clamp_instances();
    }

    // ── Toggle lifecycle ─────────────────────────────────────────

    /// Start the toggle Enter asked for. Ignored while a fetch or another
    /// toggle runs, so at most one write is ever in flight.
    pub(crate) fn start_toggle(&mut self, rpc: &Arc<RpcClient>) {
        let Some(target) = self.pending_toggle.take() else {
            return;
        };
        if self.is_busy() {
            return;
        }
        self.toggle_status = Some(ToggleStatus {
            package: target.package.clone(),
            alias: target.alias.clone(),
            kind: StatusKind::Saving,
        });
        let task = tokio::spawn(run_toggle(Arc::clone(rpc), target.clone()));
        self.toggle_task = Some((target, task));
    }

    /// Apply a finished toggle without waiting on one still in flight.
    pub(crate) async fn poll_toggle(&mut self) {
        let Some((target, task)) = self.toggle_task.take_if(|(_, task)| task.is_finished()) else {
            return;
        };
        let outcome = match task.await {
            Ok(outcome) => outcome,
            Err(join_error) => ToggleOutcome::Unconfirmed {
                failure: CallFailure::Other(display_safe(&format!(
                    "channel instance toggle task failed: {join_error}"
                ))),
                stored: None,
            },
        };
        self.apply_toggle(&target, outcome);
    }

    /// Show the value the daemon reported: the one read back after a write,
    /// unknown when that could not be read, and unchanged when nothing was
    /// written.
    fn apply_toggle(&mut self, target: &ToggleTarget, outcome: ToggleOutcome) {
        let value = match &outcome {
            ToggleOutcome::Saved(stored) => Some(*stored),
            ToggleOutcome::Unconfirmed { stored, .. } => Some(*stored),
            ToggleOutcome::NotChanged(_) => None,
        };
        if let (Some(value), InstanceState::Loaded(instances)) = (value, &mut self.instances)
            && let Some(instance) = instances.iter_mut().find(|instance| {
                instance.alias == target.alias
                    && instance.package.as_deref() == Some(target.package.as_str())
            })
        {
            instance.enabled = value;
        }
        self.toggle_status = Some(ToggleStatus {
            package: target.package.clone(),
            alias: target.alias.clone(),
            kind: StatusKind::Done(outcome),
        });
    }

    /// Enter on the selected instance: hold its toggle for the manager to
    /// start. An instance whose value is unknown has nothing to flip, and one
    /// whose alias is outside the alias grammar is never written, so each only
    /// gets a status line.
    fn request_toggle(&mut self) -> PluginsKeyOutcome {
        if self.is_busy() {
            return PluginsKeyOutcome::Consumed;
        }
        let (Some(entry), Some(instance)) = (self.selected_entry(), self.selected_instance())
        else {
            return PluginsKeyOutcome::Consumed;
        };
        let package = entry.name.clone();
        let alias = instance.alias.clone();
        if !is_grammar_alias(&alias) {
            self.toggle_status = Some(ToggleStatus {
                package,
                alias,
                kind: StatusKind::Done(ToggleOutcome::NotChanged(NotChanged::InvalidAlias)),
            });
            return PluginsKeyOutcome::Consumed;
        }
        match instance.enabled {
            Some(shown) => {
                self.pending_toggle = Some(ToggleTarget {
                    alias,
                    package,
                    shown,
                });
                PluginsKeyOutcome::ToggleRequested
            }
            None => {
                self.toggle_status = Some(ToggleStatus {
                    package,
                    alias,
                    kind: StatusKind::UnknownValue,
                });
                PluginsKeyOutcome::Consumed
            }
        }
    }

    /// A failure replaces any earlier data, so stale rows are never shown as
    /// current after a failed refresh. On success the cursor follows the
    /// selected package by name (row indices mean nothing across a refresh);
    /// an open detail whose package is gone closes rather than silently
    /// showing whichever package now sits at the same row.
    fn apply_result(&mut self, result: Result<PluginsListResult, CatalogError>) {
        let anchor = self.selected_entry().map(|entry| entry.name.clone());
        match result {
            Ok(data) => {
                self.data = Some(data);
                self.error = None;
            }
            Err(error) => {
                self.data = None;
                self.error = Some(error);
            }
        }
        let plugins = self.plugins();
        let row = anchor.and_then(|name| {
            self.visible_indices()
                .iter()
                .position(|idx| plugins[*idx].name == name)
        });
        self.reselect(row);
    }

    /// Select `row` when the previously selected package is still visible
    /// there, keeping an open detail and its scroll. Otherwise clamp the
    /// cursor, reset the detail scroll and leave the detail view. Either way,
    /// focus returns to the filters when the right pane no longer draws a
    /// package list, so it never rests on a list that is not on screen.
    fn reselect(&mut self, row: Option<usize>) {
        match row {
            Some(row) => self.list_state.select(Some(row)),
            None => {
                self.detail_scroll = 0;
                if self.detail_open() {
                    self.focus = Focus::Packages;
                }
                self.clamp_selection();
            }
        }
        if !self.packages_shown() && self.focus != Focus::Filters {
            self.focus = Focus::Filters;
            self.detail_scroll = 0;
        }
        self.clamp_instances();
    }

    /// Whether the right pane shows a package detail: focus is on it or on
    /// the instance list under it.
    fn detail_open(&self) -> bool {
        matches!(self.focus, Focus::Detail | Focus::Instances)
    }

    /// Whether the right pane draws a package list (or a detail opened from
    /// it): a catalog is loaded, the daemon has WASM plugin support, and the
    /// active filter shows at least one row. In every other state the right
    /// pane shows a message, and focus stays on the filters.
    fn packages_shown(&self) -> bool {
        self.error.is_none()
            && self
                .data
                .as_ref()
                .is_some_and(|data| data.wasm_plugins_available)
            && !self.visible_indices().is_empty()
    }

    // ── Selection ────────────────────────────────────────────────

    fn unreadable(&self) -> Unreadable {
        self.data
            .as_ref()
            .map(|data| Unreadable::from_issues(&data.issues))
            .unwrap_or_default()
    }

    fn plugins(&self) -> &[PluginCatalogEntry] {
        self.data
            .as_ref()
            .map(|data| data.plugins.as_slice())
            .unwrap_or_default()
    }

    /// Daemon-order indices of the rows the active filter shows.
    fn visible_indices(&self) -> Vec<usize> {
        self.plugins()
            .iter()
            .enumerate()
            .filter(|(_, entry)| self.filter.allows(entry))
            .map(|(idx, _)| idx)
            .collect()
    }

    fn selected_entry(&self) -> Option<&PluginCatalogEntry> {
        let row = self.list_state.selected()?;
        let idx = *self.visible_indices().get(row)?;
        self.plugins().get(idx)
    }

    fn clamp_selection(&mut self) {
        let len = self.visible_indices().len();
        match (len, self.list_state.selected()) {
            (0, _) => self.list_state.select(None),
            (_, None) => self.list_state.select(Some(0)),
            (len, Some(row)) if row >= len => self.list_state.select(Some(len - 1)),
            _ => {}
        }
        if self.detail_open() && self.selected_entry().is_none() {
            self.focus = Focus::Packages;
        }
    }

    // ── Channel instances ────────────────────────────────────────

    /// The loaded instances whose `package` is exactly `package`. Raw string
    /// equality: an instance naming a lookalike package is not shown here.
    fn instances_of(&self, package: &str) -> Vec<&ChannelInstance> {
        match &self.instances {
            InstanceState::Loaded(instances) => instances
                .iter()
                .filter(|instance| instance.package.as_deref() == Some(package))
                .collect(),
            InstanceState::NotLoaded | InstanceState::Failed(_) => Vec::new(),
        }
    }

    fn selected_instances(&self) -> Vec<&ChannelInstance> {
        self.selected_entry()
            .map(|entry| self.instances_of(&entry.name))
            .unwrap_or_default()
    }

    fn selected_instance(&self) -> Option<&ChannelInstance> {
        let row = self.instance_state.selected()?;
        self.selected_instances().get(row).copied()
    }

    /// Whether a package's detail shows the instance block: its installed
    /// record declares the channel capability, or a configured instance
    /// names it. A failed read shows its error in the block of every
    /// installed channel package; nothing shows before the first read.
    fn shows_instance_block(&self, entry: &PluginCatalogEntry) -> bool {
        let channel = entry.installed.as_ref().is_some_and(|installed| {
            installed
                .capabilities
                .iter()
                .any(|capability| capability == CHANNEL_CAPABILITY)
        });
        match &self.instances {
            InstanceState::NotLoaded => false,
            InstanceState::Failed(_) => channel,
            InstanceState::Loaded(_) => channel || !self.instances_of(&entry.name).is_empty(),
        }
    }

    /// Keep the instance cursor on a row, and move focus back to the detail
    /// when the open package has no instance left to select.
    fn clamp_instances(&mut self) {
        let len = self.selected_instances().len();
        match (len, self.instance_state.selected()) {
            (0, _) => self.instance_state.select(None),
            (_, None) => self.instance_state.select(Some(0)),
            (len, Some(row)) if row >= len => self.instance_state.select(Some(len - 1)),
            _ => {}
        }
        if self.focus == Focus::Instances && len == 0 {
            self.focus = Focus::Detail;
        }
    }

    fn step_instance(&mut self, delta: isize) {
        let len = self.selected_instances().len();
        if len == 0 {
            self.instance_state.select(None);
            return;
        }
        let current = self.instance_state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, len as isize - 1) as usize;
        self.instance_state.select(Some(next));
    }

    /// Enter in the detail moves into the instance list, only when the open
    /// package has an instance to select.
    fn enter_instances(&mut self) {
        if !self.selected_instances().is_empty() {
            self.focus = Focus::Instances;
            self.clamp_instances();
        }
    }

    /// The display-safe instance block of `entry`, or `None` when its detail
    /// shows none.
    fn instance_block(&self, entry: &PluginCatalogEntry) -> Option<InstanceBlock> {
        if !self.shows_instance_block(entry) {
            return None;
        }
        let rows = self
            .instances_of(&entry.name)
            .into_iter()
            .map(|instance| (display_alias(&instance.alias), instance.enabled))
            .collect();
        let error = match &self.instances {
            InstanceState::Failed(failure) => Some(failure.instances_message()),
            InstanceState::NotLoaded | InstanceState::Loaded(_) => None,
        };
        let status = self
            .toggle_status
            .as_ref()
            .filter(|status| status.package == entry.name)
            .map(|status| (status.text(), status.is_warning()));
        Some(InstanceBlock {
            rows,
            error,
            status,
        })
    }

    /// The data is unchanged, so the cursor follows the selected package by
    /// its daemon index into the new filter when that package stays visible.
    fn set_filter(&mut self, filter: CatalogFilter) {
        if self.filter != filter {
            let anchor = self
                .list_state
                .selected()
                .and_then(|row| self.visible_indices().get(row).copied());
            self.filter = filter;
            let row = anchor.and_then(|idx| self.visible_indices().iter().position(|i| *i == idx));
            self.reselect(row);
        }
    }

    fn step_filter(&mut self, delta: isize) {
        let current = FILTERS
            .iter()
            .position(|filter| *filter == self.filter)
            .unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, FILTERS.len() as isize - 1) as usize;
        self.set_filter(FILTERS[next]);
    }

    fn step_selection(&mut self, delta: isize) {
        let len = self.visible_indices().len();
        if len == 0 {
            self.list_state.select(None);
            return;
        }
        let current = self.list_state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, len as isize - 1) as usize;
        if self.list_state.selected() != Some(next) {
            self.detail_scroll = 0;
        }
        self.list_state.select(Some(next));
    }

    fn scroll_detail(&mut self, delta: i32) {
        self.detail_scroll = if delta < 0 {
            self.detail_scroll
                .saturating_sub(delta.unsigned_abs() as u16)
        } else {
            self.detail_scroll.saturating_add(delta as u16)
        };
    }

    fn open_detail(&mut self) {
        if self.packages_shown() && self.selected_entry().is_some() {
            self.focus = Focus::Detail;
            self.detail_scroll = 0;
            self.instance_state.select(None);
            self.clamp_instances();
        }
    }

    // ── Keys ─────────────────────────────────────────────────────

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> PluginsKeyOutcome {
        use crate::keymap::ConfigTabAction as A;
        let Some(action) = A::from_chord(&key) else {
            return PluginsKeyOutcome::Consumed;
        };
        match action {
            A::Refresh => return PluginsKeyOutcome::RefreshRequested,
            A::Up => match self.focus {
                Focus::Filters => self.step_filter(-1),
                Focus::Packages => self.step_selection(-1),
                Focus::Detail => self.scroll_detail(-1),
                Focus::Instances => self.step_instance(-1),
            },
            A::Down => match self.focus {
                Focus::Filters => self.step_filter(1),
                Focus::Packages => self.step_selection(1),
                Focus::Detail => self.scroll_detail(1),
                Focus::Instances => self.step_instance(1),
            },
            // Only Enter toggles; the inward chord never writes config.
            A::Enter if self.focus == Focus::Instances => return self.request_toggle(),
            A::Enter | A::TabRight => match self.focus {
                Focus::Filters if self.packages_shown() => self.focus = Focus::Packages,
                Focus::Filters | Focus::Instances => {}
                Focus::Packages => self.open_detail(),
                Focus::Detail => self.enter_instances(),
            },
            A::Back | A::TabLeft => match self.focus {
                Focus::Filters => return PluginsKeyOutcome::NotConsumed,
                Focus::Packages => self.focus = Focus::Filters,
                Focus::Detail => self.focus = Focus::Packages,
                Focus::Instances => self.focus = Focus::Detail,
            },
            _ => {}
        }
        PluginsKeyOutcome::Consumed
    }

    // ── Mouse ────────────────────────────────────────────────────

    /// Clicks select a filter or a package (double-click opens its detail);
    /// the wheel moves the list under the pointer or scrolls the detail.
    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) {
        use crate::mouse::{in_rect, list_click_index};
        let (col, row) = (mouse.column, mouse.row);
        let over = |area: Option<Rect>| area.is_some_and(|area| in_rect(col, row, area));
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(area) = self.last_filter_area
                    && in_rect(col, row, area)
                {
                    self.focus = Focus::Filters;
                    if let Some(idx) =
                        list_click_index(row, area, self.last_filter_offset, FILTERS.len())
                    {
                        self.set_filter(FILTERS[idx]);
                    }
                    return;
                }
                if let Some(area) = self.last_list_area
                    && in_rect(col, row, area)
                {
                    let len = self.visible_indices().len();
                    if let Some(idx) = list_click_index(row, area, self.list_state.offset(), len) {
                        self.focus = Focus::Packages;
                        if self.list_state.selected() != Some(idx) {
                            self.detail_scroll = 0;
                        }
                        self.list_state.select(Some(idx));
                        if self.double_click.click(col, row) {
                            self.open_detail();
                        }
                    }
                    return;
                }
                // A click only selects an instance; toggling takes Enter.
                if let Some(area) = self.last_instances_area
                    && in_rect(col, row, area)
                {
                    // The instance list has no border of its own: its first
                    // row is the top of its area.
                    let idx = usize::from(row - area.y) + self.instance_state.offset();
                    if idx < self.selected_instances().len() {
                        self.focus = Focus::Instances;
                        self.instance_state.select(Some(idx));
                    }
                    return;
                }
                if over(self.last_detail_area) {
                    self.focus = Focus::Detail;
                }
            }
            MouseEventKind::ScrollDown if over(self.last_filter_area) => self.step_filter(1),
            MouseEventKind::ScrollUp if over(self.last_filter_area) => self.step_filter(-1),
            MouseEventKind::ScrollDown if over(self.last_list_area) => self.step_selection(1),
            MouseEventKind::ScrollUp if over(self.last_list_area) => self.step_selection(-1),
            MouseEventKind::ScrollDown if over(self.last_instances_area) => self.step_instance(1),
            MouseEventKind::ScrollUp if over(self.last_instances_area) => self.step_instance(-1),
            MouseEventKind::ScrollDown if over(self.last_detail_area) => {
                self.scroll_detail(i32::from(DETAIL_SCROLL_LINES));
            }
            MouseEventKind::ScrollUp if over(self.last_detail_area) => {
                self.scroll_detail(-i32::from(DETAIL_SCROLL_LINES));
            }
            _ => {}
        }
    }

    // ── Help and footer ──────────────────────────────────────────

    /// Help for the focused list, so every entry names what its keys do
    /// there: at the filters, Back leaves for the previous sub-tab; in the
    /// detail, Up/Down scroll and Enter reaches the channel instances when
    /// there are any; in the instance list, Enter toggles.
    pub(crate) fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::keymap::{ConfigTabAction as A, GlobalAction};
        use crate::widgets::{HelpEntry, HelpNode};
        let entry = |actions: &[A], key: &str| HelpEntry::new(chord_labels(actions), t(key));
        let mut entries = match self.focus {
            // Enter reaches the packages only while a list is drawn.
            Focus::Filters => [
                Some(entry(&[A::Up, A::Down], "zc-plugins-help-choose-filter")),
                self.packages_shown()
                    .then(|| entry(&[A::Enter, A::TabRight], "zc-plugins-help-show-packages")),
                Some(entry(
                    &[A::Back, A::TabLeft],
                    "zc-plugins-help-previous-subtab",
                )),
            ]
            .into_iter()
            .flatten()
            .collect(),
            Focus::Packages => vec![
                entry(&[A::Up, A::Down], "zc-plugins-help-navigate"),
                entry(&[A::Enter, A::TabRight], "zc-plugins-help-open"),
                entry(&[A::Back, A::TabLeft], "zc-plugins-help-back-to-filters"),
            ],
            Focus::Detail => [
                Some(entry(&[A::Up, A::Down], "zc-plugins-help-scroll")),
                (!self.selected_instances().is_empty())
                    .then(|| entry(&[A::Enter, A::TabRight], "zc-plugins-help-instances")),
                Some(entry(
                    &[A::Back, A::TabLeft],
                    "zc-plugins-help-back-to-packages",
                )),
            ]
            .into_iter()
            .flatten()
            .collect(),
            Focus::Instances => vec![
                entry(&[A::Up, A::Down], "zc-plugins-help-navigate"),
                entry(&[A::Enter], "zc-plugins-help-toggle"),
                entry(&[A::Back, A::TabLeft], "zc-plugins-help-back-to-detail"),
            ],
        };
        entries.extend([
            entry(&[A::Refresh], "zc-plugins-help-refresh"),
            HelpEntry::new(
                chord_labels(&[GlobalAction::Help]),
                t("zc-plugins-help-this-help"),
            ),
            HelpEntry::spacer(),
            HelpEntry::key(
                t("zc-config-help-mouse-label"),
                t("zc-config-help-mouse-open"),
            ),
        ]);
        HelpNode::entries(entries)
    }

    /// One-row action hint for the bottom of the Config pane, built from the
    /// live keymap and worded for the focused list.
    pub(crate) fn footer_hint(&self) -> String {
        use crate::keymap::{ConfigTabAction as A, GlobalAction};
        let (navigate, enter, back) = match self.focus {
            Focus::Filters => (
                "zc-plugins-footer-filter",
                self.packages_shown()
                    .then_some("zc-plugins-footer-packages"),
                "zc-plugins-footer-previous-subtab",
            ),
            Focus::Packages => (
                "zc-plugins-footer-navigate",
                Some("zc-plugins-footer-open"),
                "zc-plugins-footer-back",
            ),
            Focus::Detail => (
                "zc-plugins-footer-scroll",
                (!self.selected_instances().is_empty()).then_some("zc-plugins-footer-instances"),
                "zc-plugins-footer-back",
            ),
            Focus::Instances => (
                "zc-plugins-footer-navigate",
                Some("zc-plugins-footer-toggle"),
                "zc-plugins-footer-back",
            ),
        };
        let mut hint = format!(
            " {}={}  {}={}",
            first_chord(GlobalAction::Help),
            t("zc-plugins-footer-help"),
            chord_labels(&[A::Up, A::Down]).join("/"),
            t(navigate),
        );
        if let Some(enter) = enter {
            hint.push_str(&format!("  {}={}", first_chord(A::Enter), t(enter)));
        }
        hint.push_str(&format!(
            "  {}={}  {}={}",
            first_chord(A::Back),
            t(back),
            first_chord(A::Refresh),
            t("zc-plugins-footer-refresh"),
        ));
        hint
    }

    // ── Draw ─────────────────────────────────────────────────────

    pub(crate) fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(LEFT_COLUMN_WIDTH), Constraint::Min(0)])
            .split(area);
        let left = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(FILTERS.len() as u16 + 2),
                Constraint::Min(0),
            ])
            .split(cols[0]);

        self.last_filter_area = None;
        self.last_list_area = None;
        self.last_detail_area = None;
        self.last_instances_area = None;

        self.draw_filters(frame, left[0]);
        self.draw_host(frame, left[1]);
        self.draw_right(frame, cols[1]);
    }

    fn highlight(focused: bool) -> (ratatui::style::Style, &'static str) {
        let symbol = if focused { "\u{203a} " } else { "  " };
        (theme::selection_highlight(focused, false), symbol)
    }

    fn draw_filters(&mut self, frame: &mut Frame, area: Rect) {
        let plugins = self.plugins();
        let unreadable = self.unreadable();
        // Without WASM plugin support there is no catalog to count, so the
        // filters show bare labels, as they do while loading or after a
        // failure.
        let counted = self
            .data
            .as_ref()
            .is_some_and(|data| data.wasm_plugins_available);
        let items: Vec<ListItem> = FILTERS
            .iter()
            .map(|filter| {
                let text = if counted {
                    filter.counted_label(plugins, unreadable)
                } else {
                    t(filter.fluent_key())
                };
                ListItem::new(Line::from(Span::styled(text, theme::body_style())))
            })
            .collect();
        let mut state = ListState::default();
        state.select(FILTERS.iter().position(|filter| *filter == self.filter));
        let (style, symbol) = Self::highlight(self.focus == Focus::Filters);
        let title = format!(" {} ", t("zc-plugins-filters-title"));
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(&title))
                .highlight_style(style)
                .highlight_symbol(symbol),
            area,
            &mut state,
        );
        self.last_filter_area = Some(area);
        self.last_filter_offset = state.offset();
    }

    fn draw_host(&self, frame: &mut Frame, area: Rect) {
        let lines = self.data.as_ref().map(host_lines).unwrap_or_default();
        let title = format!(" {} ", t("zc-plugins-host-title"));
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(theme::panel_block(&title)),
            area,
        );
    }

    /// Panel title fitted inside the border corners of `area`, marked while a
    /// refresh runs over data still on screen. The marker keeps its room; only
    /// the base text is truncated.
    fn panel_title(&self, base: &str, area: Rect) -> String {
        use crate::display_width::display_width;
        use crate::widgets::truncate_to_width;
        let room = usize::from(area.width.saturating_sub(4));
        if self.is_loading() && self.data.is_some() {
            let marker = display_width(&t_args("zc-plugins-title-refreshing", &[("title", "")]));
            let fitted = truncate_to_width(base, room.saturating_sub(marker));
            let title = t_args("zc-plugins-title-refreshing", &[("title", &fitted)]);
            format!(" {} ", truncate_to_width(&title, room))
        } else {
            format!(" {} ", truncate_to_width(base, room))
        }
    }

    fn draw_message(&self, frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
        let title = self.panel_title(&t("zc-plugins-packages-title"), area);
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(theme::panel_block(&title)),
            area,
        );
    }

    fn draw_right(&mut self, frame: &mut Frame, area: Rect) {
        let body = |key: &str| Line::from(Span::styled(t(key), theme::body_style()));
        if let Some(error) = &self.error {
            let mut lines = vec![Line::from(Span::styled(
                error.message(),
                theme::warn_style(),
            ))];
            let keys = first_chord(crate::keymap::ConfigTabAction::Refresh);
            if !keys.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    t_args("zc-plugins-retry-hint", &[("keys", &keys)]),
                    theme::dim_style(),
                )));
            }
            self.draw_message(frame, area, lines);
            return;
        }
        let Some(data) = &self.data else {
            self.draw_message(frame, area, vec![body("zc-plugins-loading")]);
            return;
        };
        // A daemon without WASM plugin support has no catalog, so its state
        // wins over any rows; the host block on the left still shows why.
        if !data.wasm_plugins_available {
            self.draw_message(frame, area, vec![body("zc-plugins-no-wasm")]);
            return;
        }
        if data.plugins.is_empty() {
            let key = if data.issues.is_empty() {
                "zc-plugins-empty"
            } else {
                "zc-plugins-empty-with-issues"
            };
            self.draw_message(frame, area, vec![body(key)]);
            return;
        }
        if self.detail_open()
            && let Some(entry) = self.selected_entry()
        {
            let detail = project_detail(entry, self.unreadable());
            let block = self.instance_block(entry);
            self.draw_detail(frame, area, &detail, block.as_ref());
            return;
        }
        self.draw_list(frame, area);
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect) {
        let visible = self.visible_indices();
        let unreadable = self.unreadable();
        if visible.is_empty() {
            // An empty filter over an unreadable source says nothing about
            // what that source holds.
            let key = if self.filter.is_unknown(unreadable) {
                "zc-plugins-filter-empty-unknown"
            } else {
                "zc-plugins-filter-empty"
            };
            self.draw_message(
                frame,
                area,
                vec![Line::from(Span::styled(t(key), theme::body_style()))],
            );
            return;
        }
        let width = usize::from(area.width.saturating_sub(2)).saturating_sub(HIGHLIGHT_GUTTER);
        let plugins = self.plugins();
        let items: Vec<ListItem> = visible
            .iter()
            .filter_map(|idx| plugins.get(*idx))
            .map(|entry| {
                let row = project_row(entry, unreadable);
                let (lead, versions) = fit_row(&row, width);
                let lead_style = if row.has_installed {
                    theme::body_style()
                } else {
                    theme::dim_style()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(lead, lead_style),
                    Span::styled(versions, theme::dim_style()),
                ]))
            })
            .collect();
        let (style, symbol) = Self::highlight(self.focus == Focus::Packages);
        let title = self.panel_title(&t("zc-plugins-packages-title"), area);
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(&title))
                .highlight_style(style)
                .highlight_symbol(symbol),
            area,
            &mut self.list_state,
        );
        self.last_list_area = Some(area);
    }

    /// The package detail, with its channel instance block in a split below
    /// it. The detail scrolls on its own above the block, and its scroll is
    /// clamped against the rows the block leaves it.
    fn draw_detail(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        detail: &PackageDetail,
        block: Option<&InstanceBlock>,
    ) {
        let area = match block {
            Some(block) => {
                let height = instance_block_height(block, area);
                let parts = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(0), Constraint::Length(height)])
                    .split(area);
                self.draw_instances(frame, parts[1], block);
                parts[0]
            }
            None => area,
        };
        let paragraph = Paragraph::new(detail_lines(detail)).wrap(Wrap { trim: false });
        let inner_width = area.width.saturating_sub(2);
        let inner_height = area.height.saturating_sub(2);
        let max_scroll = u16::try_from(paragraph.line_count(inner_width))
            .unwrap_or(u16::MAX)
            .saturating_sub(inner_height);
        self.detail_scroll = self.detail_scroll.min(max_scroll);
        let title = self.panel_title(&detail.name, area);
        frame.render_widget(
            paragraph
                .block(theme::panel_block(&title))
                .scroll((self.detail_scroll, 0)),
            area,
        );
        self.last_detail_area = Some(area);
    }

    /// The instance list first, then the status line, then the footnote, so
    /// a short block cuts the footnote before the status.
    fn draw_instances(&mut self, frame: &mut Frame, area: Rect, block: &InstanceBlock) {
        let title = self.panel_title(&t("zc-plugins-instances-title"), area);
        let panel = theme::panel_block(&title);
        let inner = panel.inner(area);
        frame.render_widget(panel, area);
        let list_height = instance_list_rows(block).min(inner.height);
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(list_height), Constraint::Min(0)])
            .split(inner);
        if list_height > 0 {
            let width = usize::from(parts[0].width).saturating_sub(HIGHLIGHT_GUTTER);
            let items: Vec<ListItem> = fit_instance_rows(&block.rows, width)
                .into_iter()
                .map(|(alias, state)| {
                    ListItem::new(Line::from(vec![
                        Span::styled(alias, theme::body_style()),
                        Span::styled(state, theme::dim_style()),
                    ]))
                })
                .collect();
            let (style, symbol) = Self::highlight(self.focus == Focus::Instances);
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_style(style)
                    .highlight_symbol(symbol),
                parts[0],
                &mut self.instance_state,
            );
            self.last_instances_area = Some(parts[0]);
        }
        frame.render_widget(
            Paragraph::new(instance_text(block)).wrap(Wrap { trim: false }),
            parts[1],
        );
    }
}

/// Rows the instance list takes, capped so a long list scrolls in place.
fn instance_list_rows(block: &InstanceBlock) -> u16 {
    u16::try_from(block.rows.len())
        .unwrap_or(u16::MAX)
        .min(MAX_INSTANCE_ROWS)
}

/// The block's text under the list: the read error or the empty note, the
/// toggle status, and the footnote on what an enabled instance still needs.
fn instance_text(block: &InstanceBlock) -> Vec<Line<'static>> {
    let dim = |text: String| Line::from(Span::styled(text, theme::dim_style()));
    let mut lines = Vec::new();
    if let Some(error) = &block.error {
        lines.push(Line::from(Span::styled(error.clone(), theme::warn_style())));
        lines.extend(refresh_hint().map(dim));
    } else if block.rows.is_empty() {
        lines.push(dim(t("zc-plugins-instances-none")));
    }
    if let Some((status, warn)) = &block.status {
        let style = if *warn {
            theme::warn_style()
        } else {
            theme::accent_style()
        };
        lines.push(Line::from(Span::styled(status.clone(), style)));
    }
    lines.push(dim(t("zc-plugins-instances-footnote")));
    lines
}

/// Height of the instance block in `area`: everything it holds when that
/// fits, but never more than leaves the detail [`MIN_DETAIL_ROWS`].
fn instance_block_height(block: &InstanceBlock, area: Rect) -> u16 {
    let inner_width = area.width.saturating_sub(2);
    let text_rows = u16::try_from(
        Paragraph::new(instance_text(block))
            .wrap(Wrap { trim: false })
            .line_count(inner_width),
    )
    .unwrap_or(u16::MAX);
    let wanted = instance_list_rows(block)
        .saturating_add(text_rows)
        .saturating_add(2);
    let floor = area.height.min(3);
    wanted
        .min(area.height.saturating_sub(MIN_DETAIL_ROWS))
        .max(floor)
}

/// The config state words of one instance.
fn instance_state_text(enabled: Option<bool>) -> String {
    t(match enabled {
        Some(true) => "zc-plugins-instance-enabled",
        Some(false) => "zc-plugins-instance-disabled",
        None => "zc-plugins-instance-unknown",
    })
}

/// Fit instance rows into `width` cells: the aliases padded to one column,
/// then the state. The state keeps priority like the versions column of a
/// package row; an alias gives up room down to [`MIN_LEAD`] cells first.
fn fit_instance_rows(rows: &[(String, Option<bool>)], width: usize) -> Vec<(String, String)> {
    use crate::display_width::display_width;
    use crate::widgets::truncate_to_width;
    let states: Vec<String> = rows
        .iter()
        .map(|(_, enabled)| instance_state_text(*enabled))
        .collect();
    let widest_state = states.iter().map(|s| display_width(s)).max().unwrap_or(0);
    let alias_cap = if widest_state + 2 + MIN_LEAD <= width {
        width - widest_state - 2
    } else {
        width / 2
    };
    let column = rows
        .iter()
        .map(|(alias, _)| display_width(alias))
        .max()
        .unwrap_or(0)
        .min(alias_cap);
    rows.iter()
        .zip(states)
        .map(|((alias, _), state)| {
            let mut lead = truncate_to_width(alias, column);
            let pad = column.saturating_sub(display_width(&lead));
            lead.push_str(&" ".repeat(pad));
            let room = width.saturating_sub(display_width(&lead));
            let state = if room > 2 {
                format!("  {}", truncate_to_width(&state, room - 2))
            } else {
                String::new()
            };
            (lead, state)
        })
        .collect()
}

impl Drop for PluginsPane {
    fn drop(&mut self) {
        if let Some(task) = self.refresh_task.take() {
            task.abort();
        }
        if let Some((_, task)) = self.toggle_task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonrpc::{JsonRpcError, RpcOutbound};
    use crossterm::event::{KeyCode, KeyModifiers};
    use serde_json::{Value, json};
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::sync::mpsc;

    // ── Fixtures ─────────────────────────────────────────────────

    /// The daemon's canonical row (installed 0.1.0, registry 0.2.0) plus a
    /// registry-only and an installed-only row, deliberately not in name
    /// order so a client-side sort would show.
    fn catalog_body() -> Value {
        json!({
            "plugins_enabled": false,
            "wasm_plugins_available": true,
            "plugins_dir": "/tmp/.tmpXXXX/plugins",
            "plugins": [
                {
                    "name": "calendar",
                    "installed": {
                        "version": "0.1.0",
                        "description": "installed description",
                        "capabilities": ["tool"],
                        "permissions": ["file_read"]
                    },
                    "available": {
                        "version": "0.2.0",
                        "description": "registry description",
                        "capabilities": ["tool", "skill"],
                        "install_source": "calendar@0.2.0"
                    }
                },
                {
                    "name": "mail",
                    "installed": null,
                    "available": {
                        "version": "1.2.3",
                        "description": "Mail integration",
                        "capabilities": ["channel"],
                        "install_source": "mail@1.2.3"
                    }
                },
                {
                    "name": "archive",
                    "installed": {
                        "version": "0.3.0",
                        "description": null,
                        "capabilities": ["memory"],
                        "permissions": []
                    },
                    "available": null
                }
            ],
            "issues": []
        })
    }

    fn catalog() -> PluginsListResult {
        serde_json::from_value(catalog_body()).unwrap()
    }

    fn entry(
        name: &str,
        installed: Option<(&str, &[&str])>,
        available: Option<(&str, &[&str])>,
    ) -> PluginCatalogEntry {
        let strings = |items: &[&str]| items.iter().map(|item| item.to_string()).collect();
        PluginCatalogEntry {
            name: name.to_string(),
            installed: installed.map(|(version, capabilities)| {
                crate::wire::InstalledPluginPackage {
                    version: version.to_string(),
                    description: None,
                    capabilities: strings(capabilities),
                    permissions: Vec::new(),
                }
            }),
            available: available.map(|(version, capabilities)| {
                crate::wire::AvailablePluginPackage {
                    version: version.to_string(),
                    description: None,
                    capabilities: strings(capabilities),
                    install_source: format!("{name}@{version}"),
                }
            }),
        }
    }

    fn loaded_pane(data: PluginsListResult) -> PluginsPane {
        let mut pane = PluginsPane::new();
        pane.apply_result(Ok(data));
        pane
    }

    /// Hold the keymap test guard with no overrides installed, so chord
    /// labels and key resolution see the default bindings. Never call
    /// [`press`] while holding it: the guard is not reentrant.
    fn default_keymap() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        guard
    }

    /// Press `code` with no modifiers, resolved against the default keymap
    /// and serialized with every test that installs keybinding overrides.
    fn press(pane: &mut PluginsPane, code: KeyCode) -> PluginsKeyOutcome {
        let _guard = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        pane.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn client() -> (Arc<RpcClient>, Arc<RpcOutbound>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel(8);
        let outbound = Arc::new(RpcOutbound::new(tx));
        (
            Arc::new(RpcClient::with_rpc(Arc::clone(&outbound))),
            outbound,
            rx,
        )
    }

    /// Receive the next outbound request, assert it is `plugins/list` with
    /// `{}` params, and return its id.
    async fn receive_catalog_request(rx: &mut mpsc::Receiver<String>) -> String {
        let raw = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("a plugins/list request should be sent")
            .expect("the RPC writer should remain connected");
        let request: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(request["method"], crate::client::method::PLUGINS_LIST);
        assert_eq!(request["params"], json!({}));
        request["id"].as_str().unwrap().to_string()
    }

    /// Receive the instance read that follows a loaded catalog, assert it
    /// is `config/list` over the whole instance table, and answer it with no
    /// instances.
    async fn answer_instance_request(rx: &mut mpsc::Receiver<String>, outbound: &RpcOutbound) {
        let raw = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("a config/list request should follow the catalog")
            .expect("the RPC writer should remain connected");
        let request: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(request["method"], crate::client::method::CONFIG_LIST);
        assert_eq!(request["params"], json!({ "prefix": "channels.plugin" }));
        let id = request["id"].as_str().unwrap();
        outbound.dispatch_response(id, Some(json!({ "entries": [] })), None);
    }

    /// Give any spawned fetch time to reach the wire, then assert none did.
    async fn assert_no_request(rx: &mut mpsc::Receiver<String>, why: &str) {
        let next = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
        assert!(next.is_err(), "{why}: {next:?}");
    }

    async fn settle(pane: &mut PluginsPane) {
        for _ in 0..200 {
            tokio::task::yield_now().await;
            pane.poll_refresh().await;
            if !pane.is_loading() {
                return;
            }
        }
        panic!("the catalog fetch never finished");
    }

    fn rpc_error(code: i32, message: &str) -> JsonRpcError {
        JsonRpcError {
            code,
            message: message.to_string(),
            data: None,
        }
    }

    /// A client answered by a task that records every method and replies to
    /// each request with `reply`.
    fn responding_client(
        reply: Result<Value, JsonRpcError>,
    ) -> (Arc<RpcClient>, Arc<Mutex<Vec<String>>>) {
        let (tx, mut rx) = mpsc::channel::<String>(16);
        let outbound = Arc::new(RpcOutbound::new(tx));
        let rpc = Arc::new(RpcClient::with_rpc(Arc::clone(&outbound)));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let calls_for_task = Arc::clone(&calls);
        tokio::spawn(async move {
            while let Some(raw) = rx.recv().await {
                let Ok(request) = serde_json::from_str::<Value>(&raw) else {
                    continue;
                };
                calls_for_task
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(request["method"].as_str().unwrap_or_default().to_string());
                let id = request["id"].as_str().unwrap_or_default().to_string();
                match &reply {
                    Ok(body) => outbound.dispatch_response(&id, Some(body.clone()), None),
                    Err(error) => outbound.dispatch_response(&id, None, Some(error.clone())),
                }
            }
        });
        (rpc, calls)
    }

    fn render_rows(pane: &mut PluginsPane, w: u16, h: u16) -> Vec<String> {
        use ratatui::{Terminal, backend::TestBackend};
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|frame| pane.draw(frame, Rect::new(0, 0, w, h)))
            .unwrap();
        let buffer = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    // ── Untrusted text ───────────────────────────────────────────

    #[test]
    fn escape_sequences_become_a_visible_replacement_character() {
        assert_eq!(display_safe("red\u{1b}[31mtext"), "red\u{fffd}[31mtext");
    }

    #[test]
    fn carriage_return_newline_and_tab_become_one_space() {
        assert_eq!(display_safe("a\rb"), "a b");
        assert_eq!(display_safe("a\r\nb"), "a b");
        assert_eq!(display_safe("a\tb\nc"), "a b c");
        assert_eq!(display_safe("  lead \t\n trail  "), "lead trail");
        assert_eq!(display_safe("a\u{2028}b\u{00a0}c"), "a b c");
    }

    #[test]
    fn bidi_override_is_replaced() {
        assert_eq!(display_safe("abc\u{202e}fed"), "abc\u{fffd}fed");
        assert_eq!(display_safe("\u{2066}x\u{2069}"), "\u{fffd}x\u{fffd}");
    }

    #[test]
    fn zero_width_space_is_replaced_so_lookalike_names_differ() {
        assert_eq!(display_safe("evil\u{200b}name"), "evil\u{fffd}name");
        assert_ne!(display_safe("evil\u{200b}name"), display_safe("evilname"));
    }

    #[test]
    fn delete_and_c1_controls_are_replaced() {
        assert_eq!(display_safe("a\u{7f}b"), "a\u{fffd}b");
        assert_eq!(display_safe("a\u{9b}31mb"), "a\u{fffd}31mb");
        assert_eq!(display_safe("bell\u{7}"), "bell\u{fffd}");
    }

    #[test]
    fn long_text_is_capped_with_an_ellipsis() {
        let raw = "x".repeat(10_000);
        let safe = display_safe(&raw);
        assert_eq!(safe.chars().count(), MAX_DISPLAY_CHARS);
        assert!(safe.ends_with('\u{2026}'));
        assert_eq!(
            display_safe(&"y".repeat(MAX_DISPLAY_CHARS)).chars().count(),
            MAX_DISPLAY_CHARS
        );
        assert!(!display_safe(&"y".repeat(MAX_DISPLAY_CHARS)).ends_with('\u{2026}'));
    }

    #[test]
    fn sanitized_text_carries_no_control_or_format_characters() {
        let hostile = "\u{1b}]2;title\u{7}\r\n\u{202e}\u{200d}\u{feff}\u{85}\u{9c}ok\u{0}\
                       \u{fe0f}\u{fe00}\u{34f}\u{3164}\u{115f}\u{ffa0}\u{180b}\u{e0100}\u{e0041}";
        let safe = display_safe(hostile);
        // An exact expectation, so a character dropped from the denylist
        // fails here instead of passing its own membership check.
        let replaced = |n: usize| "\u{fffd}".repeat(n);
        assert_eq!(
            safe,
            format!("\u{fffd}]2;title\u{fffd} {}ok{}", replaced(5), replaced(10))
        );
    }

    #[test]
    fn a_version_that_is_not_one_token_is_quoted() {
        let spoof = entry("evil", None, Some(("1.0.0 installed, in registry", &[])));
        assert_eq!(
            versions_text(&spoof),
            "v\"1.0.0\\u{20}installed,\\u{20}in\\u{20}registry\" in registry"
        );
        assert_eq!(
            project_detail(&spoof, Unreadable::default())
                .registry
                .unwrap()
                .version,
            "\"1.0.0\\u{20}installed,\\u{20}in\\u{20}registry\""
        );
        // An allowlist, not a denylist: a blank-looking character that is
        // neither whitespace nor a control still makes the value quoted.
        let braille = entry(
            "evil",
            None,
            Some(("1.0.0\u{2800}installed,\u{2800}in\u{2800}registry", &[])),
        );
        assert_eq!(
            versions_text(&braille),
            "v\"1.0.0\\u{2800}installed,\\u{2800}in\\u{2800}registry\" in registry"
        );
        for raw in [
            "",
            "1.0.0\u{200b}",
            "1.0.0\n",
            "1.0.0\u{2800}",
            "1.0.\u{e9}",
        ] {
            let shown = display_token(raw);
            assert!(shown.starts_with('"'), "{raw:?} shown as {shown:?}");
            assert!(
                shown.chars().all(|c| !is_hidden(c) && c != ' '),
                "{shown:?}"
            );
        }
        assert_eq!(
            display_token("0.2.0-beta.1+build.7"),
            "0.2.0-beta.1+build.7"
        );
        assert_eq!(display_safe("a\u{2800}b"), "a\u{fffd}b");

        // A raw value spelled like the quoted form of another value is
        // itself quoted, so the two never render the same.
        let spelled_quoted = r#""1.0.0\u{20}""#;
        assert_eq!(display_token("1.0.0 "), spelled_quoted);
        assert_eq!(display_token(spelled_quoted), r#""\"1.0.0\\u{20}\"""#);

        // The install identity names a package, so a lookalike is quoted
        // instead of reading as the real package's identity.
        assert_eq!(display_token("calendar@1.0.0"), "calendar@1.0.0");
        assert_eq!(
            display_token(" calendar@1.0.0"),
            "\"\\u{20}calendar@1.0.0\""
        );
    }

    #[test]
    fn scans_and_lists_are_bounded() {
        let padded = " ".repeat(100_000);
        assert_eq!(display_safe(&format!("{padded}x")), "\u{2026}");
        assert_eq!(display_safe(&format!("a{padded}x")), "a\u{2026}");
        assert_eq!(display_safe(&format!("a{}", " ".repeat(10))), "a");

        let many: Vec<String> = (0..100).map(|i| format!("cap{i}")).collect();
        let shown = safe_list(&many);
        assert_eq!(shown.len(), MAX_LIST_ITEMS + 1);
        assert_eq!(shown[MAX_LIST_ITEMS - 1], "cap31");
        assert_eq!(shown[MAX_LIST_ITEMS], "+68 more");
        assert_eq!(safe_list(&many[..MAX_LIST_ITEMS]), many[..MAX_LIST_ITEMS]);
    }

    #[test]
    fn quoted_values_escape_what_would_not_print() {
        assert_eq!(display_name("a\u{301}b"), r#""a\u{301}b""#);
        assert_eq!(display_name("x\u{e000}"), r#""x\u{e000}""#);
        assert_eq!(display_name("x\u{378}"), r#""x\u{378}""#);
        assert_eq!(display_name("it's"), r#""it\'s""#);
        // Printable non-ASCII stays readable inside the quotes.
        assert_eq!(display_name("\u{65e5}\u{672c}"), "\"\u{65e5}\u{672c}\"");
    }

    /// Every place a version or install identity reaches the screen goes
    /// through the token allowlist, not just one of them.
    #[test]
    fn every_version_and_identity_slot_quotes_a_non_token() {
        let spaced = r#""1\u{20}x""#;
        assert_eq!(
            versions_text(&entry("a", Some(("1 x", &[])), Some(("1 x", &[])))),
            format!("v{spaced} installed, in registry")
        );
        assert_eq!(
            versions_text(&entry("b", Some(("1 x", &[])), Some(("0.2.0", &[])))),
            format!("v{spaced} installed, registry v0.2.0")
        );
        assert_eq!(
            versions_text(&entry("c", Some(("1 x", &[])), None)),
            format!("v{spaced} installed")
        );
        let detail = project_detail(
            &entry("d", Some(("1 x", &[])), Some(("1 x", &[]))),
            Unreadable::default(),
        );
        assert_eq!(detail.installed.unwrap().version, spaced);
        assert_eq!(detail.registry.unwrap().version, spaced);

        let lookalike = entry(" calendar", None, Some(("1.0.0", &[])));
        assert_eq!(
            project_detail(&lookalike, Unreadable::default())
                .registry
                .unwrap()
                .install_source,
            r#""\u{20}calendar@1.0.0""#
        );

        // And through the real draw path: the row shows the registry version
        // quoted, so it cannot repeat the row's own wording.
        let mut data = catalog();
        data.plugins = vec![entry(
            "cal",
            Some(("0.1.0", &[])),
            Some(("9.9.9 installed, registry v9.9.9", &[])),
        )];
        let rows = render_rows(&mut loaded_pane(data), 120, 8);
        assert!(
            rows.iter()
                .any(|row| row.contains(r#"registry v"9.9.9\u{20}installed,"#)),
            "{rows:#?}"
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.contains("registry v9.9.9 installed")),
            "{rows:#?}"
        );
    }

    #[test]
    fn oversized_names_and_versions_are_capped_in_quoted_form() {
        let huge = "\u{200b}".repeat(100_000);
        for shown in [display_name(&huge), display_token(&huge)] {
            assert_eq!(shown.chars().count(), MAX_DISPLAY_CHARS);
            assert!(shown.starts_with('"') && shown.ends_with('\u{2026}'));
        }
        let long_token = "9".repeat(MAX_DISPLAY_CHARS + 1);
        assert_eq!(
            display_token(&long_token).chars().count(),
            MAX_DISPLAY_CHARS
        );
    }

    #[test]
    fn invisible_fillers_and_variation_selectors_are_replaced() {
        for raw in ["cal\u{fe0f}endar", "a\u{34f}b", "\u{3164}", "x\u{e0100}"] {
            assert!(
                display_safe(raw).contains('\u{fffd}'),
                "{raw:?} rendered as {:?}",
                display_safe(raw)
            );
        }
        assert_eq!(
            display_safe("\u{3164}"),
            "\u{fffd}",
            "a filler-only name is not blank"
        );
        assert_ne!(display_safe("calendar\u{fe00}"), display_safe("calendar"));
        assert_ne!(display_safe("cal\u{34f}endar"), display_safe("calendar"));
    }

    #[test]
    fn rows_fit_wide_characters_at_a_width_boundary() {
        let row = PackageRow {
            has_installed: true,
            installed_unknown: false,
            name: "日本語プラグイン".to_string(),
            versions: "v1.0.0 installed".to_string(),
        };
        for width in 0..24 {
            let (lead, versions) = fit_row(&row, width);
            let used = crate::display_width::display_width(&lead)
                + crate::display_width::display_width(&versions);
            assert!(
                used <= width,
                "width {width}: {lead:?} {versions:?} uses {used}"
            );
        }
        // Eleven cells: the versions do not fit beside even the shortest
        // name, so the name is capped at half the row (five cells: the
        // marker, a space, one wide character and the ellipsis; a second
        // wide character would overflow), and the versions take the rest.
        let (lead, versions) = fit_row(&row, 11);
        assert_eq!(lead, "\u{25cf} 日\u{2026}");
        assert_eq!(versions, "  v1.\u{2026}");
        let (lead, versions) = fit_row(&row, 12);
        assert_eq!(lead, "\u{25cf} 日\u{2026}");
        assert_eq!(versions, "  v1.0\u{2026}");
        // A row wide enough for both keeps the whole name.
        let (lead, versions) = fit_row(&row, 40);
        assert_eq!(lead, "\u{25cf} 日本語プラグイン");
        assert_eq!(versions, "  v1.0.0 installed");
    }

    #[test]
    fn a_long_name_never_pushes_the_versions_out_of_the_row() {
        let row = PackageRow {
            has_installed: true,
            installed_unknown: false,
            name: "n".repeat(60),
            versions: "v1.0.0 installed, registry v2.0.0".to_string(),
        };
        let (lead, versions) = fit_row(&row, 46);
        assert!(lead.ends_with('\u{2026}'), "{lead:?}");
        assert!(versions.starts_with("  v1.0.0"), "{versions:?}");
        let used = crate::display_width::display_width(&lead)
            + crate::display_width::display_width(&versions);
        assert!(used <= 46, "{lead:?} {versions:?} uses {used}");
    }

    /// 46 cells is the package row of an 80-column terminal: the 50-cell
    /// right pane less its borders and the highlight gutter.
    #[test]
    fn both_versions_stay_whole_beside_a_medium_name_at_80_columns() {
        let versions = "v1.10.0 installed, registry v1.11.0";
        for name in ["calendar-s", "calendar-sync-x", "calendar-sync-bridge!"] {
            let row = PackageRow {
                has_installed: true,
                installed_unknown: false,
                name: name.to_string(),
                versions: versions.to_string(),
            };
            let (lead, shown) = fit_row(&row, 46);
            assert_eq!(shown, format!("  {versions}"), "{name}: {lead:?}");
            assert!(lead.starts_with("\u{25cf} calend"), "{name}: {lead:?}");
            let used = crate::display_width::display_width(&lead)
                + crate::display_width::display_width(&shown);
            assert!(used <= 46, "{name}: {lead:?} {shown:?} uses {used}");
        }
        // A name that fits beside the versions is kept whole.
        let row = PackageRow {
            has_installed: true,
            installed_unknown: false,
            name: "cal".to_string(),
            versions: versions.to_string(),
        };
        assert_eq!(fit_row(&row, 46).0, "\u{25cf} cal");
    }

    #[test]
    fn every_daemon_string_is_sanitized_at_projection() {
        let hostile = PluginCatalogEntry {
            name: "cal\u{1b}[2Jendar\nx\u{fe0f}\u{3164}".to_string(),
            installed: Some(crate::wire::InstalledPluginPackage {
                version: "1.0\u{202e}".to_string(),
                description: Some("line one\r\nline two".to_string()),
                capabilities: vec!["to\u{200b}ol\u{34f}".to_string()],
                permissions: vec!["file\u{7f}read".to_string()],
            }),
            available: Some(crate::wire::AvailablePluginPackage {
                version: "2.0\u{9b}".to_string(),
                description: Some("\u{2066}hidden".to_string()),
                capabilities: vec!["ch\u{0}annel".to_string()],
                install_source: "cal@2.0\u{7}\u{e0100}".to_string(),
            }),
        };
        let row = project_row(&hostile, Unreadable::default());
        let detail = project_detail(&hostile, Unreadable::default());
        let installed = detail.installed.as_ref().unwrap();
        let registry = detail.registry.as_ref().unwrap();
        let mut texts = vec![
            row.name.clone(),
            row.versions.clone(),
            detail.name.clone(),
            installed.version.clone(),
            installed.description.clone().unwrap_or_default(),
            registry.version.clone(),
            registry.description.clone().unwrap_or_default(),
            registry.install_source.clone(),
        ];
        texts.extend(installed.capabilities.iter().cloned());
        texts.extend(installed.permissions.iter().cloned());
        texts.extend(registry.capabilities.iter().cloned());
        for text in texts {
            assert!(
                text.chars().all(|c| !is_hidden(c)),
                "unsanitized projection: {text:?}"
            );
        }
        // Not a valid package name, so it is shown escaped rather than
        // sanitized: every hidden character stays visible as an escape.
        assert_eq!(row.name, r#""cal\u{1b}[2Jendar\nx\u{fe0f}\u{3164}""#);
        assert_eq!(detail.name, row.name);
        assert_eq!(installed.description.as_deref(), Some("line one line two"));
    }

    #[test]
    fn names_that_differ_never_render_the_same() {
        let shown = |name: &str| {
            let entry = entry(name, None, Some(("1.0.0", &[])));
            let row = project_row(&entry, Unreadable::default());
            let detail = project_detail(&entry, Unreadable::default());
            assert_eq!(row.name, detail.name, "{name:?}");
            row.name
        };
        // A valid name renders unchanged, with no quotes.
        assert_eq!(shown("calendar"), "calendar");
        assert_eq!(shown("acme.chat-v2"), "acme.chat-v2");

        // Registry names that sanitizing would fold into "calendar", and a
        // name spelled like the escaped form of one of them.
        let names = [
            "calendar",
            "calendar ",
            " calendar",
            "calendar\u{a0}",
            "calendar\t",
            r#""calendar\u{20}""#,
            "Calendar",
        ];
        let rendered: Vec<String> = names.iter().map(|name| shown(name)).collect();
        for (i, a) in rendered.iter().enumerate() {
            for b in &rendered[i + 1..] {
                assert_ne!(a, b, "{rendered:#?}");
            }
            assert!(a.chars().all(|c| !is_hidden(c)), "{a:?}");
        }
        assert_eq!(rendered[1], r#""calendar\u{20}""#);
        assert_eq!(rendered[3], r#""calendar\u{a0}""#);

        // An oversized name is capped like any other daemon string.
        let long = shown(&"x ".repeat(5_000));
        assert_eq!(long.chars().count(), MAX_DISPLAY_CHARS);
        assert!(long.starts_with('"') && long.ends_with('\u{2026}'));

        // The list keeps the two rows apart too.
        let mut data = catalog_of(&[]);
        data.plugins = vec![
            entry("calendar", Some(("0.1.0", &[])), None),
            entry("calendar ", None, Some(("0.2.0", &[]))),
        ];
        let rows = render_rows(&mut loaded_pane(data), 80, 24);
        assert!(
            rows.iter()
                .any(|row| row.contains("\u{25cf} calendar  v0.1.0 installed")),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("\u{25cb} \"calendar\\u{20}\"  v0.2.0 in registry")),
            "{rows:#?}"
        );
    }

    #[test]
    fn plugin_directory_and_daemon_messages_are_sanitized() {
        let mut data = catalog();
        data.plugins_dir = "/srv/\u{1b}[31mplugins\u{202e}".to_string();
        let text: String = host_lines(&data)
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.to_string()))
            .collect();
        assert!(text.contains("/srv/\u{fffd}[31mplugins\u{fffd}"));
        assert!(!text.contains('\u{1b}'));

        let err = anyhow::Error::new(RpcCallError {
            method: "plugins/list".to_string(),
            code: error_codes::INTERNAL_ERROR,
            message: "scan\u{1b}[2J busy\r\nretry".to_string(),
        });
        assert_eq!(
            CatalogError::from_call(&err),
            CatalogError::Other("scan\u{fffd}[2J busy retry".to_string())
        );
    }

    #[test]
    fn plugins_enabled_renders_as_its_own_config_value() {
        for (enabled, expected) in [(true, "  yes"), (false, "  no")] {
            let mut data = catalog();
            data.plugins_enabled = enabled;
            let lines: Vec<String> = host_lines(&data)
                .iter()
                .map(|line| line.spans.iter().map(|s| s.content.to_string()).collect())
                .collect();
            let label = lines
                .iter()
                .position(|line| *line == t("zc-plugins-host-enabled-label"))
                .unwrap_or_else(|| panic!("{lines:#?}"));
            assert_eq!(lines[label + 1], expected, "{lines:#?}");
        }
    }

    // ── Projection ───────────────────────────────────────────────

    #[test]
    fn version_column_covers_the_four_record_cases() {
        let same = entry("a", Some(("1.0.0", &[])), Some(("1.0.0", &[])));
        let other = entry("b", Some(("0.1.0", &[])), Some(("0.2.0", &[])));
        let installed_only = entry("c", Some(("0.3.0", &[])), None);
        let registry_only = entry("d", None, Some(("1.2.3", &[])));

        assert_eq!(versions_text(&same), "v1.0.0 installed, in registry");
        assert_eq!(versions_text(&other), "v0.1.0 installed, registry v0.2.0");
        assert_eq!(versions_text(&installed_only), "v0.3.0 installed");
        assert_eq!(versions_text(&registry_only), "v1.2.3 in registry");

        // An older registry version is described exactly like a newer one:
        // versions are never ordered, so no upgrade is implied either way.
        let older_registry = entry("e", Some(("0.2.0", &[])), Some(("0.1.0", &[])));
        assert_eq!(
            versions_text(&older_registry),
            "v0.2.0 installed, registry v0.1.0"
        );

        // The wording follows the daemon's raw strings, so versions that only
        // look alike once sanitized are still described as two versions, and
        // the lookalike is shown quoted rather than as the installed version.
        for (registry, shown) in [
            ("1.0.0 ", r#""1.0.0\u{20}""#),
            ("1.0.0\n", r#""1.0.0\n""#),
            ("1.0.0\u{fe0f}", r#""1.0.0\u{fe0f}""#),
        ] {
            let lookalike = entry("f", Some(("1.0.0", &[])), Some((registry, &[])));
            assert_eq!(
                versions_text(&lookalike),
                format!("v1.0.0 installed, registry v{shown}")
            );
        }

        assert!(project_row(&same, Unreadable::default()).has_installed);
        assert!(!project_row(&registry_only, Unreadable::default()).has_installed);
        assert_eq!(
            fit_row(&project_row(&registry_only, Unreadable::default()), 40).0,
            "\u{25cb} d"
        );
        assert_eq!(
            fit_row(&project_row(&installed_only, Unreadable::default()), 40).0,
            "\u{25cf} c"
        );
    }

    #[test]
    fn filter_counts_follow_record_presence() {
        let plugins = catalog().plugins;
        assert_eq!(CatalogFilter::All.count(&plugins), 3);
        assert_eq!(CatalogFilter::Installed.count(&plugins), 2);
        assert_eq!(CatalogFilter::Registry.count(&plugins), 2);
        assert_eq!(CatalogFilter::Registry.count(&[]), 0);
    }

    #[test]
    fn rows_keep_daemon_order_and_are_never_deduplicated() {
        let mut data = catalog();
        data.plugins
            .push(entry("calendar", None, Some(("9.9.9", &[]))));
        let mut pane = loaded_pane(data.clone());
        let names: Vec<String> = pane
            .visible_indices()
            .into_iter()
            .map(|idx| data.plugins[idx].name.clone())
            .collect();
        assert_eq!(names, vec!["calendar", "mail", "archive", "calendar"]);

        // The drawn list too: every row, top to bottom, from its marker to
        // the right border.
        let rows = render_rows(&mut pane, 100, 20);
        let drawn: Vec<String> = rows
            .iter()
            .filter_map(|row| {
                let start = row.find(['\u{25cf}', '\u{25cb}'])?;
                Some(row[start..].trim_end_matches('\u{2502}').trim().to_string())
            })
            .collect();
        assert_eq!(
            drawn,
            vec![
                "\u{25cf} calendar  v0.1.0 installed, registry v0.2.0",
                "\u{25cb} mail  v1.2.3 in registry",
                "\u{25cf} archive  v0.3.0 installed",
                "\u{25cb} calendar  v9.9.9 in registry",
            ],
            "{rows:#?}"
        );
    }

    #[test]
    fn installed_and_registry_capabilities_are_never_combined() {
        let split = entry(
            "mixed",
            Some(("1.0.0", &["tool"])),
            Some(("1.0.0", &["channel", "skill"])),
        );
        let detail = project_detail(&split, Unreadable::default());
        assert_eq!(
            detail.installed.as_ref().unwrap().capabilities,
            vec!["tool"]
        );
        assert_eq!(
            detail.registry.as_ref().unwrap().capabilities,
            vec!["channel", "skill"]
        );

        let lines: Vec<String> = detail_lines(&detail)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect();
        let capability_lines: Vec<&String> = lines
            .iter()
            .filter(|line| line.contains("Capabilities:"))
            .collect();
        assert_eq!(
            capability_lines,
            vec!["  Capabilities: tool", "  Capabilities: channel, skill"],
            "each record keeps its own capability line"
        );
        let installed_heading = lines.iter().position(|l| l == "Installed").unwrap();
        let registry_heading = lines.iter().position(|l| l == "Cached registry").unwrap();
        let tool_line = lines
            .iter()
            .position(|l| l == "  Capabilities: tool")
            .unwrap();
        let registry_line = lines
            .iter()
            .position(|l| l == "  Capabilities: channel, skill")
            .unwrap();
        assert!(installed_heading < tool_line && tool_line < registry_heading);
        assert!(registry_heading < registry_line);
    }

    #[test]
    fn detail_marks_absent_records_and_missing_descriptions() {
        let registry_only = entry("mail", None, Some(("1.2.3", &[])));
        let text: Vec<String> =
            detail_lines(&project_detail(&registry_only, Unreadable::default()))
                .iter()
                .map(|line| line.spans.iter().map(|s| s.content.to_string()).collect())
                .collect();
        assert_eq!(text[0], "Name: mail");
        assert!(text.contains(&"  Not installed".to_string()));
        assert!(text.contains(&"  Description: none provided".to_string()));
        assert!(text.contains(&"  Capabilities: none".to_string()));
        assert!(text.contains(&"  Package identity: mail@1.2.3".to_string()));
        assert!(
            text.iter()
                .any(|line| line.contains("does not show whether a plugin is loaded or running"))
        );

        let installed_only = entry("notes", Some(("0.3.0", &["memory"])), None);
        let text: Vec<String> =
            detail_lines(&project_detail(&installed_only, Unreadable::default()))
                .iter()
                .map(|line| line.spans.iter().map(|s| s.content.to_string()).collect())
                .collect();
        assert!(text.contains(&"  Not in the cached registry".to_string()));
        assert!(text.contains(&"  Requested permissions: none".to_string()));
    }

    #[test]
    fn issue_lines_name_the_source_and_point_at_the_daemon_log() {
        let issue = |source, code| PluginCatalogIssue { source, code };
        use PluginCatalogIssueCode as C;
        use PluginCatalogIssueSource as S;
        assert!(
            issue_text(&issue(S::Installed, C::DiscoveryFailed))
                .starts_with("Could not read installed packages")
        );
        assert!(
            issue_text(&issue(S::Registry, C::CacheReadFailed))
                .starts_with("Could not read the cached registry")
        );
        assert!(issue_text(&issue(S::Unknown, C::Unknown)).starts_with("Unknown catalog"));
        assert!(issue_text(&issue(S::Installed, C::Unknown)).starts_with("Unknown catalog"));
        for text in [
            issue_text(&issue(S::Installed, C::DiscoveryFailed)),
            issue_text(&issue(S::Registry, C::CacheReadFailed)),
            issue_text(&issue(S::Unknown, C::Unknown)),
        ] {
            assert!(text.contains("daemon log"), "{text}");
        }
    }

    #[test]
    fn unreadable_sources_follow_the_issue_source_whatever_the_code() {
        let issues = |pairs: &[(PluginCatalogIssueSource, PluginCatalogIssueCode)]| {
            Unreadable::from_issues(
                &pairs
                    .iter()
                    .map(|&(source, code)| PluginCatalogIssue { source, code })
                    .collect::<Vec<_>>(),
            )
        };
        use PluginCatalogIssueCode as C;
        use PluginCatalogIssueSource as S;
        let both = Unreadable {
            installed: true,
            registry: true,
        };
        assert_eq!(issues(&[]), Unreadable::default());
        assert!(!issues(&[]).any());
        assert_eq!(
            issues(&[(S::Installed, C::DiscoveryFailed)]),
            Unreadable {
                installed: true,
                registry: false
            }
        );
        assert_eq!(
            issues(&[(S::Installed, C::Unknown)]),
            Unreadable {
                installed: true,
                registry: false
            }
        );
        assert_eq!(
            issues(&[(S::Registry, C::CacheReadFailed)]),
            Unreadable {
                installed: false,
                registry: true
            }
        );
        assert_eq!(issues(&[(S::Unknown, C::Unknown)]), both);
        assert_eq!(
            issues(&[
                (S::Installed, C::DiscoveryFailed),
                (S::Registry, C::Unknown)
            ]),
            both
        );
    }

    #[test]
    fn a_missing_record_from_an_unreadable_source_is_unknown_not_absent() {
        let lines = |entry: &PluginCatalogEntry, unreadable| -> Vec<String> {
            detail_lines(&project_detail(entry, unreadable))
                .iter()
                .map(|line| line.spans.iter().map(|s| s.content.to_string()).collect())
                .collect()
        };
        let registry_only = entry("mail", None, Some(("1.2.3", &[])));
        let installed_only = entry("notes", Some(("0.3.0", &[])), None);
        let installed_unreadable = Unreadable {
            installed: true,
            registry: false,
        };
        let registry_unreadable = Unreadable {
            installed: false,
            registry: true,
        };

        let text = lines(&registry_only, installed_unreadable);
        assert!(!text.contains(&"  Not installed".to_string()), "{text:#?}");
        assert!(
            text.iter()
                .any(|line| line.contains("Unknown: installed packages could not be read")),
            "{text:#?}"
        );
        let row = project_row(&registry_only, installed_unreadable);
        assert!(row.installed_unknown);
        assert_eq!(fit_row(&row, 40).0, "? mail");
        assert_eq!(row.versions, "v1.2.3 in registry");
        // A record that is present is shown whatever the other source says.
        let text = lines(&installed_only, installed_unreadable);
        assert!(text.contains(&"  Version: 0.3.0".to_string()), "{text:#?}");
        assert!(
            text.contains(&"  Not in the cached registry".to_string()),
            "{text:#?}"
        );
        assert_eq!(
            fit_row(&project_row(&installed_only, installed_unreadable), 40).0,
            "\u{25cf} notes"
        );

        let text = lines(&installed_only, registry_unreadable);
        assert!(
            !text.contains(&"  Not in the cached registry".to_string()),
            "{text:#?}"
        );
        assert!(
            text.iter()
                .any(|line| line.contains("Unknown: the cached registry could not be read")),
            "{text:#?}"
        );
        let text = lines(&registry_only, registry_unreadable);
        assert!(text.contains(&"  Not installed".to_string()), "{text:#?}");
        assert_eq!(
            fit_row(&project_row(&registry_only, registry_unreadable), 40).0,
            "\u{25cb} mail"
        );
    }

    #[test]
    fn filter_counts_over_an_unreadable_source_are_not_exact() {
        let plugins = catalog().plugins;
        let installed_unreadable = Unreadable {
            installed: true,
            registry: false,
        };
        assert_eq!(
            CatalogFilter::All.counted_label(&plugins, Unreadable::default()),
            "All (3)"
        );
        assert_eq!(
            CatalogFilter::Installed.counted_label(&plugins, installed_unreadable),
            "Installed (?)"
        );
        assert_eq!(
            CatalogFilter::Registry.counted_label(&plugins, installed_unreadable),
            "In registry (2)"
        );
        assert_eq!(
            CatalogFilter::All.counted_label(&plugins, installed_unreadable),
            "All (3+)"
        );
    }

    // ── Error classification ─────────────────────────────────────

    #[test]
    fn typed_errors_classify_without_string_matching() {
        let rpc = |code: i32, message: &str| {
            anyhow::Error::new(RpcCallError {
                method: "plugins/list".to_string(),
                code,
                message: message.to_string(),
            })
        };
        assert_eq!(
            CatalogError::from_call(&rpc(error_codes::METHOD_NOT_FOUND, "whatever")),
            CatalogError::Unsupported
        );
        assert_eq!(
            CatalogError::from_call(&rpc(error_codes::FORBIDDEN, "denied\u{1b}[2J")),
            CatalogError::Forbidden("denied\u{fffd}[2J".to_string())
        );
        assert_eq!(
            CatalogError::from_call(&rpc(
                error_codes::INTERNAL_ERROR,
                "another plugin catalog scan is running; retry"
            )),
            CatalogError::Other("another plugin catalog scan is running; retry".to_string())
        );
        let timeout = anyhow::Error::new(RpcCallTimeout {
            method: "plugins/list".to_string(),
            timeout: Duration::from_secs(20),
        });
        assert_eq!(CatalogError::from_call(&timeout), CatalogError::TimedOut);
        // Text alone never classifies: a message that merely mentions a code
        // or a timeout is an ordinary failure.
        let text_only = anyhow::Error::msg("RPC plugins/list: timed out after 20s (-32601)");
        assert!(matches!(
            CatalogError::from_call(&text_only),
            CatalogError::Other(_)
        ));
    }

    #[test]
    fn each_error_kind_has_its_own_message() {
        assert!(
            CatalogError::Unsupported
                .message()
                .contains("does not provide the plugin catalog")
        );
        assert_eq!(
            CatalogError::Forbidden("no grant".to_string()).message(),
            "The daemon refused the plugin catalog request: no grant"
        );
        assert!(CatalogError::TimedOut.message().contains("timed out"));
        assert_eq!(
            CatalogError::Other("boom".to_string()).message(),
            "Could not load the plugin catalog: boom"
        );
    }

    // ── Fetch lifecycle ──────────────────────────────────────────

    #[tokio::test]
    async fn first_entry_sends_exactly_one_request() {
        let (rpc, outbound, mut rx) = client();
        let mut pane = PluginsPane::new();
        assert!(!pane.is_loading(), "construction must not fetch");

        pane.refresh_if_inactive(&rpc);
        let id = receive_catalog_request(&mut rx).await;
        // The first request is on the wire and unanswered.
        pane.refresh_if_inactive(&rpc);
        assert_no_request(&mut rx, "a second entry while loading must not refetch").await;
        assert!(pane.is_loading());

        outbound.dispatch_response(&id, Some(catalog_body()), None);
        answer_instance_request(&mut rx, &outbound).await;
        settle(&mut pane).await;
        assert_eq!(pane.data, Some(catalog()));
        assert_eq!(pane.instances, InstanceState::Loaded(Vec::new()));

        pane.refresh_if_inactive(&rpc);
        tokio::task::yield_now().await;
        assert!(
            !pane.is_loading(),
            "a loaded catalog is not refetched on re-entry"
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn refresh_key_refetches_and_is_ignored_while_loading() {
        let (rpc, outbound, mut rx) = client();
        let mut pane = PluginsPane::new();
        pane.refresh_if_inactive(&rpc);
        let id = receive_catalog_request(&mut rx).await;
        outbound.dispatch_response(&id, Some(catalog_body()), None);
        answer_instance_request(&mut rx, &outbound).await;
        settle(&mut pane).await;

        assert_eq!(
            press(&mut pane, KeyCode::Char('r')),
            PluginsKeyOutcome::RefreshRequested
        );
        pane.refresh(&rpc);
        let id = receive_catalog_request(&mut rx).await;
        // The refresh is on the wire and unanswered: another r is ignored
        // rather than restarting the fetch.
        assert_eq!(
            press(&mut pane, KeyCode::Char('r')),
            PluginsKeyOutcome::RefreshRequested
        );
        pane.refresh(&rpc);
        assert_no_request(
            &mut rx,
            "refresh while loading must not send a second request",
        )
        .await;
        assert!(pane.is_loading());
        assert_eq!(
            outbound.pending_count(),
            1,
            "the first request stays pending"
        );

        // The previous rows stay on screen, marked as refreshing.
        assert!(pane.data.is_some());
        let rows = render_rows(&mut pane, 100, 20);
        assert!(
            rows.iter()
                .any(|row| row.contains("Packages (refreshing…)")),
            "{rows:#?}"
        );
        assert!(rows.iter().any(|row| row.contains("calendar")), "{rows:#?}");

        let mut body = catalog_body();
        body["plugins"].as_array_mut().unwrap().truncate(1);
        outbound.dispatch_response(&id, Some(body), None);
        answer_instance_request(&mut rx, &outbound).await;
        settle(&mut pane).await;
        assert_eq!(pane.plugins().len(), 1);
        let rows = render_rows(&mut pane, 100, 20);
        assert!(
            !rows.iter().any(|row| row.contains("refreshing")),
            "{rows:#?}"
        );
    }

    #[tokio::test]
    async fn failed_refresh_replaces_stale_rows_with_the_error() {
        let (rpc, outbound, mut rx) = client();
        let mut pane = PluginsPane::new();
        pane.refresh_if_inactive(&rpc);
        let id = receive_catalog_request(&mut rx).await;
        outbound.dispatch_response(&id, Some(catalog_body()), None);
        answer_instance_request(&mut rx, &outbound).await;
        settle(&mut pane).await;
        assert_eq!(
            press(&mut pane, KeyCode::Right),
            PluginsKeyOutcome::Consumed
        );
        assert_eq!(
            press(&mut pane, KeyCode::Enter),
            PluginsKeyOutcome::Consumed
        );
        assert_eq!(pane.focus, Focus::Detail);

        assert_eq!(
            press(&mut pane, KeyCode::Char('r')),
            PluginsKeyOutcome::RefreshRequested
        );
        assert_eq!(pane.focus, Focus::Detail, "refresh does not move focus");
        pane.refresh(&rpc);
        let id = receive_catalog_request(&mut rx).await;
        outbound.dispatch_response(
            &id,
            None,
            Some(rpc_error(
                error_codes::INTERNAL_ERROR,
                "plugin catalog discovery failed",
            )),
        );
        settle(&mut pane).await;

        assert!(
            pane.data.is_none(),
            "stale rows must not survive a failed refresh"
        );
        assert_eq!(
            pane.instances,
            InstanceState::NotLoaded,
            "nor do the instances read with them"
        );
        assert_no_request(&mut rx, "a failed catalog reads no instances").await;
        assert_eq!(
            pane.error,
            Some(CatalogError::Other(
                "plugin catalog discovery failed".to_string()
            ))
        );
        assert_eq!(
            pane.focus,
            Focus::Filters,
            "no package list is left on screen to hold focus"
        );
        let rows = render_rows(&mut pane, 100, 20);
        assert!(
            !rows.iter().any(|row| row.contains("calendar")),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("Could not load the plugin catalog: plugin catalog")),
            "{rows:#?}"
        );

        // A held error does not auto-retry on re-entry; only a refresh does.
        pane.refresh_if_inactive(&rpc);
        tokio::task::yield_now().await;
        assert!(rx.try_recv().is_err());
        pane.refresh(&rpc);
        assert!(pane.error.is_none(), "a new fetch shows the loading state");
        let _id = receive_catalog_request(&mut rx).await;
    }

    #[tokio::test]
    async fn rpc_errors_map_to_their_messages() {
        let cases = [
            (
                rpc_error(
                    error_codes::METHOD_NOT_FOUND,
                    "Unknown method: plugins/list",
                ),
                CatalogError::Unsupported,
                "This daemon does not provide the plugin catalog",
            ),
            (
                rpc_error(
                    error_codes::FORBIDDEN,
                    "Principal is not granted plugins:read (required by plugins/list)",
                ),
                CatalogError::Forbidden(
                    "Principal is not granted plugins:read (required by plugins/list)".to_string(),
                ),
                "The daemon refused the plugin catalog request: Principal is not granted \
                 plugins:read",
            ),
            // The daemon's auth gate also answers forbidden when its policy
            // is broken; the pane shows that reason, not a grant it guessed.
            (
                rpc_error(
                    error_codes::FORBIDDEN,
                    "Authentication is misconfigured on this daemon (fail closed)",
                ),
                CatalogError::Forbidden(
                    "Authentication is misconfigured on this daemon (fail closed)".to_string(),
                ),
                "Authentication is misconfigured on this daemon (fail closed)",
            ),
            (
                rpc_error(
                    error_codes::INTERNAL_ERROR,
                    "another plugin catalog scan is running; retry",
                ),
                CatalogError::Other("another plugin catalog scan is running; retry".to_string()),
                "another plugin catalog scan is running; retry",
            ),
        ];
        for (error, expected, text) in cases {
            let (rpc, calls) = responding_client(Err(error));
            let mut pane = PluginsPane::new();
            pane.refresh_if_inactive(&rpc);
            settle(&mut pane).await;
            assert_eq!(pane.error.as_ref(), Some(&expected));
            assert_eq!(calls.lock().unwrap().as_slice(), ["plugins/list"]);
            // Held only around the synchronous render, never across an await.
            let _keymap = default_keymap();
            let rows = render_rows(&mut pane, 160, 20);
            assert!(
                rows.iter().any(|row| row.contains(text)),
                "{text}: {rows:#?}"
            );
            assert!(
                !rows
                    .iter()
                    .any(|row| row.contains("lacks the plugins:read grant")),
                "{rows:#?}"
            );
            assert!(
                rows.iter().any(|row| row.contains("Press r to retry.")),
                "{rows:#?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_unanswered_request_times_out_into_its_own_state() {
        let (rpc, _outbound, mut rx) = client();
        let mut pane = PluginsPane::new();
        pane.refresh_if_inactive(&rpc);
        let _id = receive_catalog_request(&mut rx).await;

        // Just under the 20 s budget the fetch is still running; the 5 s
        // default of a plain call would have failed long before.
        tokio::time::sleep(Duration::from_secs(19)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
            pane.poll_refresh().await;
        }
        assert!(pane.is_loading(), "the fetch must still run at 19 s");
        assert!(pane.error.is_none(), "{:?}", pane.error);

        tokio::time::sleep(Duration::from_secs(2)).await;
        settle(&mut pane).await;

        assert_eq!(pane.error, Some(CatalogError::TimedOut));
        let rows = render_rows(&mut pane, 120, 20);
        assert!(
            rows.iter()
                .any(|row| row.contains("The plugin catalog request timed out")),
            "{rows:#?}"
        );
    }

    #[tokio::test]
    async fn dropping_the_pane_aborts_the_pending_request() {
        let (rpc, outbound, mut rx) = client();
        let mut pane = PluginsPane::new();
        pane.refresh_if_inactive(&rpc);
        let _id = receive_catalog_request(&mut rx).await;
        assert_eq!(outbound.pending_count(), 1);

        drop(pane);
        for _ in 0..100 {
            if outbound.pending_count() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            outbound.pending_count(),
            0,
            "dropping the pane must cancel its pending plugins/list request"
        );
    }

    // ── Channel instances ────────────────────────────────────────

    use super::fake_daemon::{self, Declaration, Request, State, declaration};
    use crate::keymap::GlobalAction;

    /// An installed channel package (`chat`) and a registry-only lookalike
    /// of it (`chat `), a registry-only channel package (`mail`), an
    /// installed channel package with no instance (`relay`), and an
    /// installed package that provides no channel (`calendar`).
    fn channel_catalog_body() -> Value {
        json!({
            "plugins_enabled": true,
            "wasm_plugins_available": true,
            "plugins_dir": "/tmp/plugins",
            "plugins": [
                {
                    "name": "chat",
                    "installed": {
                        "version": "0.1.0",
                        "description": "Chat bridge",
                        "capabilities": ["channel"],
                        "permissions": []
                    },
                    "available": null
                },
                {
                    "name": "calendar",
                    "installed": {
                        "version": "0.1.0",
                        "description": null,
                        "capabilities": ["tool"],
                        "permissions": []
                    },
                    "available": null
                },
                {
                    "name": "mail",
                    "installed": null,
                    "available": {
                        "version": "1.2.3",
                        "description": null,
                        "capabilities": ["channel"],
                        "install_source": "mail@1.2.3"
                    }
                },
                {
                    "name": "chat ",
                    "installed": null,
                    "available": {
                        "version": "9.9.9",
                        "description": null,
                        "capabilities": ["channel"],
                        "install_source": "chat@9.9.9"
                    }
                },
                {
                    "name": "relay",
                    "installed": {
                        "version": "0.2.0",
                        "description": null,
                        "capabilities": ["channel"],
                        "permissions": []
                    },
                    "available": null
                }
            ],
            "issues": []
        })
    }

    fn channel_catalog() -> PluginsListResult {
        serde_json::from_value(channel_catalog_body()).unwrap()
    }

    /// Listed out of alias order, as the daemon's map order may be. `orphan`
    /// names a package the catalog does not list.
    fn channel_declarations() -> Vec<Declaration> {
        vec![
            declaration("ops", "chat", "true"),
            declaration("lookalike", "chat ", "true"),
            declaration("alerts", "chat", "false"),
            declaration("inbox", "mail", "true"),
            declaration("orphan", "gone", "true"),
        ]
    }

    fn channel_state() -> State {
        State::new(Ok(channel_catalog_body())).with(channel_declarations())
    }

    type Served = (
        PluginsPane,
        Arc<RpcClient>,
        Arc<RpcOutbound>,
        Arc<Mutex<State>>,
    );

    /// A pane that has read the catalog and the instances from `state`.
    async fn served_pane(state: State) -> Served {
        let (rpc, outbound, daemon) = fake_daemon::serve(state);
        let mut pane = PluginsPane::new();
        pane.refresh_if_inactive(&rpc);
        settle(&mut pane).await;
        (pane, rpc, outbound, daemon)
    }

    fn request(method: &str, params: Value) -> Request {
        Request {
            method: method.to_string(),
            params,
        }
    }

    fn requests(daemon: &Arc<Mutex<State>>) -> Vec<Request> {
        daemon.lock().unwrap().requests.clone()
    }

    fn methods(daemon: &Arc<Mutex<State>>) -> Vec<String> {
        daemon.lock().unwrap().methods()
    }

    fn rpc_failure(code: i32, message: &str) -> Option<JsonRpcError> {
        Some(rpc_error(code, message))
    }

    /// Open `package`'s detail, move into its instance list and select
    /// `alias`.
    fn focus_instance(pane: &mut PluginsPane, package: &str, alias: &str) {
        select_and_open(pane, package);
        assert_eq!(press(pane, KeyCode::Enter), PluginsKeyOutcome::Consumed);
        assert_eq!(pane.focus, Focus::Instances, "{package} has instances");
        let row = pane
            .selected_instances()
            .iter()
            .position(|instance| instance.alias == alias)
            .unwrap();
        pane.instance_state.select(Some(row));
    }

    async fn settle_toggle(pane: &mut PluginsPane) {
        for _ in 0..200 {
            tokio::task::yield_now().await;
            pane.poll_toggle().await;
            if !pane.is_toggling() {
                return;
            }
        }
        panic!("the toggle never finished");
    }

    /// Press Enter on the focused instance and run the toggle it asks for.
    async fn toggle(pane: &mut PluginsPane, rpc: &Arc<RpcClient>) {
        assert_eq!(
            press(pane, KeyCode::Enter),
            PluginsKeyOutcome::ToggleRequested
        );
        pane.start_toggle(rpc);
        assert!(pane.is_toggling());
        settle_toggle(pane).await;
    }

    /// The toggle status as shown, under the default keymap.
    fn status_text(pane: &PluginsPane) -> String {
        let _keymap = default_keymap();
        pane.toggle_status
            .as_ref()
            .map(ToggleStatus::text)
            .unwrap_or_default()
    }

    fn default_reload_chord() -> String {
        let _keymap = default_keymap();
        first_chord(GlobalAction::ReloadDaemon)
    }

    fn enabled_of(pane: &PluginsPane, alias: &str) -> Option<bool> {
        match &pane.instances {
            InstanceState::Loaded(instances) => {
                instances
                    .iter()
                    .find(|instance| instance.alias == alias)
                    .unwrap_or_else(|| panic!("no instance {alias}"))
                    .enabled
            }
            other => panic!("instances not loaded: {other:?}"),
        }
    }

    fn block_of(pane: &PluginsPane, package: &str) -> Option<InstanceBlock> {
        let entry = pane
            .plugins()
            .iter()
            .find(|entry| entry.name == package)
            .unwrap();
        let _keymap = default_keymap();
        pane.instance_block(entry)
    }

    fn rows_of(rows: &[(&str, Option<bool>)]) -> Vec<(String, Option<bool>)> {
        rows.iter()
            .map(|(alias, enabled)| (alias.to_string(), *enabled))
            .collect()
    }

    /// The drawn rows flowed into one line with box-drawing borders and runs
    /// of whitespace collapsed, so a wrapped sentence can be found whole.
    fn flowing(rows: &[String]) -> String {
        rows.iter()
            .flat_map(|row| row.split(|c: char| ('\u{2500}'..='\u{257f}').contains(&c)))
            .flat_map(str::split_whitespace)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn field_row(path: &str, value: Option<Value>) -> ConfigFieldEntry {
        ConfigFieldEntry {
            path: path.to_string(),
            category: "channels".to_string(),
            kind: crate::wire::PropKind::String,
            type_hint: "String".to_string(),
            value,
            populated: true,
            is_secret: false,
            is_env_overridden: false,
            enum_variants: Vec::new(),
            description: String::new(),
            section: Some("channels".to_string()),
            tab: Default::default(),
            alias_source: None,
        }
    }

    #[test]
    fn instance_rows_parse_into_sorted_instances_with_unknown_values() {
        let text = |value: &str| Some(json!(value));
        let rows = vec![
            field_row("channels.plugin.zed.package", text("chat")),
            field_row("channels.plugin.zed.enabled", text("true")),
            field_row("channels.plugin.alpha.enabled", text("false")),
            field_row("channels.plugin.alpha.package", text("chat")),
            field_row("channels.plugin.garbage.package", text("chat")),
            field_row("channels.plugin.garbage.enabled", text("True")),
            field_row("channels.plugin.missing.package", text("chat")),
            // The daemon sends every value as a string; anything else is
            // not a value this pane can read.
            field_row("channels.plugin.boolean.package", text("chat")),
            field_row("channels.plugin.boolean.enabled", Some(json!(true))),
            field_row("channels.plugin.bare.enabled", None),
            // Other tables, other fields, and paths with no field.
            field_row("channels.plugins.x.package", text("chat")),
            field_row("channels.pluginx.y.package", text("chat")),
            field_row("channels.telegram.z.enabled", text("true")),
            field_row("channels.plugin.extra.webhook_path", text("/hook")),
            field_row("channels.plugin.nofield", text("x")),
            field_row("channels.plugin", text("x")),
        ];
        let instance = |alias: &str, package: Option<&str>, enabled| ChannelInstance {
            alias: alias.to_string(),
            package: package.map(str::to_string),
            enabled,
        };
        assert_eq!(
            parse_instances(&rows),
            vec![
                instance("alpha", Some("chat"), Some(false)),
                instance("bare", None, None),
                instance("boolean", Some("chat"), None),
                instance("garbage", Some("chat"), None),
                instance("missing", Some("chat"), None),
                instance("zed", Some("chat"), Some(true)),
            ]
        );
        assert_eq!(parse_instances(&[]), Vec::new());

        // An alias outside the config grammar is quoted, so it never reads
        // as a valid one.
        assert_eq!(display_alias("ops_2"), "ops_2");
        assert_eq!(display_alias("Ops"), r#""Ops""#);
        assert_eq!(display_alias("ops "), r#""ops\u{20}""#);
        assert_eq!(display_alias("a.b"), r#""a.b""#);
        assert_eq!(display_alias(""), r#""""#);
        assert_eq!(
            display_alias(&"a".repeat(64)),
            format!("\"{}\"", "a".repeat(64))
        );
    }

    #[tokio::test]
    async fn instances_appear_only_under_the_package_they_name() {
        let (mut pane, ..) = served_pane(channel_state()).await;
        assert_eq!(
            block_of(&pane, "chat").unwrap().rows,
            rows_of(&[("alerts", Some(false)), ("ops", Some(true))]),
            "sorted by alias; the lookalike package's instance is not here"
        );
        assert_eq!(
            block_of(&pane, "chat ").unwrap().rows,
            rows_of(&[("lookalike", Some(true))])
        );
        // Named by an instance, so shown although it is not installed.
        assert_eq!(
            block_of(&pane, "mail").unwrap().rows,
            rows_of(&[("inbox", Some(true))])
        );
        // An installed channel package with no instance gets an empty block.
        assert_eq!(block_of(&pane, "relay").unwrap().rows, Vec::new());
        assert_eq!(block_of(&pane, "calendar"), None);
        // The orphan names no listed package, so it shows nowhere.
        for entry in pane.plugins() {
            let rows = block_of(&pane, &entry.name)
                .map(|b| b.rows)
                .unwrap_or_default();
            assert!(rows.iter().all(|(alias, _)| alias != "orphan"), "{rows:?}");
        }

        let _keymap = default_keymap();
        select_and_open(&mut pane, "chat");
        let rows = render_rows(&mut pane, 100, 30);
        let text = flowing(&rows);
        assert!(text.contains("Channel instances"), "{rows:#?}");
        assert!(text.contains("alerts disabled in config"), "{rows:#?}");
        assert!(text.contains("ops enabled in config"), "{rows:#?}");
        assert!(!text.contains("lookalike"), "{rows:#?}");
        assert!(
            text.contains(
                "An enabled instance starts after a daemon reload only if [plugins] enabled \
                 is on and an enabled agent lists plugin.<alias> in its channels."
            ),
            "{rows:#?}"
        );

        select_and_open(&mut pane, "relay");
        let text = flowing(&render_rows(&mut pane, 100, 30));
        assert!(
            text.contains("No channel instance in config names this package."),
            "{text}"
        );

        select_and_open(&mut pane, "calendar");
        let text = flowing(&render_rows(&mut pane, 100, 30));
        assert!(!text.contains("Channel instances"), "{text}");
    }

    #[tokio::test]
    async fn one_refresh_reads_the_catalog_then_the_instances() {
        let (mut pane, rpc, _outbound, daemon) = served_pane(channel_state()).await;
        assert_eq!(
            requests(&daemon),
            vec![
                request("plugins/list", json!({})),
                request("config/list", json!({ "prefix": "channels.plugin" })),
            ]
        );
        let aliases: Vec<&str> = match &pane.instances {
            InstanceState::Loaded(instances) => instances
                .iter()
                .map(|instance| instance.alias.as_str())
                .collect(),
            other => panic!("{other:?}"),
        };
        assert_eq!(aliases, ["alerts", "inbox", "lookalike", "ops", "orphan"]);

        assert_eq!(
            press(&mut pane, KeyCode::Char('r')),
            PluginsKeyOutcome::RefreshRequested
        );
        pane.refresh(&rpc);
        settle(&mut pane).await;
        assert_eq!(
            methods(&daemon),
            ["plugins/list", "config/list", "plugins/list", "config/list"],
            "r refreshes both"
        );
    }

    #[tokio::test]
    async fn an_instance_read_failure_leaves_the_catalog_and_shows_in_the_block() {
        let reason = "Principal is not granted config:read (required by config/list)";
        let mut state = channel_state();
        state.list_error = rpc_failure(error_codes::FORBIDDEN, reason);
        let (mut pane, ..) = served_pane(state).await;
        assert_eq!(pane.error, None);
        assert_eq!(pane.data, Some(channel_catalog()));
        assert_eq!(
            pane.instances,
            InstanceState::Failed(CallFailure::Forbidden(reason.to_string()))
        );

        let _keymap = default_keymap();
        let text = flowing(&render_rows(&mut pane, 100, 30));
        assert!(text.contains("\u{25cf} chat"), "{text}");

        select_and_open(&mut pane, "chat");
        let text = flowing(&render_rows(&mut pane, 100, 30));
        assert!(
            text.contains(&format!(
                "The daemon refused to list channel instances: {reason}"
            )),
            "{text}"
        );
        assert!(text.contains("Press r to refresh."), "{text}");
        assert!(
            text.contains("Name: chat"),
            "the detail still renders: {text}"
        );
        assert!(!text.contains("ops"), "{text}");
        // Nothing to move into.
        drop(_keymap);
        assert_eq!(
            press(&mut pane, KeyCode::Enter),
            PluginsKeyOutcome::Consumed
        );
        assert_eq!(pane.focus, Focus::Detail);

        // A package that provides no channel shows no block, error or not.
        assert_eq!(block_of(&pane, "calendar"), None);

        assert_eq!(
            CallFailure::TimedOut.instances_message(),
            "Reading the channel instances timed out."
        );
        assert_eq!(
            CallFailure::Other("boom".to_string()).instances_message(),
            "Could not read channel instances: boom"
        );
    }

    #[tokio::test]
    async fn a_failed_catalog_reads_no_instances() {
        let state = State::new(Err(rpc_error(error_codes::INTERNAL_ERROR, "scan failed")))
            .with(channel_declarations());
        let (pane, _rpc, _outbound, daemon) = served_pane(state).await;
        assert_eq!(methods(&daemon), ["plugins/list"]);
        assert_eq!(pane.instances, InstanceState::NotLoaded);
    }

    #[tokio::test]
    async fn a_toggle_rereads_writes_a_json_bool_and_reads_back() {
        let (mut pane, rpc, _outbound, daemon) = served_pane(channel_state()).await;
        let reload = default_reload_chord();
        assert!(!reload.is_empty());

        focus_instance(&mut pane, "chat", "ops");
        let before = requests(&daemon).len();
        toggle(&mut pane, &rpc).await;
        let sent = requests(&daemon)[before..].to_vec();
        assert_eq!(
            sent,
            vec![
                request("config/list", json!({ "prefix": "channels.plugin.ops" })),
                request(
                    "config/set",
                    json!({ "prop": "channels.plugin.ops.enabled", "value": false })
                ),
                request("config/list", json!({ "prefix": "channels.plugin.ops" })),
            ]
        );
        assert!(
            sent[1].params["value"].is_boolean(),
            "the value is a JSON bool, not a string"
        );
        assert_eq!(
            daemon
                .lock()
                .unwrap()
                .find("ops")
                .unwrap()
                .enabled
                .as_deref(),
            Some("false")
        );
        assert_eq!(enabled_of(&pane, "ops"), Some(false));
        assert_eq!(
            pane.toggle_status.as_ref().map(|status| &status.kind),
            Some(&StatusKind::Done(ToggleOutcome::Saved(Some(false))))
        );
        let text = status_text(&pane);
        assert_eq!(
            text,
            format!(
                "Saved: ops is now disabled in config. Takes effect after a daemon reload \
                 ({reload})."
            )
        );
        for word in ["start", "stop", "running", "active", "healthy"] {
            assert!(!text.to_lowercase().contains(word), "{word}: {text}");
        }

        // The other way round: a disabled instance is written true.
        assert_eq!(press(&mut pane, KeyCode::Up), PluginsKeyOutcome::Consumed);
        assert_eq!(
            pane.selected_instance().map(|i| i.alias.as_str()),
            Some("alerts")
        );
        let before = requests(&daemon).len();
        toggle(&mut pane, &rpc).await;
        assert_eq!(
            requests(&daemon)[before + 1],
            request(
                "config/set",
                json!({ "prop": "channels.plugin.alerts.enabled", "value": true })
            )
        );
        assert_eq!(enabled_of(&pane, "alerts"), Some(true));
        assert!(
            status_text(&pane).starts_with("Saved: alerts is now enabled in config."),
            "{}",
            status_text(&pane)
        );

        // The status line is drawn in the block, and only in this package's.
        let _keymap = default_keymap();
        let text = flowing(&render_rows(&mut pane, 100, 30));
        assert!(
            text.contains("Saved: alerts is now enabled in config."),
            "{text}"
        );
        assert!(text.contains("alerts enabled in config"), "{text}");
        drop(_keymap);
        select_and_open(&mut pane, "mail");
        assert_eq!(block_of(&pane, "mail").unwrap().status, None);
    }

    #[tokio::test]
    async fn a_toggle_writes_nothing_when_the_instance_changed_since_it_was_read() {
        type Change = fn(&mut State);
        let cases: [(&str, Change); 3] = [
            ("removed", |state: &mut State| {
                state.declarations.retain(|decl| decl.alias != "ops");
            }),
            ("package changed", |state: &mut State| {
                if let Some(decl) = state.declarations.iter_mut().find(|d| d.alias == "ops") {
                    decl.package = Some("chat-v2".to_string());
                }
            }),
            ("value changed", |state: &mut State| {
                if let Some(decl) = state.declarations.iter_mut().find(|d| d.alias == "ops") {
                    decl.enabled = Some("false".to_string());
                }
            }),
        ];
        for (name, change) in cases {
            let (mut pane, rpc, _outbound, daemon) = served_pane(channel_state()).await;
            focus_instance(&mut pane, "chat", "ops");
            change(&mut daemon.lock().unwrap());
            let declarations = daemon.lock().unwrap().declarations.clone();
            let before = requests(&daemon).len();
            toggle(&mut pane, &rpc).await;
            assert_eq!(
                requests(&daemon)[before..].to_vec(),
                vec![request(
                    "config/list",
                    json!({ "prefix": "channels.plugin.ops" })
                )],
                "{name}: no config/set"
            );
            assert_eq!(
                daemon.lock().unwrap().declarations,
                declarations,
                "{name}: nothing written, and a removed alias is not re-created"
            );
            assert_eq!(enabled_of(&pane, "ops"), Some(true), "{name}");
            assert_eq!(
                status_text(&pane),
                "ops was not changed: it changed since it was read. Press r to refresh.",
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn a_refused_or_failed_write_shows_the_reason_and_keeps_the_value() {
        // Refused: the daemon checks write authority before staging, so
        // nothing is read back.
        let reason =
            "Principal is not granted config write access to \"channels.plugin.ops.enabled\"";
        let mut state = channel_state();
        state.set_error = rpc_failure(error_codes::FORBIDDEN, reason);
        let (mut pane, rpc, _outbound, daemon) = served_pane(state).await;
        focus_instance(&mut pane, "chat", "ops");
        let before = methods(&daemon).len();
        toggle(&mut pane, &rpc).await;
        assert_eq!(methods(&daemon)[before..], ["config/list", "config/set"]);
        assert_eq!(
            status_text(&pane),
            format!("ops was not changed: not permitted ({reason}).")
        );
        assert_eq!(enabled_of(&pane, "ops"), Some(true));
        assert_eq!(
            daemon
                .lock()
                .unwrap()
                .find("ops")
                .unwrap()
                .enabled
                .as_deref(),
            Some("true")
        );

        // Any other failure: the read-back confirms the value is unchanged,
        // and the daemon's message is shown sanitized.
        let mut state = channel_state();
        state.set_error = rpc_failure(
            error_codes::INTERNAL_ERROR,
            "Config save failed: disk\u{1b}[2J full",
        );
        let (mut pane, rpc, _outbound, daemon) = served_pane(state).await;
        focus_instance(&mut pane, "chat", "ops");
        let before = methods(&daemon).len();
        toggle(&mut pane, &rpc).await;
        assert_eq!(
            methods(&daemon)[before..],
            ["config/list", "config/set", "config/list"]
        );
        assert_eq!(
            status_text(&pane),
            "ops was not changed: Config save failed: disk\u{fffd}[2J full"
        );
        assert_eq!(enabled_of(&pane, "ops"), Some(true));

        // The re-read before the write fails: nothing is written.
        let (mut pane, rpc, _outbound, daemon) = served_pane(channel_state()).await;
        focus_instance(&mut pane, "chat", "ops");
        daemon.lock().unwrap().list_error =
            rpc_failure(error_codes::INTERNAL_ERROR, "config is busy");
        let before = methods(&daemon).len();
        toggle(&mut pane, &rpc).await;
        assert_eq!(methods(&daemon)[before..], ["config/list"]);
        assert_eq!(status_text(&pane), "ops was not changed: config is busy");
        assert_eq!(enabled_of(&pane, "ops"), Some(true));
    }

    #[tokio::test(start_paused = true)]
    async fn a_write_with_no_answer_is_reported_as_unconfirmed() {
        let mut state = channel_state();
        state.hold_set = true;
        let (mut pane, rpc, _outbound, daemon) = served_pane(state).await;
        focus_instance(&mut pane, "chat", "ops");
        assert_eq!(
            press(&mut pane, KeyCode::Enter),
            PluginsKeyOutcome::ToggleRequested
        );
        pane.start_toggle(&rpc);
        assert_eq!(status_text(&pane), "Saving ops\u{2026}");

        tokio::time::sleep(Duration::from_secs(4)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
            pane.poll_toggle().await;
        }
        assert!(pane.is_toggling(), "the write is still waiting at 4 s");

        tokio::time::sleep(Duration::from_secs(2)).await;
        settle_toggle(&mut pane).await;
        assert_eq!(
            methods(&daemon)[2..],
            ["config/list", "config/set"],
            "a read now could show the old value of a write that lands later"
        );
        assert_eq!(
            status_text(&pane),
            "ops may or may not have changed (the daemon did not answer in time). \
             Press r to refresh."
        );
        assert_eq!(enabled_of(&pane, "ops"), None, "the value is now unknown");
    }

    #[tokio::test]
    async fn a_write_whose_result_is_unclear_shows_what_was_read_back() {
        // The write landed but its reply was lost.
        let mut state = channel_state();
        state.set_error = rpc_failure(error_codes::INTERNAL_ERROR, "Outbound RPC dropped");
        state.apply_failed_set = true;
        let (mut pane, rpc, _outbound, _daemon) = served_pane(state).await;
        focus_instance(&mut pane, "chat", "ops");
        toggle(&mut pane, &rpc).await;
        assert_eq!(
            status_text(&pane),
            "ops may or may not have changed (Outbound RPC dropped). Press r to refresh."
        );
        assert_eq!(enabled_of(&pane, "ops"), Some(false), "the value read back");

        // The write was saved but could not be read back.
        let reload = default_reload_chord();
        let mut state = channel_state();
        state.list_error_after_set = rpc_failure(error_codes::INTERNAL_ERROR, "busy");
        let (mut pane, rpc, _outbound, _daemon) = served_pane(state).await;
        focus_instance(&mut pane, "chat", "ops");
        toggle(&mut pane, &rpc).await;
        assert_eq!(
            status_text(&pane),
            format!(
                "Saved ops, but its stored value could not be read back. Press r to refresh. \
                 Takes effect after a daemon reload ({reload})."
            )
        );
        assert_eq!(enabled_of(&pane, "ops"), None);
    }

    #[tokio::test]
    async fn enter_on_an_unknown_value_writes_nothing() {
        let state = State::new(Ok(channel_catalog_body())).with(vec![
            declaration("odd", "chat", "yes"),
            Declaration {
                alias: "partial".to_string(),
                package: Some("chat".to_string()),
                enabled: None,
            },
        ]);
        let (mut pane, rpc, _outbound, daemon) = served_pane(state).await;
        for alias in ["odd", "partial"] {
            focus_instance(&mut pane, "chat", alias);
            assert_eq!(enabled_of(&pane, alias), None);
            assert_eq!(
                press(&mut pane, KeyCode::Enter),
                PluginsKeyOutcome::Consumed
            );
            pane.start_toggle(&rpc);
            assert!(!pane.is_toggling());
            assert_eq!(
                status_text(&pane),
                format!("{alias} has no known value to toggle. Press r to refresh.")
            );
        }
        assert_eq!(methods(&daemon), ["plugins/list", "config/list"]);
        let _keymap = default_keymap();
        let text = flowing(&render_rows(&mut pane, 100, 30));
        assert!(text.contains("odd unknown"), "{text}");
    }

    #[test]
    fn the_alias_grammar_matches_the_daemon() {
        for alias in ["a", "0", "ops", "matrix_room", "a1_b2"] {
            assert!(is_grammar_alias(alias), "{alias:?}");
        }
        assert!(is_grammar_alias(&"a".repeat(63)));
        for alias in ["", "a.b", "A", "_a", "a_", "a__b", "a-b", "a b", "\u{e9}"] {
            assert!(!is_grammar_alias(alias), "{alias:?}");
        }
        assert!(!is_grammar_alias(&"a".repeat(64)));
        assert_eq!(display_alias("ops"), "ops");
        assert_eq!(display_alias("a.b"), "\"a.b\"");
    }

    /// The daemon resolves `channels.plugin.a.b.enabled` by the first path
    /// segment, so a write to a hand-edited `a.b` would create a second,
    /// package-less instance `a`. Neither Enter nor the toggle task writes.
    #[tokio::test]
    async fn an_alias_outside_the_grammar_is_never_written() {
        let state =
            State::new(Ok(channel_catalog_body())).with(vec![declaration("a.b", "chat", "true")]);
        let (mut pane, rpc, _outbound, daemon) = served_pane(state).await;
        focus_instance(&mut pane, "chat", "a.b");
        assert_eq!(
            press(&mut pane, KeyCode::Enter),
            PluginsKeyOutcome::Consumed
        );
        pane.start_toggle(&rpc);
        assert!(!pane.is_toggling());
        assert_eq!(
            status_text(&pane),
            "\"a.b\" was not changed: its name is outside the alias grammar, \
             so it can only be fixed in the config file."
        );

        let target = ToggleTarget {
            alias: "a.b".to_string(),
            package: "chat".to_string(),
            shown: true,
        };
        assert_eq!(
            run_toggle(Arc::clone(&rpc), target).await,
            ToggleOutcome::NotChanged(NotChanged::InvalidAlias)
        );
        assert_eq!(methods(&daemon), ["plugins/list", "config/list"]);
    }

    #[tokio::test]
    async fn one_toggle_at_a_time_and_dropping_the_pane_aborts_it() {
        let mut state = channel_state();
        state.hold_set = true;
        let (mut pane, rpc, outbound, daemon) = served_pane(state).await;
        focus_instance(&mut pane, "chat", "ops");
        assert_eq!(
            press(&mut pane, KeyCode::Enter),
            PluginsKeyOutcome::ToggleRequested
        );
        pane.start_toggle(&rpc);
        for _ in 0..200 {
            if methods(&daemon).iter().any(|m| m == "config/set") {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(outbound.pending_count(), 1, "the write is in flight");

        // Further toggles, on either instance, ask for nothing.
        assert_eq!(
            press(&mut pane, KeyCode::Enter),
            PluginsKeyOutcome::Consumed
        );
        assert_eq!(press(&mut pane, KeyCode::Up), PluginsKeyOutcome::Consumed);
        assert_eq!(
            press(&mut pane, KeyCode::Enter),
            PluginsKeyOutcome::Consumed
        );
        pane.start_toggle(&rpc);
        // Even a target held from before is dropped while one runs.
        pane.pending_toggle = Some(ToggleTarget {
            alias: "alerts".to_string(),
            package: "chat".to_string(),
            shown: false,
        });
        pane.start_toggle(&rpc);
        assert_eq!(pane.pending_toggle, None);
        // A refresh waits too, so its read cannot land after the write.
        assert_eq!(
            press(&mut pane, KeyCode::Char('r')),
            PluginsKeyOutcome::RefreshRequested
        );
        pane.refresh(&rpc);
        assert!(!pane.is_loading());
        for _ in 0..20 {
            tokio::task::yield_now().await;
            pane.poll_toggle().await;
        }
        assert_eq!(
            methods(&daemon),
            ["plugins/list", "config/list", "config/list", "config/set"]
        );
        assert!(pane.is_toggling());

        drop(pane);
        for _ in 0..100 {
            if outbound.pending_count() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            outbound.pending_count(),
            0,
            "dropping the pane must cancel its pending config/set"
        );
    }

    #[tokio::test]
    async fn focus_reaches_the_instances_only_when_there_are_some() {
        let (mut pane, _rpc, _outbound, daemon) = served_pane(channel_state()).await;

        // No block, and an empty block: Enter and the inward chord stay put.
        for package in ["calendar", "relay"] {
            select_and_open(&mut pane, package);
            for code in [KeyCode::Enter, KeyCode::Right] {
                assert_eq!(press(&mut pane, code), PluginsKeyOutcome::Consumed);
                assert_eq!(pane.focus, Focus::Detail, "{package}");
            }
            assert_eq!(pane.pending_toggle, None);
        }

        select_and_open(&mut pane, "chat");
        assert_eq!(
            press(&mut pane, KeyCode::Right),
            PluginsKeyOutcome::Consumed
        );
        assert_eq!(pane.focus, Focus::Instances);
        let selected = |pane: &PluginsPane| pane.selected_instance().map(|i| i.alias.clone());
        assert_eq!(selected(&pane).as_deref(), Some("alerts"));
        press(&mut pane, KeyCode::Down);
        assert_eq!(selected(&pane).as_deref(), Some("ops"));
        press(&mut pane, KeyCode::Down);
        assert_eq!(selected(&pane).as_deref(), Some("ops"), "Down clamps");
        // The inward chord never toggles.
        assert_eq!(
            press(&mut pane, KeyCode::Right),
            PluginsKeyOutcome::Consumed
        );
        assert_eq!(pane.pending_toggle, None);
        assert_eq!(press(&mut pane, KeyCode::Esc), PluginsKeyOutcome::Consumed);
        assert_eq!(pane.focus, Focus::Detail);
        assert_eq!(
            press(&mut pane, KeyCode::Enter),
            PluginsKeyOutcome::Consumed
        );
        assert_eq!(pane.focus, Focus::Instances);
        assert_eq!(
            selected(&pane).as_deref(),
            Some("ops"),
            "the cursor is kept"
        );
        assert_eq!(press(&mut pane, KeyCode::Left), PluginsKeyOutcome::Consumed);
        assert_eq!(pane.focus, Focus::Detail);
        assert_eq!(press(&mut pane, KeyCode::Esc), PluginsKeyOutcome::Consumed);
        assert_eq!(pane.focus, Focus::Packages);

        // Opening another package starts its list at the top.
        select_and_open(&mut pane, "mail");
        assert_eq!(selected(&pane).as_deref(), Some("inbox"));
        assert_eq!(methods(&daemon), ["plugins/list", "config/list"]);
    }

    #[test]
    fn instance_focus_falls_back_when_a_refresh_removes_what_it_rests_on() {
        let instance = |alias: &str, package: &str| ChannelInstance {
            alias: alias.to_string(),
            package: Some(package.to_string()),
            enabled: Some(true),
        };
        let fetch =
            |catalog: Result<PluginsListResult, CatalogError>,
             instances: Option<Result<Vec<ChannelInstance>, CallFailure>>| {
                FetchResult { catalog, instances }
            };
        let loaded =
            |instances: Vec<ChannelInstance>| fetch(Ok(channel_catalog()), Some(Ok(instances)));
        let enter = |pane: &mut PluginsPane, alias: &str| {
            focus_instance(pane, "chat", alias);
        };

        let mut pane = PluginsPane::new();
        pane.apply_fetch(loaded(vec![
            instance("alerts", "chat"),
            instance("ops", "chat"),
        ]));
        enter(&mut pane, "ops");

        // A new alias that sorts first: the cursor follows ops.
        pane.apply_fetch(loaded(vec![
            instance("aaa", "chat"),
            instance("alerts", "chat"),
            instance("ops", "chat"),
        ]));
        assert_eq!(pane.focus, Focus::Instances);
        assert_eq!(
            pane.selected_instance().map(|i| i.alias.as_str()),
            Some("ops")
        );

        // ops is gone: the cursor clamps onto a remaining row.
        pane.apply_fetch(loaded(vec![
            instance("aaa", "chat"),
            instance("alerts", "chat"),
        ]));
        assert_eq!(pane.focus, Focus::Instances);
        assert_eq!(
            pane.selected_instance().map(|i| i.alias.as_str()),
            Some("alerts")
        );

        // No instance left for chat: back to the detail.
        pane.apply_fetch(loaded(vec![instance("ops", "mail")]));
        assert_eq!(pane.focus, Focus::Detail);
        assert_eq!(pane.selected_instance(), None);

        // The instance read fails: back to the detail.
        pane.apply_fetch(loaded(vec![instance("ops", "chat")]));
        enter(&mut pane, "ops");
        pane.apply_fetch(fetch(
            Ok(channel_catalog()),
            Some(Err(CallFailure::TimedOut)),
        ));
        assert_eq!(pane.focus, Focus::Detail);

        // The package is gone: back to the packages.
        pane.apply_fetch(loaded(vec![instance("ops", "chat")]));
        enter(&mut pane, "ops");
        let mut without_chat = channel_catalog();
        without_chat.plugins.retain(|entry| entry.name != "chat");
        pane.apply_fetch(fetch(
            Ok(without_chat),
            Some(Ok(vec![instance("ops", "chat")])),
        ));
        assert_eq!(pane.focus, Focus::Packages);

        // The catalog fails: back to the filters.
        pane.apply_fetch(loaded(vec![instance("ops", "chat")]));
        enter(&mut pane, "ops");
        pane.apply_fetch(fetch(Err(CatalogError::TimedOut), None));
        assert_eq!(pane.focus, Focus::Filters);
        assert_eq!(pane.instances, InstanceState::NotLoaded);
    }

    /// The pane's only write is `config/set` on one instance's `enabled`,
    /// with a JSON bool, and only after Enter in the instance list; every
    /// read is the catalog or the instance table.
    #[tokio::test]
    async fn the_only_write_is_an_instance_toggle_after_the_toggle_key() {
        let (mut pane, rpc, _outbound, daemon) = served_pane(channel_state()).await;
        let allowed = |request: &Request| {
            let alias_only = |alias: &str| !alias.is_empty() && !alias.contains('.');
            match request.method.as_str() {
                "plugins/list" => request.params == json!({}),
                "config/list" => request.params["prefix"].as_str().is_some_and(|prefix| {
                    prefix == "channels.plugin"
                        || prefix
                            .strip_prefix("channels.plugin.")
                            .is_some_and(alias_only)
                }),
                "config/set" => {
                    request.params["value"].is_boolean()
                        && request.params["prop"].as_str().is_some_and(|prop| {
                            prop.strip_prefix("channels.plugin.")
                                .and_then(|rest| rest.strip_suffix(".enabled"))
                                .is_some_and(alias_only)
                        })
                }
                _ => false,
            }
        };

        // Every focus, every key that edits in other panes, and a refresh.
        let keys = [
            KeyCode::Right,
            KeyCode::Enter,
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Right,
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Right,
            KeyCode::Char('d'),
            KeyCode::Char('x'),
            KeyCode::Char('t'),
            KeyCode::Char('/'),
            KeyCode::Left,
            KeyCode::Esc,
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Right,
            KeyCode::Enter,
            KeyCode::Char('d'),
            KeyCode::Esc,
            KeyCode::Esc,
            KeyCode::Down,
            KeyCode::Char('r'),
            KeyCode::Up,
        ];
        let mut saw_instances = false;
        for code in keys {
            match press(&mut pane, code) {
                PluginsKeyOutcome::RefreshRequested => pane.refresh(&rpc),
                PluginsKeyOutcome::ToggleRequested => panic!("{code:?} asked for a toggle"),
                PluginsKeyOutcome::Consumed | PluginsKeyOutcome::NotConsumed => {}
            }
            saw_instances |= pane.focus == Focus::Instances;
            settle(&mut pane).await;
        }
        assert!(saw_instances, "the sweep reached the instance list");
        {
            let _keymap = default_keymap();
            let _ = render_rows(&mut pane, 80, 24);
        }
        let reads = requests(&daemon);
        assert!(reads.iter().all(|r| r.method != "config/set"), "{reads:#?}");
        assert!(reads.iter().all(allowed), "{reads:#?}");

        focus_instance(&mut pane, "chat", "ops");
        let before_toggle = requests(&daemon).len();
        toggle(&mut pane, &rpc).await;
        let all = requests(&daemon);
        assert!(all.iter().all(allowed), "{all:#?}");
        let writes: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(_, r)| r.method == "config/set")
            .map(|(at, _)| at)
            .collect();
        assert_eq!(writes.len(), 1, "{all:#?}");
        assert!(
            writes[0] > before_toggle,
            "the write follows the toggle key"
        );
        assert_eq!(
            all[writes[0]].params["prop"],
            json!("channels.plugin.ops.enabled")
        );
    }

    #[tokio::test]
    async fn a_click_selects_an_instance_and_never_toggles() {
        use crossterm::event::KeyModifiers as M;
        let (mut pane, _rpc, _outbound, daemon) = served_pane(channel_state()).await;
        select_and_open(&mut pane, "chat");
        {
            let _keymap = default_keymap();
            let _ = render_rows(&mut pane, 100, 30);
        }
        let area = pane
            .last_instances_area
            .expect("the instance list is drawn");
        let mouse = |kind, row: u16| MouseEvent {
            kind,
            column: area.x + 3,
            row,
            modifiers: M::NONE,
        };
        let click = MouseEventKind::Down(MouseButton::Left);
        pane.handle_mouse(mouse(click, area.y + 1));
        pane.handle_mouse(mouse(click, area.y + 1));
        assert_eq!(pane.focus, Focus::Instances);
        assert_eq!(
            pane.selected_instance().map(|i| i.alias.as_str()),
            Some("ops")
        );
        assert_eq!(pane.pending_toggle, None, "a double click does not toggle");
        pane.handle_mouse(mouse(MouseEventKind::ScrollUp, area.y));
        assert_eq!(
            pane.selected_instance().map(|i| i.alias.as_str()),
            Some("alerts")
        );
        // A click past the last row selects nothing new.
        pane.handle_mouse(mouse(click, area.y + 5));
        assert_eq!(
            pane.selected_instance().map(|i| i.alias.as_str()),
            Some("alerts")
        );
        assert_eq!(methods(&daemon), ["plugins/list", "config/list"]);
    }

    #[tokio::test]
    async fn help_and_footer_name_the_instance_keys() {
        let (mut pane, ..) = served_pane(channel_state()).await;
        select_and_open(&mut pane, "chat");
        let _keymap = default_keymap();
        let actions = |pane: &PluginsPane| -> Vec<String> {
            pane.help_context()
                .entries
                .iter()
                .map(|e| e.action.clone())
                .collect()
        };
        let mouse = t("zc-config-help-mouse-open");
        let tail = [
            "Refresh the catalog and channel instances",
            "This help",
            "",
            mouse.as_str(),
        ];

        let expected: Vec<&str> = [
            "Scroll",
            "Go to the channel instances",
            "Back to the packages",
        ]
        .into_iter()
        .chain(tail)
        .collect();
        assert_eq!(actions(&pane), expected);
        let footer = pane.footer_hint();
        assert!(footer.contains("Enter=instances"), "{footer}");

        pane.focus = Focus::Instances;
        let expected: Vec<&str> = [
            "Navigate",
            "Toggle enabled in config",
            "Back to the package detail",
        ]
        .into_iter()
        .chain(tail)
        .collect();
        assert_eq!(actions(&pane), expected);
        let node = pane.help_context();
        let toggle = node
            .entries
            .iter()
            .find(|e| e.action == "Toggle enabled in config")
            .unwrap();
        assert_eq!(toggle.keys, vec!["Enter".to_string()]);
        let footer = pane.footer_hint();
        assert!(footer.starts_with(" ?=help"), "{footer}");
        assert!(footer.contains("=navigate"), "{footer}");
        assert!(footer.contains("Enter=toggle"), "{footer}");
        assert!(footer.contains("=back"), "{footer}");
        assert!(footer.contains("r=refresh"), "{footer}");
    }

    #[test]
    fn instance_rows_keep_their_state_whole_and_fit_the_width() {
        let rows = rows_of(&[
            ("a", Some(true)),
            ("a_much_longer_alias_name", Some(false)),
            ("mid", None),
        ]);
        let fitted = fit_instance_rows(&rows, 40);
        assert_eq!(fitted[0].1, "  enabled in config");
        assert_eq!(fitted[1].1, "  disabled in config");
        assert_eq!(fitted[2].1, "  unknown");
        // One column for every state.
        let lead_width: Vec<usize> = fitted
            .iter()
            .map(|(lead, _)| crate::display_width::display_width(lead))
            .collect();
        assert!(lead_width.iter().all(|w| *w == lead_width[0]), "{fitted:?}");
        for width in 0..50 {
            for (lead, state) in fit_instance_rows(&rows, width) {
                let used = crate::display_width::display_width(&lead)
                    + crate::display_width::display_width(&state);
                assert!(used <= width, "{width}: {lead:?} {state:?}");
            }
        }
    }

    #[tokio::test]
    async fn the_detail_scrolls_above_the_block_and_clamps_to_its_own_rows() {
        let (mut pane, ..) = served_pane(channel_state()).await;
        select_and_open(&mut pane, "chat");
        pane.detail_scroll = u16::MAX;
        let _keymap = default_keymap();
        let rows = render_rows(&mut pane, 80, 20);
        let detail = pane.last_detail_area.expect("the detail is drawn");
        let list = pane
            .last_instances_area
            .expect("the instance list is drawn");
        assert!(
            detail.bottom() <= list.top(),
            "the block sits below the detail"
        );
        let entry = pane.selected_entry().unwrap();
        let lines = Paragraph::new(detail_lines(&project_detail(entry, Unreadable::default())))
            .wrap(Wrap { trim: false })
            .line_count(detail.width - 2);
        let expected = u16::try_from(lines).unwrap() - (detail.height - 2);
        assert!(expected > 0, "the detail overflows its rows at this size");
        assert_eq!(
            pane.detail_scroll, expected,
            "clamped against the rows the block leaves, not the whole pane"
        );
        let text = flowing(&rows);
        assert!(text.contains("loaded or running."), "{rows:#?}");
        assert!(text.contains("alerts disabled in config"), "{rows:#?}");
    }

    #[test]
    fn the_saved_status_names_the_live_reload_chord() {
        use crate::keymap::{Chord, overrides};
        let _keymap = default_keymap();
        let _reset = ResetOverrides;
        let status = ToggleStatus {
            package: "chat".to_string(),
            alias: "ops".to_string(),
            kind: StatusKind::Done(ToggleOutcome::Saved(Some(false))),
        };
        overrides::set_row(
            GlobalAction::TAG,
            "reload_daemon",
            vec![Chord::key(KeyCode::F(9))],
        );
        let label = crate::keymap::action_key_labels(GlobalAction::ReloadDaemon);
        assert_eq!(label.len(), 1, "{label:?}");
        assert_eq!(
            status.text(),
            format!(
                "Saved: ops is now disabled in config. Takes effect after a daemon reload \
                 ({}).",
                label[0]
            )
        );
        overrides::set_row(GlobalAction::TAG, "reload_daemon", Vec::new());
        assert_eq!(
            status.text(),
            "Saved: ops is now disabled in config. Takes effect after a daemon reload or \
             restart."
        );
    }

    // ── Keys and mouse ───────────────────────────────────────────

    #[test]
    fn focus_moves_filters_packages_detail_and_back_out() {
        let mut pane = loaded_pane(catalog());
        assert_eq!(pane.focus, Focus::Filters);
        assert_eq!(
            press(&mut pane, KeyCode::Right),
            PluginsKeyOutcome::Consumed
        );
        assert_eq!(pane.focus, Focus::Packages);
        assert_eq!(press(&mut pane, KeyCode::Down), PluginsKeyOutcome::Consumed);
        assert_eq!(pane.selected_entry().map(|e| e.name.as_str()), Some("mail"));
        assert_eq!(
            press(&mut pane, KeyCode::Enter),
            PluginsKeyOutcome::Consumed
        );
        assert_eq!(pane.focus, Focus::Detail);
        assert_eq!(
            press(&mut pane, KeyCode::Char('r')),
            PluginsKeyOutcome::RefreshRequested,
            "r refreshes from the detail too"
        );
        assert_eq!(pane.focus, Focus::Detail);
        assert_eq!(press(&mut pane, KeyCode::Esc), PluginsKeyOutcome::Consumed);
        assert_eq!(pane.focus, Focus::Packages);
        assert_eq!(press(&mut pane, KeyCode::Left), PluginsKeyOutcome::Consumed);
        assert_eq!(pane.focus, Focus::Filters);
        assert_eq!(
            press(&mut pane, KeyCode::Esc),
            PluginsKeyOutcome::NotConsumed
        );
        assert_eq!(
            press(&mut pane, KeyCode::Left),
            PluginsKeyOutcome::NotConsumed
        );
        assert_eq!(
            press(&mut pane, KeyCode::Enter),
            PluginsKeyOutcome::Consumed
        );
        assert_eq!(pane.focus, Focus::Packages);
        assert_eq!(
            press(&mut pane, KeyCode::Char('r')),
            PluginsKeyOutcome::RefreshRequested
        );
    }

    #[test]
    fn filters_hide_rows_and_cursors_clamp_on_every_change() {
        let mut pane = loaded_pane(catalog());
        press(&mut pane, KeyCode::Right);
        press(&mut pane, KeyCode::Down);
        press(&mut pane, KeyCode::Down);
        press(&mut pane, KeyCode::Down);
        assert_eq!(
            pane.list_state.selected(),
            Some(2),
            "Down clamps at the end"
        );
        press(&mut pane, KeyCode::Left);

        press(&mut pane, KeyCode::Down);
        assert_eq!(pane.filter, CatalogFilter::Installed);
        assert_eq!(pane.list_state.selected(), Some(1));
        assert_eq!(
            pane.selected_entry().map(|e| e.name.as_str()),
            Some("archive")
        );
        press(&mut pane, KeyCode::Down);
        press(&mut pane, KeyCode::Down);
        assert_eq!(
            pane.filter,
            CatalogFilter::Registry,
            "Down clamps at the last filter"
        );
        press(&mut pane, KeyCode::Up);
        press(&mut pane, KeyCode::Up);
        press(&mut pane, KeyCode::Up);
        assert_eq!(
            pane.filter,
            CatalogFilter::All,
            "Up clamps at the first filter"
        );

        // A catalog with no registry rows: the filter empties the list and
        // nothing indexes past it.
        let mut data = catalog();
        data.plugins.retain(|entry| entry.available.is_none());
        pane.apply_result(Ok(data));
        pane.set_filter(CatalogFilter::Registry);
        assert_eq!(pane.list_state.selected(), None);
        press(&mut pane, KeyCode::Right);
        press(&mut pane, KeyCode::Enter);
        assert_eq!(
            pane.focus,
            Focus::Filters,
            "an empty filter draws no list to move into"
        );
        assert_eq!(
            press(&mut pane, KeyCode::Esc),
            PluginsKeyOutcome::NotConsumed
        );
        let rows = render_rows(&mut pane, 80, 24);
        assert!(
            rows.iter()
                .any(|row| row.contains("No packages match this filter."))
        );

        let mut empty = PluginsPane::new();
        press(&mut empty, KeyCode::Right);
        press(&mut empty, KeyCode::Down);
        press(&mut empty, KeyCode::Enter);
        assert_eq!(empty.focus, Focus::Filters);
        assert_eq!(
            press(&mut empty, KeyCode::Left),
            PluginsKeyOutcome::NotConsumed
        );
    }

    #[test]
    fn focus_never_rests_on_a_list_that_is_not_drawn() {
        let mut no_wasm = catalog();
        no_wasm.wasm_plugins_available = false;
        let mut installed_none = catalog();
        installed_none
            .plugins
            .retain(|entry| entry.installed.is_none());
        let states: [(&str, Result<PluginsListResult, CatalogError>, CatalogFilter); 4] = [
            ("error", Err(CatalogError::Unsupported), CatalogFilter::All),
            ("no WASM with rows", Ok(no_wasm), CatalogFilter::All),
            ("empty", Ok(catalog_of(&[])), CatalogFilter::All),
            (
                "Installed (0)",
                Ok(installed_none),
                CatalogFilter::Installed,
            ),
        ];
        for (name, result, filter) in states {
            let mut pane = PluginsPane::new();
            pane.apply_result(result);
            pane.set_filter(filter);
            for code in [KeyCode::Enter, KeyCode::Enter, KeyCode::Right] {
                assert_eq!(press(&mut pane, code), PluginsKeyOutcome::Consumed);
                assert_eq!(pane.focus, Focus::Filters, "{name}");
            }
            assert_eq!(
                press(&mut pane, KeyCode::Esc),
                PluginsKeyOutcome::NotConsumed,
                "{name}: one Back leaves the sub-tab"
            );
            let _keymap = default_keymap();
            let footer = pane.footer_hint();
            assert!(footer.contains("=previous sub-tab"), "{name}: {footer}");
            assert!(!footer.contains("Enter="), "{name}: {footer}");
            assert!(
                !pane
                    .help_context()
                    .entries
                    .iter()
                    .any(|e| e.action == "Show the packages"),
                "{name}"
            );
        }
    }

    #[test]
    fn a_refresh_that_leaves_no_list_returns_focus_to_the_filters() {
        // A failed refresh while the package list is focused.
        let mut pane = loaded_pane(catalog());
        press(&mut pane, KeyCode::Right);
        assert_eq!(pane.focus, Focus::Packages);
        pane.apply_result(Err(CatalogError::TimedOut));
        assert_eq!(pane.focus, Focus::Filters);

        // A refresh that turns an open detail into a daemon without WASM
        // support, with the same package still listed.
        let mut pane = loaded_pane(catalog());
        select_and_open(&mut pane, "calendar");
        pane.detail_scroll = 2;
        let mut no_wasm = catalog();
        no_wasm.wasm_plugins_available = false;
        pane.apply_result(Ok(no_wasm));
        assert_eq!(pane.focus, Focus::Filters);
        assert_eq!(pane.detail_scroll, 0);
        pane.open_detail();
        assert_eq!(pane.focus, Focus::Filters, "no detail opens without WASM");

        // A wheel tick over the filters that empties the focused list.
        let mut data = catalog();
        data.plugins.retain(|entry| entry.installed.is_none());
        let mut pane = loaded_pane(data);
        press(&mut pane, KeyCode::Right);
        assert_eq!(pane.focus, Focus::Packages);
        let _ = render_rows(&mut pane, 100, 20);
        pane.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 2,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert_eq!(pane.filter, CatalogFilter::Installed);
        assert_eq!(pane.focus, Focus::Filters, "Installed (0) draws no list");
    }

    #[test]
    fn clicks_select_filters_and_packages_and_double_click_opens_detail() {
        use crossterm::event::KeyModifiers as M;
        let mut pane = loaded_pane(catalog());
        let _ = render_rows(&mut pane, 100, 20);
        let click = |column: u16, row: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: M::NONE,
        };

        // Filter rows start inside the top border of the left column.
        pane.handle_mouse(click(5, 2));
        assert_eq!(pane.filter, CatalogFilter::Installed);
        assert_eq!(pane.focus, Focus::Filters);
        pane.handle_mouse(click(5, 1));
        assert_eq!(pane.filter, CatalogFilter::All);

        let _ = render_rows(&mut pane, 100, 20);
        pane.handle_mouse(click(40, 2));
        assert_eq!(pane.focus, Focus::Packages);
        assert_eq!(pane.selected_entry().map(|e| e.name.as_str()), Some("mail"));
        pane.handle_mouse(click(40, 2));
        assert_eq!(pane.focus, Focus::Detail, "a double click opens the detail");
        let rows = render_rows(&mut pane, 100, 20);
        assert!(
            rows.iter()
                .any(|row| row.contains("Package identity: mail@1.2.3"))
        );
    }

    #[test]
    fn help_and_footer_follow_the_focused_list() {
        let _keymap = default_keymap();
        let mut pane = loaded_pane(catalog());
        let actions = |pane: &PluginsPane| -> Vec<String> {
            pane.help_context()
                .entries
                .iter()
                .map(|e| e.action.clone())
                .collect()
        };
        let mouse = t("zc-config-help-mouse-open");
        let tail = [
            "Refresh the catalog and channel instances",
            "This help",
            "",
            mouse.as_str(),
        ];

        assert_eq!(pane.focus, Focus::Filters);
        let expected: Vec<&str> = ["Choose a filter", "Show the packages", "Previous sub-tab"]
            .into_iter()
            .chain(tail)
            .collect();
        assert_eq!(actions(&pane), expected);
        let footer = pane.footer_hint();
        assert!(footer.contains("=previous sub-tab"), "{footer}");
        assert!(footer.contains("Enter=packages"), "{footer}");

        pane.focus = Focus::Packages;
        let expected: Vec<&str> = ["Navigate", "Open", "Back to the filters"]
            .into_iter()
            .chain(tail)
            .collect();
        assert_eq!(actions(&pane), expected);
        let footer = pane.footer_hint();
        assert!(footer.contains("Enter=open"), "{footer}");

        pane.focus = Focus::Detail;
        let expected: Vec<&str> = ["Scroll", "Back to the packages"]
            .into_iter()
            .chain(tail)
            .collect();
        assert_eq!(actions(&pane), expected);
        let footer = pane.footer_hint();
        assert!(footer.contains("=scroll"), "{footer}");
        assert!(
            !footer.contains("Enter="),
            "Enter does nothing in the detail: {footer}"
        );

        for focus in [Focus::Filters, Focus::Packages, Focus::Detail] {
            pane.focus = focus;
            let node = pane.help_context();
            let refresh = node
                .entries
                .iter()
                .find(|e| e.action == "Refresh the catalog and channel instances")
                .unwrap();
            assert_eq!(refresh.keys, vec!["r".to_string()], "{focus:?}");
            assert!(
                node.entries
                    .iter()
                    .filter(|e| !e.action.is_empty())
                    .all(|e| !e.keys.is_empty()),
                "{focus:?}"
            );
            let footer = pane.footer_hint();
            assert!(footer.starts_with(" ?=help"), "{focus:?}: {footer}");
            assert!(footer.contains("r=refresh"), "{focus:?}: {footer}");
        }
    }

    /// Resets the keybinding overrides on drop, so a failed assertion never
    /// leaks a rebinding into the next test holding the guard.
    struct ResetOverrides;

    impl Drop for ResetOverrides {
        fn drop(&mut self) {
            crate::keymap::overrides::reset();
        }
    }

    #[test]
    fn a_rebound_refresh_chord_drives_the_hints_and_the_key() {
        use crate::keymap::{Chord, ConfigTabAction, overrides};
        let _keymap = default_keymap();
        let _reset = ResetOverrides;
        overrides::set_row(
            ConfigTabAction::TAG,
            "refresh",
            vec![Chord::key(KeyCode::F(5))],
        );
        let label = crate::keymap::action_key_labels(ConfigTabAction::Refresh);
        assert_eq!(label.len(), 1, "{label:?}");
        assert_ne!(label[0], "r");

        let mut pane = loaded_pane(catalog());
        let node = pane.help_context();
        let refresh = node
            .entries
            .iter()
            .find(|e| e.action == "Refresh the catalog and channel instances")
            .unwrap();
        assert_eq!(refresh.keys, label);
        let footer = pane.footer_hint();
        assert!(
            footer.contains(&format!("{}=refresh", label[0])),
            "{footer}"
        );
        assert!(!footer.contains("r=refresh"), "{footer}");

        assert_eq!(
            pane.handle_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE)),
            PluginsKeyOutcome::RefreshRequested
        );
        assert_eq!(
            pane.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
            PluginsKeyOutcome::Consumed
        );

        let mut failed = PluginsPane::new();
        failed.apply_result(Err(CatalogError::Unsupported));
        let rows = render_rows(&mut failed, 120, 20);
        assert!(
            rows.iter()
                .any(|row| row.contains(&format!("Press {} to retry.", label[0]))),
            "{rows:#?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains("Press r to retry.")),
            "{rows:#?}"
        );
    }

    // ── Selection across data changes ────────────────────────────

    fn select_and_open(pane: &mut PluginsPane, name: &str) {
        let row = pane
            .visible_indices()
            .iter()
            .position(|idx| pane.plugins()[*idx].name == name)
            .unwrap();
        pane.list_state.select(Some(row));
        pane.focus = Focus::Packages;
        pane.open_detail();
        assert_eq!(pane.focus, Focus::Detail);
    }

    fn catalog_of(names: &[&str]) -> PluginsListResult {
        let mut data = catalog();
        data.plugins = names
            .iter()
            .map(|name| {
                catalog()
                    .plugins
                    .into_iter()
                    .find(|entry| entry.name == *name)
                    .unwrap()
            })
            .collect();
        data
    }

    #[test]
    fn a_refresh_that_drops_the_open_package_closes_its_detail() {
        let mut pane = loaded_pane(catalog());
        select_and_open(&mut pane, "calendar");
        pane.apply_result(Ok(catalog_of(&["mail", "archive"])));
        assert_eq!(
            pane.focus,
            Focus::Packages,
            "row 0 is now another package; the detail must not switch to it"
        );
    }

    #[test]
    fn a_refresh_that_moves_the_open_package_keeps_showing_it() {
        let mut pane = loaded_pane(catalog());
        select_and_open(&mut pane, "mail");
        pane.detail_scroll = 2;
        assert_eq!(pane.list_state.selected(), Some(1));
        pane.apply_result(Ok(catalog_of(&["mail", "archive"])));
        assert_eq!(pane.focus, Focus::Detail);
        assert_eq!(pane.list_state.selected(), Some(0));
        assert_eq!(pane.selected_entry().map(|e| e.name.as_str()), Some("mail"));
        assert_eq!(pane.detail_scroll, 2, "the same package keeps its scroll");

        // The package cursor follows by name too, outside the detail.
        pane.focus = Focus::Packages;
        pane.apply_result(Ok(catalog_of(&["archive", "calendar", "mail"])));
        assert_eq!(pane.list_state.selected(), Some(2));
    }

    #[test]
    fn a_filter_wheel_tick_never_shows_another_package_detail() {
        use crossterm::event::KeyModifiers as M;
        let wheel = |column: u16, row: u16| MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column,
            row,
            modifiers: M::NONE,
        };

        // The open package survives a filter that still shows it.
        let mut pane = loaded_pane(catalog());
        select_and_open(&mut pane, "calendar");
        let _ = render_rows(&mut pane, 100, 20);
        pane.handle_mouse(wheel(5, 2));
        assert_eq!(pane.filter, CatalogFilter::Installed);
        assert_eq!(pane.focus, Focus::Detail);
        assert_eq!(
            pane.selected_entry().map(|e| e.name.as_str()),
            Some("calendar")
        );

        // A filter that hides it closes the detail instead of showing
        // whichever package now sits at the same row.
        let mut pane = loaded_pane(catalog());
        select_and_open(&mut pane, "mail");
        let _ = render_rows(&mut pane, 100, 20);
        pane.handle_mouse(wheel(5, 2));
        assert_eq!(pane.filter, CatalogFilter::Installed);
        assert_eq!(pane.focus, Focus::Packages);
        let rows = render_rows(&mut pane, 100, 20);
        assert!(
            !rows.iter().any(|row| row.contains("Package identity")),
            "{rows:#?}"
        );
    }

    #[test]
    fn no_wasm_support_wins_over_any_rows() {
        let mut data = catalog();
        data.wasm_plugins_available = false;
        let mut pane = loaded_pane(data);
        let rows = render_rows(&mut pane, 120, 20);
        assert!(
            rows.iter()
                .any(|row| row.contains("This daemon was built without")),
            "{rows:#?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains("calendar")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("not built in")),
            "{rows:#?}"
        );
        // No catalog, so no counts either, although the body carries rows.
        assert!(!rows.iter().any(|row| row.contains("All (")), "{rows:#?}");
        let left: Vec<String> = rows
            .iter()
            .map(|row| {
                row.chars()
                    .take(usize::from(LEFT_COLUMN_WIDTH))
                    .collect::<String>()
                    .trim_matches(|c: char| c == '\u{2502}' || c.is_whitespace())
                    .to_string()
            })
            .collect();
        for label in ["All", "Installed", "In registry"] {
            assert!(
                left.iter().any(|row| row.ends_with(label)),
                "{label}: {rows:#?}"
            );
        }
    }

    // ── Catalog keys ─────────────────────────────────────────────

    /// No crate-wide test pins these keys, so this one does: every
    /// `zc-plugins-*` key this pane and the Config manager use is defined in
    /// the English catalog, and every one defined there is used.
    #[test]
    fn every_plugins_key_is_defined_in_the_english_catalog() {
        let catalog = include_str!("../locales/en/zerocode.ftl");
        let defined: std::collections::HashSet<&str> = catalog
            .lines()
            .filter_map(|line| line.split_once(" = ").map(|(key, _)| key.trim()))
            .collect();
        let prefix = "zc-plugins-";
        // Built at run time so the needle never appears in this file.
        let needle = format!("\"{prefix}");
        let mut referenced = std::collections::HashSet::new();
        for source in [
            include_str!("plugins_pane.rs"),
            include_str!("config_manager.rs"),
        ] {
            // Each key is anchored at its own opening quote, so quotes
            // elsewhere in the file never put the scan out of step.
            for (at, _) in source.match_indices(&needle) {
                let rest = &source[at + 1..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
                    .unwrap_or(rest.len());
                let key = &rest[..end];
                // The bare prefix is this test's own filter, not a key.
                if key.len() == prefix.len() || !rest[end..].starts_with('"') {
                    continue;
                }
                assert!(defined.contains(key), "`{key}` is not in the en catalog");
                referenced.insert(key);
            }
        }
        let mut unused: Vec<&&str> = defined
            .iter()
            .filter(|key| key.starts_with(prefix) && !referenced.contains(**key))
            .collect();
        unused.sort();
        assert!(unused.is_empty(), "defined but never used: {unused:?}");
        assert!(
            referenced.len() > 60,
            "the scan found only {} keys",
            referenced.len()
        );

        for key in defined.iter().filter(|key| key.starts_with("zc-plugins-")) {
            let rendered = t(key);
            let formatted = t_args(
                key,
                &[
                    ("count", "1"),
                    ("label", "x"),
                    ("title", "x"),
                    ("version", "1"),
                    ("installed", "1"),
                    ("registry", "2"),
                    ("error", "x"),
                    ("keys", "r"),
                    ("description", "x"),
                    ("list", "x"),
                    ("identity", "x"),
                    ("name", "x"),
                    ("alias", "x"),
                ],
            );
            assert!(
                rendered != format!("{{{key}}}") || formatted != format!("{{{key}}}"),
                "`{key}` does not format"
            );
        }
        assert_eq!(
            t("zc-plugins-host-enabled-label"),
            "[plugins] enabled in config"
        );
    }
}

/// A scripted daemon for the plugins sub-tab tests here and in the Config
/// manager: it answers `plugins/list` from a fixed reply, and `config/list`
/// and `config/set` from an in-memory `[channels.plugin.<alias>]` table the
/// way the daemon does, recording every request.
#[cfg(test)]
pub(crate) mod fake_daemon {
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};
    use tokio::sync::mpsc;

    use crate::client::RpcClient;
    use crate::jsonrpc::{JsonRpcError, RpcOutbound};
    use zeroclaw_api::jsonrpc::error_codes;

    /// One request the daemon received.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Request {
        pub(crate) method: String,
        pub(crate) params: Value,
    }

    /// One `[channels.plugin.<alias>]` table with the raw values the daemon
    /// lists; `None` leaves that field's row out.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Declaration {
        pub(crate) alias: String,
        pub(crate) package: Option<String>,
        pub(crate) enabled: Option<String>,
    }

    pub(crate) fn declaration(alias: &str, package: &str, enabled: &str) -> Declaration {
        Declaration {
            alias: alias.to_string(),
            package: Some(package.to_string()),
            enabled: Some(enabled.to_string()),
        }
    }

    pub(crate) struct State {
        /// The reply to `plugins/list`, and to any method not modeled here.
        pub(crate) catalog: Result<Value, JsonRpcError>,
        /// Listed in this order, which is deliberately not sorted.
        pub(crate) declarations: Vec<Declaration>,
        /// Every `config/list` fails with this.
        pub(crate) list_error: Option<JsonRpcError>,
        /// Every `config/list` after a `config/set` fails with this.
        pub(crate) list_error_after_set: Option<JsonRpcError>,
        /// Every `config/set` fails with this.
        pub(crate) set_error: Option<JsonRpcError>,
        /// Store the write before answering with `set_error`, the way a
        /// dropped connection loses a reply to a write that landed.
        pub(crate) apply_failed_set: bool,
        /// Leave every `config/set` unanswered.
        pub(crate) hold_set: bool,
        pub(crate) requests: Vec<Request>,
        set_seen: bool,
    }

    impl State {
        pub(crate) fn new(catalog: Result<Value, JsonRpcError>) -> Self {
            Self {
                catalog,
                declarations: Vec::new(),
                list_error: None,
                list_error_after_set: None,
                set_error: None,
                apply_failed_set: false,
                hold_set: false,
                requests: Vec::new(),
                set_seen: false,
            }
        }

        pub(crate) fn with(mut self, declarations: Vec<Declaration>) -> Self {
            self.declarations = declarations;
            self
        }

        pub(crate) fn methods(&self) -> Vec<String> {
            self.requests
                .iter()
                .map(|request| request.method.clone())
                .collect()
        }

        pub(crate) fn find(&self, alias: &str) -> Option<&Declaration> {
            self.declarations.iter().find(|decl| decl.alias == alias)
        }

        fn rows(&self, prefix: Option<&str>) -> Value {
            let matches = |path: &str| match prefix {
                None => true,
                Some(prefix) => {
                    path == prefix
                        || path
                            .strip_prefix(prefix)
                            .is_some_and(|rest| rest.starts_with('.'))
                }
            };
            let mut entries = Vec::new();
            for decl in &self.declarations {
                let fields = [
                    ("package", "string", "String", &decl.package),
                    ("enabled", "bool", "bool", &decl.enabled),
                ];
                for (field, kind, type_hint, value) in fields {
                    let path = format!("channels.plugin.{}.{field}", decl.alias);
                    if let Some(value) = value
                        && matches(&path)
                    {
                        entries.push(json!({
                            "path": path,
                            "category": "channels",
                            "kind": kind,
                            "type_hint": type_hint,
                            "value": value,
                            "populated": true,
                            "is_secret": false,
                            "description": "",
                            "section": "channels",
                        }));
                    }
                }
            }
            json!({ "entries": entries })
        }

        /// `config/set` as the daemon does it on this table: a JSON bool is
        /// coerced, a string passes through, and a missing alias is created
        /// with an empty package.
        fn set(&mut self, params: &Value) -> Result<Value, JsonRpcError> {
            let prop = params["prop"].as_str().unwrap_or_default().to_string();
            let invalid = |message: String| JsonRpcError {
                code: error_codes::INVALID_PARAMS,
                message,
                data: None,
            };
            let alias = prop
                .strip_prefix("channels.plugin.")
                .and_then(|rest| rest.strip_suffix(".enabled"))
                .ok_or_else(|| invalid(format!("Unknown property '{prop}'")))?;
            let value = match &params["value"] {
                Value::Bool(value) => value.to_string(),
                Value::String(value) if value == "true" || value == "false" => value.clone(),
                other => return Err(invalid(format!("not a bool: {other}"))),
            };
            if let Some(error) = &self.set_error
                && !self.apply_failed_set
            {
                return Err(error.clone());
            }
            match self
                .declarations
                .iter_mut()
                .find(|decl| decl.alias == alias)
            {
                Some(decl) => decl.enabled = Some(value),
                None => self.declarations.push(Declaration {
                    alias: alias.to_string(),
                    package: Some(String::new()),
                    enabled: Some(value),
                }),
            }
            match &self.set_error {
                Some(error) => Err(error.clone()),
                None => Ok(json!({ "prop": prop, "set": true })),
            }
        }

        /// The reply to one request, or `None` to leave it unanswered.
        fn answer(&mut self, method: &str, params: &Value) -> Option<Result<Value, JsonRpcError>> {
            match method {
                crate::client::method::CONFIG_LIST => {
                    let error = if self.set_seen {
                        self.list_error_after_set
                            .as_ref()
                            .or(self.list_error.as_ref())
                    } else {
                        self.list_error.as_ref()
                    };
                    Some(match error {
                        Some(error) => Err(error.clone()),
                        None => Ok(self.rows(params["prefix"].as_str())),
                    })
                }
                crate::client::method::CONFIG_SET => {
                    self.set_seen = true;
                    if self.hold_set {
                        return None;
                    }
                    Some(self.set(params))
                }
                _ => Some(self.catalog.clone()),
            }
        }
    }

    /// A client served by `state`, the outbound side for counting pending
    /// requests, and the shared state for scripting and inspection.
    pub(crate) fn serve(state: State) -> (Arc<RpcClient>, Arc<RpcOutbound>, Arc<Mutex<State>>) {
        let (tx, mut rx) = mpsc::channel::<String>(16);
        let outbound = Arc::new(RpcOutbound::new(tx));
        let rpc = Arc::new(RpcClient::with_rpc(Arc::clone(&outbound)));
        let state = Arc::new(Mutex::new(state));
        let shared = Arc::clone(&state);
        let replies = Arc::clone(&outbound);
        tokio::spawn(async move {
            while let Some(raw) = rx.recv().await {
                let Ok(request) = serde_json::from_str::<Value>(&raw) else {
                    continue;
                };
                let method = request["method"].as_str().unwrap_or_default().to_string();
                let id = request["id"].as_str().unwrap_or_default().to_string();
                let params = request["params"].clone();
                let reply = {
                    let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                    state.requests.push(Request {
                        method: method.clone(),
                        params: params.clone(),
                    });
                    state.answer(&method, &params)
                };
                match reply {
                    Some(Ok(body)) => replies.dispatch_response(&id, Some(body), None),
                    Some(Err(error)) => replies.dispatch_response(&id, None, Some(error)),
                    None => {}
                }
            }
        });
        (rpc, outbound, state)
    }
}
