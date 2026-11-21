//! Operator-facing records for plugin egress refusals.
//!
//! `wasi:http`, sockets, and WebSocket share one egress boundary, and a refusal
//! there reaches the guest only as that transport's own error code. The
//! operator needs more than the guest gets: which instance was refused, which
//! host, why, and, when a config change would help, the exact command that
//! grants reach. This module is the one place that record is built, so every
//! transport reports a refusal the same way and an operator searching for
//! `plugin_egress_denied` finds all of them.
//!
//! The requested host is guest input, and so is every reason that quotes it.
//! Both are escaped and bounded as they are formatted, never after: a guest
//! chooses them, can make them as large as its own memory, and can repeat a
//! refused connect once per host call. Rendering one in full before cutting it
//! would let a guest spend host memory outside its own limits.

use std::fmt::{self, Display, Write as _};

use zeroclaw_infra::net_guard::NetworkGuardError;

use crate::egress::{EgressError, EgressHostService, EgressTransport, GrantLists};
use crate::instance::{PluginInstanceId, PluginInstanceScope};

/// The reason recorded when a store carries no egress service at all.
const NO_EGRESS_SERVICE: &str = "no egress policy granted for this instance";

/// The longest recorded host. A DNS name is at most 253 characters, so only a
/// malformed request is ever cut.
const RECORDED_HOST_CHARS: usize = 253;

/// The longest recorded reason.
const RECORDED_REASON_CHARS: usize = 2_048;

/// Record a refusal the egress boundary returned for `host`.
///
/// `service` is the instance's egress service when the refusal came from its
/// policy, so a remedy can carry the grant as it resolves right now. It is
/// `None` for a request refused before any grant was consulted, such as a
/// malformed destination, and such a refusal never carries a command.
///
/// Only refusals at the egress boundary come here. A TLS setup failure after
/// the destination was authorized is not one, and is not recorded.
pub(crate) fn record_refusal(
    scope: &PluginInstanceScope,
    service: Option<&EgressHostService>,
    transport: EgressTransport,
    host: &str,
    error: &EgressError,
) {
    Refusal::of(scope, service, host, error).record(scope.id(), transport, host);
}

/// Record that a store built without an egress service asked for `host`.
pub(crate) fn record_no_egress_service(
    scope: &PluginInstanceScope,
    transport: EgressTransport,
    host: &str,
) {
    Refusal::no_egress_service().record(scope.id(), transport, host);
}

/// What one refusal means to the operator.
enum Refusal {
    /// The destination was refused. `remedy` is the command that repairs it,
    /// when a grant or a private-address carve-out can.
    Denied {
        reason: String,
        remedy: Option<String>,
    },
    /// The destination was granted, but the instance's connection budget was
    /// spent: a ceiling the guest hit, not a verdict on the destination.
    ConnectionLimit { reason: String },
}

impl Refusal {
    fn of(
        scope: &PluginInstanceScope,
        service: Option<&EgressHostService>,
        host: &str,
        error: &EgressError,
    ) -> Self {
        // The error can quote the guest's whole request, so it is formatted
        // through the bound rather than rendered first and cut afterwards.
        let reason = bounded_display(error, RECORDED_REASON_CHARS);
        match error {
            // The destination may well be granted; a grant does not repair a
            // name that did not resolve.
            EgressError::DnsFailed { .. }
            | EgressError::Network(NetworkGuardError::NoAddresses { .. }) => Self::Denied {
                reason,
                remedy: None,
            },
            EgressError::ConnectionLimitReached { .. } => Self::ConnectionLimit { reason },
            _ => Self::Denied {
                // Without the service no grant was consulted, so there is no
                // grant to derive a command from.
                remedy: service.and_then(|service| {
                    let current = service.current_grant(scope);
                    egress_remedy(scope.id(), host, error, current.as_ref())
                }),
                reason,
            },
        }
    }

    /// No configuration grants reach to a store built without an egress
    /// service, so there is no command to print.
    fn no_egress_service() -> Self {
        Self::Denied {
            reason: NO_EGRESS_SERVICE.to_string(),
            remedy: None,
        }
    }

