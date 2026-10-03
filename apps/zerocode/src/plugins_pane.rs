//! Read-only `plugins` sub-tab of the Config pane.
//!
//! Renders the daemon's `plugins/list` catalog, the same body `GET
//! /api/plugins` serves: one row per package in the daemon's order, with the
//! installed record and the cached-registry record kept apart. The pane only
//! ever sends `plugins/list`. It never writes config, never merges or re-sorts
//! rows, and never claims that a package is loaded, running or healthy,
//! because the catalog carries no runtime evidence.

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
    PluginCatalogEntry, PluginCatalogIssue, PluginCatalogIssueCode, PluginCatalogIssueSource,
    PluginsListResult,
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

type CatalogFetch = JoinHandle<Result<PluginsListResult, CatalogError>>;

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
/// list on the right, or the package detail that replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Filters,
    Packages,
    Detail,
}

/// What the Config manager does after the pane saw a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginsKeyOutcome {
    Consumed,
    /// Back at the filter list: the manager crosses to the previous sub-tab.
    NotConsumed,
    /// The refresh chord: the manager starts a fetch with its live client.
    RefreshRequested,
}

pub(crate) struct PluginsPane {
    data: Option<PluginsListResult>,
    error: Option<CatalogError>,
    /// A fetch in flight. Loading is exactly "a task is present".
    refresh_task: Option<CatalogFetch>,
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
    double_click: crate::mouse::DoubleClickTracker,
}

impl PluginsPane {
    /// No RPC here: the catalog loads on first entry into the sub-tab.
    pub(crate) fn new() -> Self {
        Self {
            data: None,
            error: None,
            refresh_task: None,
            focus: Focus::Filters,
            filter: CatalogFilter::All,
            list_state: ListState::default(),
            detail_scroll: 0,
            last_filter_area: None,
            last_filter_offset: 0,
            last_list_area: None,
            last_detail_area: None,
            double_click: crate::mouse::DoubleClickTracker::new(),
        }
    }

    // ── Fetch lifecycle ──────────────────────────────────────────

    /// Start the first fetch: only when nothing is loaded, no error is held
    /// and none is running. A held error waits for an explicit refresh.
    pub(crate) fn refresh_if_inactive(&mut self, rpc: &Arc<RpcClient>) {
        if self.data.is_none() && self.error.is_none() && !self.is_loading() {
            self.start_refresh(rpc);
        }
    }

    /// Refetch on request. Ignored while a fetch is already running.
    pub(crate) fn refresh(&mut self, rpc: &Arc<RpcClient>) {
        if !self.is_loading() {
            self.start_refresh(rpc);
        }
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.refresh_task.is_some()
    }

    fn start_refresh(&mut self, rpc: &Arc<RpcClient>) {
        // Loaded data stays on screen, marked as refreshing, until the result
        // lands; a held error gives way to the loading state.
        self.error = None;
        let rpc = Arc::clone(rpc);
        self.refresh_task = Some(tokio::spawn(async move {
            rpc.plugins_list()
                .await
                .map_err(|err| CatalogError::from_call(&err))
        }));
    }

    /// Apply a finished fetch. Never waits on one still in flight, so the
    /// draw loop is never blocked.
    pub(crate) async fn poll_refresh(&mut self) {
        let Some(task) = self.refresh_task.take_if(|task| task.is_finished()) else {
            return;
        };
        let result = match task.await {
            Ok(result) => result,
            Err(join_error) => Err(CatalogError::Other(display_safe(&format!(
                "plugin catalog request task failed: {join_error}"
            )))),
        };
        self.apply_result(result);
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
                if self.focus == Focus::Detail {
                    self.focus = Focus::Packages;
                }
                self.clamp_selection();
            }
        }
        if !self.packages_shown() && self.focus != Focus::Filters {
            self.focus = Focus::Filters;
            self.detail_scroll = 0;
        }
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
        if self.focus == Focus::Detail && self.selected_entry().is_none() {
            self.focus = Focus::Packages;
        }
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
            },
            A::Down => match self.focus {
                Focus::Filters => self.step_filter(1),
                Focus::Packages => self.step_selection(1),
                Focus::Detail => self.scroll_detail(1),
            },
            A::Enter | A::TabRight => match self.focus {
                Focus::Filters if self.packages_shown() => self.focus = Focus::Packages,
                Focus::Filters => {}
                Focus::Packages => self.open_detail(),
                Focus::Detail => {}
            },
            A::Back | A::TabLeft => match self.focus {
                Focus::Filters => return PluginsKeyOutcome::NotConsumed,
                Focus::Packages => self.focus = Focus::Filters,
                Focus::Detail => self.focus = Focus::Packages,
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
                if over(self.last_detail_area) {
                    self.focus = Focus::Detail;
                }
            }
            MouseEventKind::ScrollDown if over(self.last_filter_area) => self.step_filter(1),
            MouseEventKind::ScrollUp if over(self.last_filter_area) => self.step_filter(-1),
            MouseEventKind::ScrollDown if over(self.last_list_area) => self.step_selection(1),
            MouseEventKind::ScrollUp if over(self.last_list_area) => self.step_selection(-1),
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
    /// detail, Up/Down scroll and nothing opens.
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
            Focus::Detail => vec![
                entry(&[A::Up, A::Down], "zc-plugins-help-scroll"),
                entry(&[A::Back, A::TabLeft], "zc-plugins-help-back-to-packages"),
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
            Focus::Detail => ("zc-plugins-footer-scroll", None, "zc-plugins-footer-back"),
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
        if self.focus == Focus::Detail
            && let Some(entry) = self.selected_entry()
        {
            let detail = project_detail(entry, self.unreadable());
            self.draw_detail(frame, area, &detail);
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

    fn draw_detail(&mut self, frame: &mut Frame, area: Rect, detail: &PackageDetail) {
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
}

impl Drop for PluginsPane {
    fn drop(&mut self) {
        if let Some(task) = self.refresh_task.take() {
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
        settle(&mut pane).await;
        assert_eq!(pane.data, Some(catalog()));

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

    #[tokio::test]
    async fn navigation_and_refresh_only_ever_send_the_catalog_method() {
        let (rpc, calls) = responding_client(Ok(catalog_body()));
        let mut pane = PluginsPane::new();
        pane.refresh_if_inactive(&rpc);
        settle(&mut pane).await;

        let keys = [
            KeyCode::Down,
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Right,
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Down,
            KeyCode::Char('d'),
            KeyCode::Char('x'),
            KeyCode::Char('t'),
            KeyCode::Esc,
            KeyCode::Left,
            KeyCode::Enter,
            KeyCode::Char('/'),
        ];
        for code in keys {
            if press(&mut pane, code) == PluginsKeyOutcome::RefreshRequested {
                pane.refresh(&rpc);
            }
            settle(&mut pane).await;
        }
        assert_eq!(
            press(&mut pane, KeyCode::Char('r')),
            PluginsKeyOutcome::RefreshRequested
        );
        pane.refresh(&rpc);
        settle(&mut pane).await;
        let _ = render_rows(&mut pane, 80, 24);

        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["plugins/list", "plugins/list"],
            "the pane is read-only: plugins/list is the only method it sends"
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
        let tail = ["Refresh the catalog", "This help", "", mouse.as_str()];

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
                .find(|e| e.action == "Refresh the catalog")
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
            .find(|e| e.action == "Refresh the catalog")
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
