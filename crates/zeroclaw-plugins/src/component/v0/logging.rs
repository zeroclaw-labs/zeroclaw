// Host-side WIT `logging` implementation for all three component-model plugin
// worlds (`tool-plugin`, `memory-plugin`, `channel-plugin`).
//
// [`PluginLoggingHost`] is the `Store<T>` data type for all three worlds.
// It carries the `WasiCtx` built from the plugin's `fine_grained_permissions`,
// and the `ResourceTable` required by WasiView.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use wasmtime::component::{HasSelf, ResourceTable};
use wasmtime_wasi::sockets::SocketAddrUse;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use zeroclaw_log::{Action, Event, EventOutcome, info_span, record};

use super::bindings;

// ── PluginLoggingHost ─────────────────────────────────────────────────────────

/// Store-data type for all three component plugin worlds.
pub struct PluginLoggingHost {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl Default for PluginLoggingHost {
    /// Constructs a fully-sandboxed host: no filesystem preopens, all network
    /// disabled. Used for metadata-probe stores where no I/O is needed.
    fn default() -> Self {
        Self {
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        }
    }
}

impl PluginLoggingHost {
    /// Build a host from a plugin's `fine_grained_permissions` list.
    ///
    /// - `Dir` entries call `WasiCtxBuilder::preopened_dir`.
    /// - `Http` + `Tcp` entries add rules to the TCP allow-list.
    /// - `Udp` entries add rules to the UDP allow-list.
    ///
    /// TCP bind (`TcpBind`) is unconditionally denied; outbound-only TCP is
    /// allowed when matching rules are present. If no TCP/HTTP rules are
    /// declared TCP is fully disabled; same for UDP.
    ///
    /// Address rules:
    /// - IPv4/IPv6 literals are matched exactly at connect time.
    /// - Exact domain names are resolved via blocking DNS at construction and
    ///   their IPs are matched at connect time.
    /// - Wildcard domain names (e.g. `*.example.com`) are resolved at connect
    ///   time using a reverse-DNS lookup; the resulting hostname is matched
    ///   against the pattern. If reverse DNS fails, the connection is denied.
    pub fn with_permissions(perms: &[crate::FineGrainedPermission]) -> anyhow::Result<Self> {
        let mut builder = WasiCtxBuilder::new();

        let mut tcp_rules: Vec<AddrRule> = Vec::new();
        let mut udp_rules: Vec<AddrRule> = Vec::new();
        let mut has_tcp = false;
        let mut has_udp = false;
        let mut has_domain_lookup = false;

        for perm in perms {
            match perm {
                crate::FineGrainedPermission::Dir(dir) => {
                    let dir_perms = match (dir.dir_read, dir.dir_write) {
                        (true, true) => DirPerms::all(),
                        (true, false) => DirPerms::READ,
                        (false, true) => DirPerms::MUTATE,
                        (false, false) => DirPerms::empty(),
                    };
                    let file_perms = match (dir.file_read, dir.file_write) {
                        (true, true) => FilePerms::all(),
                        (true, false) => FilePerms::READ,
                        (false, true) => FilePerms::WRITE,
                        (false, false) => FilePerms::empty(),
                    };
                    builder
                        .preopened_dir(&dir.host_path, &dir.guest_path, dir_perms, file_perms)
                        .map_err(|e| anyhow::Error::msg(format!("{e}")))?;
                }
                crate::FineGrainedPermission::Http(addr)
                | crate::FineGrainedPermission::Tcp(addr) => {
                    has_tcp = true;
                    if !addr.is_wildcard() {
                        has_domain_lookup =
                            has_domain_lookup || addr.as_str().parse::<IpAddr>().is_err();
                    }
                    tcp_rules.push(AddrRule::parse(addr)?);
                }
                crate::FineGrainedPermission::Udp(addr) => {
                    has_udp = true;
                    if !addr.is_wildcard() {
                        has_domain_lookup =
                            has_domain_lookup || addr.as_str().parse::<IpAddr>().is_err();
                    }
                    udp_rules.push(AddrRule::parse(addr)?);
                }
            }
        }

        builder.allow_tcp(has_tcp);
        builder.allow_udp(has_udp);
        // Enable ip-name-lookup if any domain-based (non-IP) permissions are
        // present so the plugin can resolve the names it needs.
        if has_domain_lookup {
            builder.allow_ip_name_lookup(true);
        }

        if has_tcp || has_udp {
            let tcp_rules = Arc::new(tcp_rules);
            let udp_rules = Arc::new(udp_rules);
            builder.socket_addr_check(move |socket_addr, use_kind| {
                let tcp = Arc::clone(&tcp_rules);
                let udp = Arc::clone(&udp_rules);
                let ip = socket_addr.ip();
                Box::pin(async move {
                    match use_kind {
                        // Never allow inbound server sockets.
                        SocketAddrUse::TcpBind => false,
                        SocketAddrUse::TcpConnect => addr_matches(&tcp, ip).await,
                        SocketAddrUse::UdpBind
                        | SocketAddrUse::UdpConnect
                        | SocketAddrUse::UdpOutgoingDatagram => addr_matches(&udp, ip).await,
                    }
                })
            });
        }

        Ok(Self {
            wasi: builder.build(),
            table: ResourceTable::new(),
        })
    }
}

// ── AddrRule ──────────────────────────────────────────────────────────────────

/// A pre-parsed rule for the socket address check.
enum AddrRule {
    /// An explicit IP address literal.
    Ip(IpAddr),
    /// An exact domain, pre-resolved to one or more IPs at construction.
    ResolvedDomain(Arc<[IpAddr]>),
    /// A wildcard domain pattern (e.g. `*.example.com`).  Enforced via
    /// reverse-DNS lookup at connect time.
    WildcardPattern(String),
}

impl AddrRule {
    fn parse(addr: &crate::AddressString) -> anyhow::Result<Self> {
        let s = addr.as_str();
        // IP literal
        if let Ok(ip) = s.parse::<IpAddr>() {
            return Ok(Self::Ip(ip));
        }
        // Wildcard domain — cannot pre-resolve
        if addr.is_wildcard() {
            record!(
                WARN,
                Event::new(module_path!(), Action::Note).with_attrs(json!({ "address": s })),
                "wildcard domain permission: enforcement uses reverse-DNS at connect time; connections are denied if reverse lookup fails"
            );
            return Ok(Self::WildcardPattern(s.to_string()));
        }
        // Exact domain — resolve now (blocking; plugin load happens rarely)
        use std::net::ToSocketAddrs;
        let ips: Arc<[IpAddr]> = format!("{s}:0")
            .to_socket_addrs()
            .map_err(|e| anyhow::Error::msg(format!("failed to resolve '{s}': {e}")))?
            .map(|sa| sa.ip())
            .collect();
        if ips.is_empty() {
            anyhow::bail!("domain '{s}' resolved to no addresses");
        }
        Ok(Self::ResolvedDomain(ips))
    }
}

/// Check `ip` against `rules` — used inside the async `socket_addr_check`.
async fn addr_matches(rules: &[AddrRule], ip: IpAddr) -> bool {
    for rule in rules {
        match rule {
            AddrRule::Ip(allowed) => {
                if *allowed == ip {
                    return true;
                }
            }
            AddrRule::ResolvedDomain(ips) => {
                if ips.contains(&ip) {
                    return true;
                }
            }
            AddrRule::WildcardPattern(pattern) => {
                // Reverse-DNS lookup: run blocking call off the async thread.
                let ip_owned = ip;
                let pattern_owned = pattern.clone();
                let hostname =
                    tokio::task::spawn_blocking(move || dns_lookup::lookup_addr(&ip_owned).ok())
                        .await
                        .ok()
                        .flatten();
                if let Some(ref h) = hostname
                    && wildcard_matches(h, &pattern_owned)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Returns `true` if `hostname` matches `pattern`.
///
/// `pattern` may contain `*` in labels at level 3+. Examples:
/// - `*.example.com` matches `foo.example.com` but not `bar.foo.example.com`.
/// - `id-*.docs.example.com` matches `id-123.docs.example.com`.
fn wildcard_matches(hostname: &str, pattern: &str) -> bool {
    let h_parts: Vec<&str> = hostname.trim_end_matches('.').split('.').collect();
    let p_parts: Vec<&str> = pattern.split('.').collect();
    if h_parts.len() != p_parts.len() {
        return false;
    }
    h_parts.iter().zip(p_parts.iter()).all(|(h, p)| {
        if p.contains('*') {
            // Convert glob to a simple prefix/suffix/exact match.
            label_matches_glob(h, p)
        } else {
            h.eq_ignore_ascii_case(p)
        }
    })
}

/// Match a single DNS label against a glob pattern that may contain `*`.
fn label_matches_glob(label: &str, glob: &str) -> bool {
    // Split on '*'; all non-star fragments must appear in order.
    let mut remaining = label;
    let mut parts = glob.split('*').peekable();
    let mut first = true;
    while let Some(part) = parts.next() {
        if first {
            first = false;
            if !remaining.starts_with(part) {
                return false;
            }
            remaining = &remaining[part.len()..];
        } else if parts.peek().is_none() {
            // Last segment: must be a suffix.
            if !remaining.ends_with(part) {
                return false;
            }
            remaining = &remaining[..remaining.len() - part.len()];
        } else {
            // Middle segment: find the next occurrence.
            if let Some(pos) = remaining.find(part) {
                remaining = &remaining[pos + part.len()..];
            } else {
                return false;
            }
        }
    }
    true
}

impl WasiView for PluginLoggingHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

// ── types::Host (empty marker trait) ─────────────────────────────────────────

impl bindings::tool::zeroclaw::plugin::types::Host for PluginLoggingHost {}
impl bindings::memory::zeroclaw::plugin::types::Host for PluginLoggingHost {}
impl bindings::channel::zeroclaw::plugin::types::Host for PluginLoggingHost {}

// ── Core log dispatcher ───────────────────────────────────────────────────────

/// Inner log dispatcher invoked after world-specific type mapping.
///
/// `level_idx`: `0`=Trace, `1`=Debug, `2`=Info, `3`=Warn, `4+`=Error.
fn do_log_record(
    level_idx: u8,
    fn_name: String,
    action: Action,
    outcome: EventOutcome,
    duration_ms: Option<u64>,
    raw_attrs: Option<String>,
    msg: String,
) {
    let mut ev = Event::new(module_path!(), action).with_outcome(outcome);
    if let Some(ms) = duration_ms {
        ev = ev.with_duration(ms);
    }
    let attrs = match raw_attrs {
        Some(raw) => json!({ "plugin_fn": fn_name, "raw": raw }),
        None => json!({ "plugin_fn": fn_name }),
    };
    ev = ev.with_attrs(attrs);
    match level_idx {
        0 => record!(TRACE, ev, msg),
        1 => record!(DEBUG, ev, msg),
        2 => record!(INFO, ev, msg),
        3 => record!(WARN, ev, msg),
        _ => record!(ERROR, ev, msg),
    }
}

// ── logging::Host impls ───────────────────────────────────────────────────────

/// Generate `logging::Host for PluginLoggingHost` for one bindgen world.
///
/// All three worlds produce identical-but-distinct Rust types from the same
/// WIT; the macro eliminates the otherwise triple-repeated match bodies.
macro_rules! impl_logging_host {
    ($world:ident) => {
        impl bindings::$world::zeroclaw::plugin::logging::Host for PluginLoggingHost {
            async fn log_record(
                &mut self,
                level: bindings::$world::zeroclaw::plugin::logging::LogLevel,
                event: bindings::$world::zeroclaw::plugin::logging::PluginEvent,
            ) {
                use bindings::$world::zeroclaw::plugin::logging::LogLevel;
                use bindings::$world::zeroclaw::plugin::logging::PluginAction;
                use bindings::$world::zeroclaw::plugin::logging::PluginOutcome;

                let action = match event.action {
                    PluginAction::Start => Action::Start,
                    PluginAction::Complete => Action::Complete,
                    PluginAction::Fail => Action::Fail,
                    PluginAction::Cancel => Action::Cancel,
                    PluginAction::Skip => Action::Skip,
                    PluginAction::Timeout => Action::Timeout,
                    PluginAction::Retry => Action::Retry,
                    PluginAction::Inbound => Action::Inbound,
                    PluginAction::Outbound => Action::Outbound,
                    PluginAction::Send => Action::Send,
                    PluginAction::Receive => Action::Receive,
                    PluginAction::Connect => Action::Connect,
                    PluginAction::Disconnect => Action::Disconnect,
                    PluginAction::Reconnect => Action::Reconnect,
                    PluginAction::Spawn => Action::Spawn,
                    PluginAction::Kill => Action::Kill,
                    PluginAction::Tick => Action::Tick,
                    PluginAction::Trigger => Action::Trigger,
                    PluginAction::Schedule => Action::Schedule,
                    PluginAction::Approve => Action::Approve,
                    PluginAction::Reject => Action::Reject,
                    PluginAction::Defer => Action::Defer,
                    PluginAction::Read => Action::Read,
                    PluginAction::Write => Action::Write,
                    PluginAction::Delete => Action::Delete,
                    PluginAction::ListAction => Action::List,
                    PluginAction::Query => Action::Query,
                    PluginAction::Invoke => Action::Invoke,
                    PluginAction::Dispatch => Action::Dispatch,
                    PluginAction::Resolve => Action::Resolve,
                    PluginAction::Register => Action::Register,
                    PluginAction::Unregister => Action::Unregister,
                    PluginAction::Load => Action::Load,
                    PluginAction::Save => Action::Save,
                    PluginAction::Migrate => Action::Migrate,
                    PluginAction::Validate => Action::Validate,
                    PluginAction::Note => Action::Note,
                };
                let outcome = match event.outcome {
                    Some(PluginOutcome::Success) => EventOutcome::Success,
                    Some(PluginOutcome::Failure) => EventOutcome::Failure,
                    None => EventOutcome::Unknown,
                };
                let level_idx = match level {
                    LogLevel::Trace => 0,
                    LogLevel::Debug => 1,
                    LogLevel::Info => 2,
                    LogLevel::Warn => 3,
                    LogLevel::Error => 4,
                };
                do_log_record(
                    level_idx,
                    event.function_name,
                    action,
                    outcome,
                    event.duration_ms,
                    event.attrs,
                    event.message,
                );
            }
        }
    };
}

impl_logging_host!(tool);
impl_logging_host!(memory);
impl_logging_host!(channel);

// ── Linker wiring helpers ─────────────────────────────────────────────────────

/// Wire all host interfaces for the `tool-plugin` world into `linker`.
pub fn add_to_linker_tool(
    linker: &mut wasmtime::component::Linker<PluginLoggingHost>,
) -> anyhow::Result<()> {
    // Use feature flags to allow developers to link in wit bindings that aren't stabilized yet.
    let mut options = crate::component::v0::bindings::tool::LinkOptions::default();
    #[cfg(feature = "plugins-wit-v0")]
    {
        options.plugins_wit_v0(true);
    }
    bindings::tool::ToolPlugin::add_to_linker::<PluginLoggingHost, HasSelf<PluginLoggingHost>>(
        linker,
        &options,
        |x| x,
    )
    .map_err(crate::error::PluginError::from)?;
    Ok(())
}

/// Wire all host interfaces for the `memory-plugin` world into `linker`.
pub fn add_to_linker_memory(
    linker: &mut wasmtime::component::Linker<PluginLoggingHost>,
) -> anyhow::Result<()> {
    // Use feature flags to allow developers to link in wit bindings that aren't stabilized yet.
    let mut options = crate::component::v0::bindings::memory::LinkOptions::default();
    #[cfg(feature = "plugins-wit-v0")]
    {
        options.plugins_wit_v0(true);
    }
    bindings::memory::MemoryPlugin::add_to_linker::<PluginLoggingHost, HasSelf<PluginLoggingHost>>(
        linker,
        &options,
        |x| x,
    )
    .map_err(crate::error::PluginError::from)?;
    Ok(())
}

/// Wire all host interfaces for the `channel-plugin` world into `linker`.
pub fn add_to_linker_channel(
    linker: &mut wasmtime::component::Linker<PluginLoggingHost>,
) -> anyhow::Result<()> {
    // Use feature flags to allow developers to link in wit bindings that aren't stabilized yet.
    let mut options = crate::component::v0::bindings::channel::LinkOptions::default();
    #[cfg(feature = "plugins-wit-v0")]
    {
        options.plugins_wit_v0(true);
    }
    bindings::channel::ChannelPlugin::add_to_linker::<
        PluginLoggingHost,
        HasSelf<PluginLoggingHost>,
    >(linker,
        &options, |x| x)
    .map_err(crate::error::PluginError::from)?;
    Ok(())
}

// ── Span and call wrapper ─────────────────────────────────────────────────────

/// Async call wrapper
///
/// Enters a tracing span, emits start/complete trace records with timing,
/// then returns the result of `f.await`.
pub async fn wrap_plugin_call<F, T>(
    plugin_name: &str,
    plugin_version: &str,
    op_name: &str,
    f: F,
) -> T
where
    F: std::future::Future<Output = T>,
{
    // Enter a span for the entire plugin call if the log level is Info, Debug or Trace. This
    // will attach the plugin_name and plugin_version fields to all logs emitted by the plugin
    // during this call.
    let span = info_span!(
        "plugin_call",
        plugin_name = %plugin_name,
        plugin_version = %plugin_version,
    );
    let _guard = span.enter();

    // When tracing, also record the start and end of the call along with its duration.
    record!(
        TRACE,
        Event::new(module_path!(), Action::Invoke)
            .with_attrs(json!({ "plugin": plugin_name, "op": op_name })),
        "plugin call start",
    );
    let start = Instant::now();
    let result = f.await;
    let duration_ms = start.elapsed().as_millis() as u64;
    record!(
        TRACE,
        Event::new(module_path!(), Action::Complete)
            .with_duration(duration_ms)
            .with_attrs(json!({ "plugin": plugin_name, "op": op_name })),
        "plugin call complete",
    );
    result
}

/// Sync call wrapper for use inside `spawn_blocking`.
///
/// Enters a tracing span, emits start/complete trace records with timing,
/// then returns the result of `f()`.
pub fn wrap_plugin_call_sync<F, T>(
    plugin_name: &str,
    plugin_version: &str,
    op_name: &str,
    f: F,
) -> T
where
    F: FnOnce() -> T,
{
    // Enter a span for the entire plugin call if the log level is Info, Debug or Trace. This
    // will attach the plugin_name and plugin_version fields to all logs emitted by the plugin
    // during this call.
    let span = info_span!(
        "plugin_call",
        plugin_name = %plugin_name,
        plugin_version = %plugin_version,
    );
    let _guard = span.enter();

    // When tracing, also record the start and end of the call along with its duration.
    record!(
        TRACE,
        Event::new(module_path!(), Action::Invoke)
            .with_attrs(json!({ "plugin": plugin_name, "op": op_name })),
        "plugin call start",
    );
    let start = Instant::now();
    let result = f();
    let duration_ms = start.elapsed().as_millis() as u64;
    record!(
        TRACE,
        Event::new(module_path!(), Action::Complete)
            .with_duration(duration_ms)
            .with_attrs(json!({ "plugin": plugin_name, "op": op_name })),
        "plugin call complete",
    );
    result
}