    /// The structured attributes of the record, kept apart from
    /// [`Self::record`] so the shape is testable without a log capture.
    fn attributes(
        &self,
        id: &PluginInstanceId,
        transport: EgressTransport,
        host: &str,
    ) -> serde_json::Value {
        let (reason, remedy, error_key) = match self {
            Self::Denied { reason, remedy } => (reason, Some(remedy), "plugin_egress_denied"),
            Self::ConnectionLimit { reason } => (reason, None, "plugin_egress_connection_limit"),
        };
        let mut attributes = serde_json::json!({
            "plugin": id.package(),
            "capability": format!("{:?}", id.capability()),
            "binding": id.binding(),
            "transport": transport_family(transport),
            // A malformed host is raw guest text; escaping keeps control,
            // bidirectional, and zero-width characters out of the log.
            "host": bounded_display(&host.escape_debug(), RECORDED_HOST_CHARS),
            "reason": reason,
            "error_key": error_key,
        });
        // A budget refusal has no remedy key at all: no command repairs it.
        if let (Some(remedy), Some(object)) = (remedy, attributes.as_object_mut()) {
            object.insert("remedy".to_string(), serde_json::json!(remedy));
        }
        attributes
    }

    /// Emit the structured event that attributes the attempt to the exact
    /// instance. The host and the boundary's reason are recorded host-side,
    /// because the operator needs both to seed a grant, while the guest only
    /// ever sees its transport's masked error.
    fn record(self, id: &PluginInstanceId, transport: EgressTransport, host: &str) {
        let attributes = self.attributes(id, transport, host);
        match self {
            Self::Denied { .. } => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(attributes),
                "Denied plugin outbound request by egress policy"
            ),
            // Its own message and `error_key`, so an operator searching for
            // egress denials does not mistake a budget refusal for a missing
            // grant, the same distinction the guest sees in its error code.
            Self::ConnectionLimit { .. } => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(attributes),
                "Refused plugin outbound request: the instance's connection budget is exhausted"
            ),
        }
    }
}

/// The adapter family a transport belongs to, as operators name it.
fn transport_family(transport: EgressTransport) -> &'static str {
    match transport {
        EgressTransport::Http { .. } => "http",
        EgressTransport::WebSocket { .. } => "websocket",
        EgressTransport::Tcp | EgressTransport::Tls | EgressTransport::StartTls => "socket",
    }
}

/// `value` formatted to at most `max_chars` characters, marked with an
/// ellipsis when it was cut.
///
/// Formatting stops at the budget: the sink refuses the first character past
/// it, and the refusal ends the formatter's work. However large the value, at
/// most `max_chars` characters are ever rendered.
fn bounded_display(value: &impl Display, max_chars: usize) -> String {
    struct Capped {
        text: String,
        remaining: usize,
        cut: bool,
    }

    impl fmt::Write for Capped {
        fn write_str(&mut self, fragment: &str) -> fmt::Result {
            for character in fragment.chars() {
                if self.remaining == 0 {
                    self.cut = true;
                    return Err(fmt::Error);
                }
                self.text.push(character);
                self.remaining -= 1;
            }
            Ok(())
        }
    }

    let mut capped = Capped {
        text: String::new(),
        remaining: max_chars,
        cut: false,
    };
    // An error here is the sink stopping at its budget, which `cut` records;
    // a value's own formatting failure leaves what it wrote so far.
    let _ = write!(capped, "{value}");
    if capped.cut {
        capped.text.push('…');
    }
    capped.text
}

/// Wrap `value` as one POSIX single-quoted shell word. An apostrophe inside
/// closes the quoted run, contributes a backslash-escaped apostrophe outside
/// it, and reopens the run, so the word stays one argument with the original
/// bytes. This is the quoting `shell_escape` in `zeroclaw-runtime`'s
/// `coding_cli_executor` applies to generated commands.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// `existing` with `host` appended when it is not already there, rendered as
/// one single-quoted shell argument carrying the JSON list `config set` takes.
/// `config set` replaces the whole list, so a remedy has to carry every entry
/// the operator already has.
fn list_with(existing: &[String], host: &str) -> String {
    let mut list: Vec<&str> = existing.iter().map(String::as_str).collect();
    if !list.contains(&host) {
        list.push(host);
    }
    shell_quote(&serde_json::Value::from(list).to_string())
}

/// The operator-facing next step for a missing grant: the exact command that
/// grants this instance reach to the refused host while keeping every host it
/// already has. Without the current list in hand, it says what to do in words
/// rather than print a replacement that would revoke the others. Kept separate
/// from [`Refusal::record`] so its format is unit-testable without a log
/// capture. Returns `None` only if the instance identity cannot be encoded (it
/// always can for an admitted instance).
fn egress_grant_remedy(
    id: &PluginInstanceId,
    host: &str,
    existing: Option<&[String]>,
) -> Option<String> {
    let key = id.config_entry_key().ok()?;
    let field = format!("plugins.entries.{key}.egress_hosts");
    Some(match existing {
        Some(existing) => format!(
            "grant reach with: zeroclaw config set {field} {}",
            list_with(existing, host)
        ),
        None => format!(
            "grant reach by adding \"{host}\" to {field} alongside its existing entries; `config set` replaces the whole list"
        ),
    })
}

/// The remedy for a granted host that resolved to a private, loopback or
/// link-local address: the grant is in place, what is missing is the
/// per-host `egress_allow_private` carveout, added to the carveouts already
/// there.
fn egress_private_remedy(
    id: &PluginInstanceId,
    host: &str,
    existing: Option<&[String]>,
) -> Option<String> {
    let key = id.config_entry_key().ok()?;
    let field = format!("plugins.entries.{key}.egress_allow_private");
    let preface = "the host is granted but resolves to a private, loopback or link-local address;";
    Some(match existing {
        Some(existing) => format!(
            "{preface} allow that address class for it with: zeroclaw config set {field} {}",
            list_with(existing, host)
        ),
        None => format!(
            "{preface} allow that address class for it by adding \"{host}\" to {field} alongside its existing entries; `config set` replaces the whole list"
        ),
    })
}

/// The remedy that matches what the policy refused, or `None` when no
/// configuration change would help.
///
/// A missing destination grant is fixed by `egress_hosts`; a granted host that
/// resolved into private address space needs `egress_allow_private` as well,
/// and telling the operator to add the grant again would leave the request
/// denied. A missing manifest permission, a cloud-metadata address, a
/// malformed destination or a DNS failure have no config-set fix, so those
/// carry no command at all rather than a misleading one. `current` is the
/// instance's grant as it resolves now, so a printed command keeps it intact.
fn egress_remedy(
    id: &PluginInstanceId,
    host: &str,
    error: &EgressError,
    current: Option<&GrantLists>,
) -> Option<String> {
    match error {
        EgressError::DestinationNotGranted { .. } => {
            egress_grant_remedy(id, host, current.map(|c| c.hosts.as_slice()))
        }
        EgressError::Network(
            NetworkGuardError::PrivateHostDenied(_)
            | NetworkGuardError::PrivateNetworkDenied { .. },
        ) => egress_private_remedy(id, host, current.map(|c| c.allow_private.as_slice())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egress::{EgressPolicy, EgressPolicyResolver, EgressRequest};
    use crate::{PluginCapability, PluginPermission};

    #[test]
    fn egress_grant_remedy_names_the_key_field_and_host() {
        // A denied instance's operator-facing next step must be a runnable
        // command that names the exact grant row, the field, and the host —
        // otherwise the denial is a dead end.
        let scope = crate::instance::test_scope(
            PluginCapability::Tool,
            "main",
            [PluginPermission::HttpClient],
        );
        let id = scope.id();
        let key = id
            .config_entry_key()
            .expect("an admitted instance has a config-entry key");
        let existing = vec!["docs.example.com".to_string()];
        let remedy = egress_grant_remedy(id, "api.example.com", Some(&existing))
            .expect("an admitted instance always yields a remedy");
        assert!(
            remedy.contains("docs.example.com"),
            "remedy must keep the host already granted: {remedy}"
        );
        assert!(
            remedy.contains(&key),
            "remedy must name the exact config-entry key: {remedy}"
        );
        assert!(
            remedy.contains("plugins.entries."),
            "remedy must target the plugins.entries path: {remedy}"
        );
        assert!(
            remedy.contains("egress_hosts"),
            "remedy must name the egress_hosts field: {remedy}"
        );
        assert!(
            remedy.contains("api.example.com"),
            "remedy must name the denied host: {remedy}"
        );
        assert!(
            remedy.contains("config set"),
            "remedy must be a runnable config-set command: {remedy}"
        );
    }

    /// A grant with no quoting-sensitive characters keeps the exact command the
    /// remedy has always printed. Only entries that need escaping change.
    #[test]
    fn a_remedy_for_plain_grants_keeps_the_command_unchanged() {
        let scope = crate::instance::test_scope(
            PluginCapability::Tool,
            "main",
            [PluginPermission::HttpClient],
        );
        let id = scope.id();
        let key = id
            .config_entry_key()
            .expect("an admitted instance has a config-entry key");
        let existing = vec!["docs.example.com".to_string()];
        let remedy = egress_grant_remedy(id, "new.example.com", Some(&existing))
            .expect("a missing grant has a fix");
        let expected = format!(
            "grant reach with: zeroclaw config set plugins.entries.{key}.egress_hosts '[\"docs.example.com\",\"new.example.com\"]'"
        );
        assert_eq!(remedy, expected, "{remedy}");
    }

    /// The serialized list was wrapped in single quotes without escaping the
    /// apostrophes inside it, so a grant like `o'brien.example` closed the
    /// quoted run early and left the operator with an unterminated quote. The
    /// grant grammar now rejects such an entry before it reaches a policy, so
    /// none should arrive here; the command must still be one shell word for
    /// whatever list it is handed, rather than relying on a grammar elsewhere
    /// to keep it well-formed.
    #[test]
    fn a_remedy_for_a_grant_with_an_apostrophe_is_one_shell_argument() {
        let scope = crate::instance::test_scope(
            PluginCapability::Tool,
            "main",
            [PluginPermission::HttpClient],
        );
        let id = scope.id();
        let key = id
            .config_entry_key()
            .expect("an admitted instance has a config-entry key");
        let existing = vec!["o'brien.example".to_string()];
        let remedy = egress_grant_remedy(id, "new.example.com", Some(&existing))
            .expect("a missing grant has a fix");
        let expected = format!(
            "grant reach with: zeroclaw config set plugins.entries.{key}.egress_hosts '[\"o'\\''brien.example\",\"new.example.com\"]'"
        );
        assert_eq!(remedy, expected, "{remedy}");
    }

    /// The same command read back the way a POSIX shell reads it has to carry
    /// the list `config set` replaces, apostrophe and all. Whether that list
    /// then builds a policy is the grant grammar's call, not the quoting's:
    /// the shell must hand `config set` the operator's list byte for byte so
    /// the grammar is what judges it, not a truncated copy.
    #[test]
    fn a_remedy_for_a_grant_with_an_apostrophe_round_trips() {
        let scope = crate::instance::test_scope(
            PluginCapability::Tool,
            "main",
            [PluginPermission::HttpClient],
        );
        let id = scope.id();
        let existing = vec![
            "docs.example.com".to_string(),
            "o'brien.example".to_string(),
        ];
        let remedy = egress_grant_remedy(id, "new.example.com", Some(&existing))
            .expect("a missing grant has a fix");
        let written = remedy_list(&remedy);
        assert_eq!(
            written,
            vec![
                "docs.example.com".to_string(),
                "o'brien.example".to_string(),
                "new.example.com".to_string()
            ],
            "{remedy}"
        );
    }

    /// The escape sequence is data, not syntax: a grant that already contains
    /// `'\''`, adjacent apostrophes or a trailing one must survive unchanged
    /// rather than being escaped a second time.
    #[test]
    fn a_grant_containing_the_escape_sequence_round_trips() {
        for value in [
            "a'\\''b.example",
            "o''brien.example",
            "trailing'",
            "'leading",
        ] {
            let quoted = shell_quote(value);
            assert_eq!(shell_argument(&quoted), value, "{quoted}");
        }
    }

    /// The remedy names the field each refusal actually needs, and is absent
    /// where no config change would help: a granted host that resolved into
    /// private address space must be pointed at `egress_allow_private`, not
    /// told to add a grant it already has.
    #[test]
    fn egress_remedy_matches_the_refusal() {
        let scope = crate::instance::test_scope(
            PluginCapability::Tool,
            "main",
            [PluginPermission::HttpClient],
        );
        let id = scope.id();
        let host = "gitea.internal.example";
        let current = GrantLists {
            hosts: vec![host.to_string()],
            allow_private: Vec::new(),
        };

        let not_granted = EgressError::DestinationNotGranted {
            instance: "main".to_string(),
            host: host.to_string(),
        };
        let remedy = egress_remedy(id, host, &not_granted, Some(&current))
            .expect("a missing grant has a fix");
        assert!(
            remedy.contains("egress_hosts") && !remedy.contains("egress_allow_private"),
            "{remedy}"
        );

        let private = EgressError::Network(NetworkGuardError::PrivateNetworkDenied {
            host: host.to_string(),
            reason: "resolved to 10.0.0.5".to_string(),
        });
        let remedy =
            egress_remedy(id, host, &private, Some(&current)).expect("a private address has a fix");
        assert!(
            remedy.contains(&format!(".egress_allow_private '[\"{host}\"]'")),
            "the private case must name the carveout field and the host: {remedy}"
        );
        let literal = EgressError::Network(NetworkGuardError::PrivateHostDenied(host.to_string()));
        assert!(
            egress_remedy(id, host, &literal, Some(&current))
                .is_some_and(|r| r.contains("egress_allow_private"))
        );

        for no_fix in [
            EgressError::DnsFailed {
                host: host.to_string(),
                port: 443,
                reason: "no such host".to_string(),
            },
            EgressError::Network(NetworkGuardError::NoAddresses {
                host: host.to_string(),
                port: 443,
            }),
            EgressError::Network(NetworkGuardError::CloudMetadata {
                host: host.to_string(),
                reason: "metadata endpoint".to_string(),
            }),
            EgressError::PermissionDenied {
                transport: crate::egress::EgressTransport::Http { encrypted: true },
                permission: PluginPermission::HttpClient,
            },
        ] {
            assert!(
                egress_remedy(id, host, &no_fix, Some(&current)).is_none(),
                "no config-set command repairs {no_fix}"
            );
        }
    }

    /// Through the real policy: a host that is granted but not carved out
    /// resolves into loopback space, is refused as a private-network denial,
    /// and the remedy for that refusal names `egress_allow_private`.
    #[tokio::test]
    async fn a_granted_private_host_without_the_carveout_is_pointed_at_allow_private() {
        let scope = crate::instance::test_scope(
            PluginCapability::Tool,
            "main",
            [PluginPermission::HttpClient],
        );
        let policy = EgressPolicy::new(&["127.0.0.1".to_string()], &[], &[], 16)
            .expect("a loopback grant is a valid policy");
        let service = EgressHostService::with_private_connection_accounting(
            EgressPolicyResolver::new(move |_| Ok(policy.clone())),
        );
        let request = crate::egress::EgressRequest::new(
            scope.clone(),
            crate::egress::EgressTransport::Http { encrypted: false },
            "127.0.0.1",
            80,
        )
        .expect("a loopback destination is a valid request");
        let error = service
            .authorize(request)
            .await
            .expect_err("granted but not carved out must be refused");
        // A literal loopback host is refused up front as a private host; a
        // granted name that resolves into private space is refused after
        // resolution. Both are the same operator situation.
        assert!(
            matches!(
                error,
                EgressError::Network(
                    NetworkGuardError::PrivateHostDenied(_)
                        | NetworkGuardError::PrivateNetworkDenied { .. }
                )
            ),
            "{error}"
        );
        let current = service.current_grant(&scope);
        let remedy =
            egress_remedy(scope.id(), "127.0.0.1", &error, current.as_ref()).expect("has a fix");
        assert!(remedy.contains("egress_allow_private"), "{remedy}");
    }

    /// The list a remedy command would hand to `config set`, read off the
    /// command the way a POSIX shell would read it.
    fn remedy_list(remedy: &str) -> Vec<String> {
        let argument = remedy
            .split_once("config set ")
            .and_then(|(_, tail)| tail.split_once(' '))
            .map(|(_, argument)| argument)
            .unwrap_or_else(|| panic!("no config-set argument in the remedy: {remedy}"));
        serde_json::from_str(&shell_argument(argument)).expect("the list is JSON")
    }

    /// Resolve one single-quoted shell argument. [`shell_quote`] emits a quoted
    /// run, and for an apostrophe it splices out to a backslash-escaped quote
    /// and back in; anything else after a closing quote is an unterminated
    /// quote, which is exactly the defect here, so this fails loudly there
    /// instead of quietly decoding a truncated list.
    fn shell_argument(raw: &str) -> String {
        let mut out = String::new();
        let mut rest = raw
            .strip_prefix('\'')
            .unwrap_or_else(|| panic!("the argument must open with a quote: {raw}"));
        loop {
            let end = rest
                .find('\'')
                .unwrap_or_else(|| panic!("unterminated quote in the argument: {raw}"));
            out.push_str(&rest[..end]);
            rest = &rest[end + 1..];
            if rest.is_empty() {
                return out;
            }
            rest = rest
                .strip_prefix("\\''")
                .unwrap_or_else(|| panic!("text after the closing quote in the argument: {raw}"));
            out.push('\'');
        }
    }

    /// `config set` replaces a whole list, so a remedy that printed only the
    /// denied host would revoke every other grant, and replacing
    /// `egress_hosts` alone can also strand an existing private carveout
    /// outside the host list and leave the policy invalid. Starting from an
    /// instance with several hosts and a carveout, both remedies must keep
    /// every existing entry, and the lists they would write must still build
    /// a valid policy. The singleton form is shown to fail that check, so the
    /// test would catch a regression to it.
    #[tokio::test]
    async fn a_remedy_keeps_every_existing_grant_and_leaves_a_valid_policy() {
        let scope = crate::instance::test_scope(
            PluginCapability::Tool,
            "main",
            [PluginPermission::HttpClient],
        );
        let hosts = vec![
            "api.example.com".to_string(),
            "*.cdn.example.com".to_string(),
            "nas.internal.example".to_string(),
            "127.0.0.1".to_string(),
        ];
        let allow_private = vec!["nas.internal.example".to_string()];
        let policy = EgressPolicy::new(&hosts, &allow_private, &[], 16)
            .expect("the starting grant is a valid policy");
        let service = EgressHostService::with_private_connection_accounting(
            EgressPolicyResolver::new(move |_| Ok(policy.clone())),
        );
        let current = service
            .current_grant(&scope)
            .expect("the current grant resolves");
        // The policy holds its lists normalized, so compare them as sets.
        let sorted = |v: &[String]| {
            let mut v = v.to_vec();
            v.sort();
            v
        };
        assert_eq!(sorted(&current.hosts), sorted(&hosts));
        assert_eq!(sorted(&current.allow_private), sorted(&allow_private));

        // A host outside the grant: refused before any resolution.
        let request = crate::egress::EgressRequest::new(
            scope.clone(),
            crate::egress::EgressTransport::Http { encrypted: true },
            "new.example.com",
            443,
        )
        .expect("a valid destination");
        let error = service
            .authorize(request)
            .await
            .expect_err("an ungranted host is refused");
        assert!(
            matches!(error, EgressError::DestinationNotGranted { .. }),
            "{error}"
        );
        let remedy = egress_remedy(scope.id(), "new.example.com", &error, Some(&current))
            .expect("a missing grant has a fix");
        let written = remedy_list(&remedy);
        for kept in &hosts {
            assert!(written.contains(kept), "{kept} must survive: {remedy}");
        }
        assert!(written.contains(&"new.example.com".to_string()), "{remedy}");
        EgressPolicy::new(&written, &allow_private, &[], 16)
            .expect("the remedied host list keeps the policy valid");
        assert!(
            EgressPolicy::new(&["new.example.com".to_string()], &allow_private, &[], 16).is_err(),
            "the singleton replacement strands the existing carveout"
        );

        // A granted host in private space: the carveout list keeps its entry.
        let request = crate::egress::EgressRequest::new(
            scope.clone(),
            crate::egress::EgressTransport::Http { encrypted: false },
            "127.0.0.1",
            80,
        )
        .expect("a loopback destination is a valid request");
        let error = service
            .authorize(request)
            .await
            .expect_err("granted but not carved out is refused");
        let remedy = egress_remedy(scope.id(), "127.0.0.1", &error, Some(&current))
            .expect("a private address has a fix");
        let written = remedy_list(&remedy);
        assert_eq!(
            sorted(&written),
            sorted(&["nas.internal.example".to_string(), "127.0.0.1".to_string()]),
            "{remedy}"
        );
        EgressPolicy::new(&hosts, &written, &[], 16)
            .expect("the remedied carveout list keeps the policy valid");
    }

    /// Without the current grant in hand there is no safe list to print, so
    /// the remedy says what to do in words and prints no replacement.
    #[test]
    fn without_the_current_grant_the_remedy_prints_no_replacement() {
        let scope = crate::instance::test_scope(
            PluginCapability::Tool,
            "main",
            [PluginPermission::HttpClient],
        );
        let id = scope.id();
        let not_granted = EgressError::DestinationNotGranted {
            instance: "main".to_string(),
            host: "new.example.com".to_string(),
        };
        let private = EgressError::Network(NetworkGuardError::PrivateHostDenied(
            "127.0.0.1".to_string(),
        ));
        for (host, error) in [("new.example.com", not_granted), ("127.0.0.1", private)] {
            let remedy = egress_remedy(id, host, &error, None).expect("still has a fix");
            assert!(!remedy.contains("zeroclaw config set"), "{remedy}");
            assert!(
                remedy.contains("alongside its existing entries"),
                "{remedy}"
            );
            assert!(remedy.contains(host), "{remedy}");
        }
    }

    /// The transports an operator can see in a record, one per adapter.
    const EVERY_TRANSPORT: [EgressTransport; 7] = [
        EgressTransport::Http { encrypted: false },
        EgressTransport::Http { encrypted: true },
        EgressTransport::WebSocket { encrypted: false },
        EgressTransport::WebSocket { encrypted: true },
        EgressTransport::Tcp,
        EgressTransport::Tls,
        EgressTransport::StartTls,
    ];

    fn socket_scope() -> PluginInstanceScope {
        crate::instance::test_scope(
            PluginCapability::Channel,
            "main",
            [
                PluginPermission::HttpClient,
                PluginPermission::WebSocketClient,
                PluginPermission::SocketClient,
            ],
        )
    }

    /// A service whose grant is `hosts`, with private accounting so a test
    /// never shares a connection budget with another.
    fn granting(hosts: &[&str]) -> EgressHostService {
        let hosts: Vec<String> = hosts.iter().map(|host| (*host).to_string()).collect();
        let policy =
            EgressPolicy::new(&hosts, &[], &[], 16).expect("the test grant is a valid policy");
        EgressHostService::with_private_connection_accounting(EgressPolicyResolver::new(
            move |_| Ok(policy.clone()),
        ))
    }

    #[test]
    fn every_transport_is_recorded_under_its_adapter_family() {
        let families: Vec<&str> = EVERY_TRANSPORT.into_iter().map(transport_family).collect();
        assert_eq!(
            families,
            [
                "http",
                "http",
                "websocket",
                "websocket",
                "socket",
                "socket",
                "socket"
            ]
        );
    }

    /// The same refusal yields the same record on every transport: the
    /// transport changes the `transport` attribute and nothing else.
    #[test]
    fn a_refusal_is_recorded_the_same_way_on_every_transport() {
        let scope = socket_scope();
        let service = granting(&["docs.example.com"]);
        let host = "api.example.com";
        let not_granted = EgressError::DestinationNotGranted {
            instance: "main".to_string(),
            host: host.to_string(),
        };
        let unresolved = EgressError::DnsFailed {
            host: host.to_string(),
            port: 443,
            reason: "no such host".to_string(),
        };
        let budget = EgressError::ConnectionLimitReached {
            instance: "main".to_string(),
            limit: 16,
        };

        for transport in EVERY_TRANSPORT {
            let family = transport_family(transport);

            let record = Refusal::of(&scope, Some(&service), host, &not_granted).attributes(
                scope.id(),
                transport,
                host,
            );
            assert_eq!(record["transport"], family);
            assert_eq!(record["error_key"], "plugin_egress_denied");
            assert_eq!(record["host"], host);
            let remedy = record["remedy"]
                .as_str()
                .unwrap_or_else(|| panic!("a missing grant has a fix on {family}: {record}"));
            assert!(
                remedy.contains("egress_hosts") && remedy.contains("docs.example.com"),
                "the remedy keeps the current grant on {family}: {remedy}"
            );

            let record = Refusal::of(&scope, Some(&service), host, &unresolved).attributes(
                scope.id(),
                transport,
                host,
            );
            assert_eq!(record["error_key"], "plugin_egress_denied");
            assert!(
                record["remedy"].is_null(),
                "no grant repairs a name that did not resolve: {record}"
            );

            let record = Refusal::of(&scope, Some(&service), host, &budget).attributes(
                scope.id(),
                transport,
                host,
            );
            assert_eq!(record["error_key"], "plugin_egress_connection_limit");
            assert!(
                record.get("remedy").is_none(),
                "a budget refusal carries no remedy: {record}"
            );
        }
    }

    /// Through the real policy: a socket connect to an ungranted host is
    /// refused before resolution, and its record names the socket family and
    /// the command that grants it.
    #[tokio::test]
    async fn a_refused_socket_connect_is_recorded_with_its_grant_command() {
        let scope = socket_scope();
        let service = granting(&["docs.example.com"]);
        let request =
            EgressRequest::new(scope.clone(), EgressTransport::Tls, "api.example.com", 6697)
                .expect("a valid destination");
        let error = service
            .authorize(request)
            .await
            .expect_err("an ungranted host is refused");
        let record = Refusal::of(&scope, Some(&service), "api.example.com", &error).attributes(
            scope.id(),
            EgressTransport::Tls,
            "api.example.com",
        );
        assert_eq!(record["transport"], "socket");
        assert_eq!(record["plugin"], scope.id().package());
        assert_eq!(record["binding"], "main");
        let remedy = record["remedy"]
            .as_str()
            .expect("a missing grant has a fix");
        assert_eq!(
            remedy_list(remedy),
            [
                "docs.example.com".to_string(),
                "api.example.com".to_string()
            ]
        );
    }

    #[test]
    fn a_store_without_an_egress_service_is_recorded_without_a_remedy() {
        let scope = socket_scope();
        let record = Refusal::no_egress_service().attributes(
            scope.id(),
            EgressTransport::WebSocket { encrypted: true },
            "gateway.example.com",
        );
        assert_eq!(record["error_key"], "plugin_egress_denied");
        assert_eq!(record["reason"], NO_EGRESS_SERVICE);
        assert!(record["remedy"].is_null(), "{record}");
        assert_eq!(record["transport"], "websocket");
    }

    /// A malformed destination is refused before any grant is consulted: no
    /// command repairs it, even with a service attached.
    #[test]
    fn a_malformed_destination_is_recorded_without_a_remedy() {
        let scope = socket_scope();
        let service = granting(&["docs.example.com"]);
        let host = "not a host";
        let error = EgressRequest::new(scope.clone(), EgressTransport::Tcp, host, 6667)
            .map(|_| ())
            .expect_err("a malformed host is refused");
        let record = Refusal::of(&scope, Some(&service), host, &error).attributes(
            scope.id(),
            EgressTransport::Tcp,
            host,
        );
        assert!(record["remedy"].is_null(), "{record}");
    }

    /// Without the service no grant was consulted, so even a refusal a grant
    /// would repair carries no command.
    #[test]
    fn a_refusal_without_a_service_carries_no_remedy() {
        let scope = socket_scope();
        let not_granted = EgressError::DestinationNotGranted {
            instance: "main".to_string(),
            host: "api.example.com".to_string(),
        };
        let record = Refusal::of(&scope, None, "api.example.com", &not_granted).attributes(
            scope.id(),
            EgressTransport::Tcp,
            "api.example.com",
        );
        assert!(record["remedy"].is_null(), "{record}");
    }

    /// The guest chooses the requested host, and the reason quotes it. Both
    /// are bounded in the record, whatever the guest sent.
    #[test]
    fn guest_supplied_text_is_bounded_in_the_record() {
        let scope = socket_scope();
        let host = "\u{7f}".repeat(1 << 20);
        let error = EgressRequest::new(scope.clone(), EgressTransport::Tcp, &host, 6667)
            .map(|_| ())
            .expect_err("an oversized host is refused");
        let record = Refusal::of(&scope, None, &host, &error).attributes(
            scope.id(),
            EgressTransport::Tcp,
            &host,
        );
        let recorded_host = record["host"].as_str().expect("the host is recorded");
        let recorded_reason = record["reason"].as_str().expect("the reason is recorded");
        assert_eq!(recorded_host.chars().count(), RECORDED_HOST_CHARS + 1);
        assert!(recorded_host.ends_with('…'));
        assert_eq!(recorded_reason.chars().count(), RECORDED_REASON_CHARS + 1);
        assert!(recorded_reason.ends_with('…'));
    }

    /// Bounding happens while formatting, not after it: a value that would
    /// never stop writing is cut at the budget. This is what keeps an escaped
    /// guest string, several times the size of its input, from being rendered
    /// in full on the host.
    #[test]
    fn bounded_formatting_stops_at_the_budget() {
        struct Endless(std::cell::Cell<usize>);
        impl Display for Endless {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                loop {
                    fmt::Write::write_char(f, 'x')?;
                    self.0.set(self.0.get() + 1);
                }
            }
        }

        let endless = Endless(std::cell::Cell::new(0));
        let text = bounded_display(&endless, 64);
        assert_eq!(text.chars().count(), 65);
        assert!(text.ends_with('…'));
        assert_eq!(endless.0.get(), 64, "formatting stopped at the budget");
    }

    /// A raw host can carry characters that would reorder or hide text in a
    /// log view. They are recorded escaped.
    #[test]
    fn a_recorded_host_is_escaped() {
        let scope = socket_scope();
        let host = "\u{202e}evil\u{200b}.example";
        let record =
            Refusal::no_egress_service().attributes(scope.id(), EgressTransport::Tcp, host);
        let recorded = record["host"].as_str().expect("the host is recorded");
        assert!(
            !recorded.contains('\u{202e}') && !recorded.contains('\u{200b}'),
            "{recorded}"
        );
        assert!(recorded.contains("\\u{202e}"), "{recorded}");
        assert_eq!(
            Refusal::no_egress_service().attributes(
                scope.id(),
                EgressTransport::Tcp,
                "gateway.example.com"
            )["host"],
            "gateway.example.com",
            "a valid host is recorded unchanged"
        );
    }

    #[test]
    fn bounded_cuts_on_a_character_boundary() {
        assert_eq!(bounded_display(&"short", 10), "short");
        assert_eq!(bounded_display(&"exactly", 7), "exactly");
        assert_eq!(bounded_display(&"ééééé", 3), "ééé…");
        assert_eq!(bounded_display(&"", 0), "");
        assert_eq!(bounded_display(&"x", 0), "…");
    }
}
