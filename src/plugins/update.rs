//! `zeroclaw plugin update`: the pure half.
//!
//! The runtime grants a plugin every permission its manifest requests, so a
//! replacement that requests more than the installed version would widen what
//! runs with no operator act. This module computes that increase, checks the
//! `--allow` items that accept it, and renders the exact command that repeats
//! an update with them. Every user-facing string stays in the CLI so it routes
//! through Fluent.

use std::path::Path;

use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

use super::egress_ceremony::{ShellDialect, zeroclaw_invocation_for};

/// One piece of authority a replacement requests beyond the installed version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorityItem {
    /// A permission the installed version does not request.
    Permission(PluginPermission),
    /// A capability the installed version does not declare.
    Capability(PluginCapability),
    /// A `provides` channel id the installed version does not declare.
    Provides(String),
    /// A publisher other than the installed version's: another key, or none
    /// where the installed version named one.
    Publisher(Option<String>),
}

impl AuthorityItem {
    /// How `--allow` names this item: `permission:<name>`, `capability:<name>`,
    /// `provides:<id>`, or `publisher:<key>`, with `publisher:unsigned` for a
    /// replacement that names no publisher key. Permission and capability
    /// names are spelled as a manifest spells them.
    #[must_use]
    pub fn token(&self) -> String {
        match self {
            Self::Permission(permission) => {
                format!("permission:{}", manifest_spelling(permission))
            }
            Self::Capability(capability) => {
                format!("capability:{}", manifest_spelling(capability))
            }
            Self::Provides(id) => format!("provides:{id}"),
            Self::Publisher(Some(key)) => format!("publisher:{key}"),
            Self::Publisher(None) => "publisher:unsigned".to_string(),
        }
    }
}

/// How a manifest spells a permission or capability: its serde name. An empty
/// result can never be accepted, because an `--allow` item needs a value.
fn manifest_spelling(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The authority `candidate` requests beyond what `installed` has, in manifest
/// order.
///
/// Narrowing is never an increase: dropping a permission, a capability or a
/// `provides` id, or gaining a signature, needs no approval.
#[must_use]
pub fn authority_increase(
    installed: &PluginManifest,
    candidate: &PluginManifest,
) -> Vec<AuthorityItem> {
    let mut increase = Vec::new();
    let mut add = |item: AuthorityItem| {
        if !increase.contains(&item) {
            increase.push(item);
        }
    };
    for permission in &candidate.permissions {
        if !installed.permissions.contains(permission) {
            add(AuthorityItem::Permission(*permission));
        }
    }
    for capability in &candidate.capabilities {
        if !installed.capabilities.contains(capability) {
            add(AuthorityItem::Capability(*capability));
        }
    }
    if let Some(provides) = &candidate.provides
        && installed.provides.as_ref() != Some(provides)
    {
        add(AuthorityItem::Provides(provides.clone()));
    }
    if installed.publisher_key.is_some() && candidate.publisher_key != installed.publisher_key {
        add(AuthorityItem::Publisher(candidate.publisher_key.clone()));
    }
    increase
}

/// Whether `allowed` accepts every item of `increase`.
#[must_use]
pub fn accepts_all(increase: &[AuthorityItem], allowed: &[String]) -> bool {
    increase.iter().all(|item| allowed.contains(&item.token()))
}

/// The first `--allow` item that is not `<kind>:<value>` with a known kind and
/// a value, if any.
#[must_use]
pub fn malformed_allow_item(allowed: &[String]) -> Option<&str> {
    allowed.iter().map(String::as_str).find(|item| {
        item.split_once(':').is_none_or(|(kind, value)| {
            value.is_empty()
                || !matches!(kind, "permission" | "capability" | "provides" | "publisher")
        })
    })
}

/// `text` with every control, format, line separator and paragraph separator
/// character (Unicode categories Cc, Cf, Zl and Zp) written as an escape, for
/// printing publisher-controlled text to a terminal: an escape sequence, a
/// reordering mark or an invisible character in a version, a `provides` id or
/// a schema location cannot rewrite, rearrange or disguise what the operator
/// reads where they decide what to accept. Other text is unchanged.
#[must_use]
pub fn printable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() || is_format_or_separator(c) {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether `c` is in Unicode category Cf (format), Zl (line separator) or Zp
/// (paragraph separator). The standard library exposes no general category,
/// so the Cf ranges are listed here, as of Unicode 16.
fn is_format_or_separator(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

/// Whether `candidate` is an older version than `installed`, when both are
/// semantic versions, by SemVer precedence: build metadata does not count, and
/// a pre-release is older than its release. Update picks the version the
/// registry lists without ordering versions, so this only reports a step back
/// and never refuses one.
#[must_use]
pub fn is_older_version(candidate: &str, installed: &str) -> bool {
    match (
        semver::Version::parse(candidate),
        semver::Version::parse(installed),
    ) {
        (Ok(candidate), Ok(installed)) => candidate.cmp_precedence(&installed).is_lt(),
        _ => false,
    }
}

/// Where a replacement comes from, as a printed update command names it.
#[derive(Clone, Copy, Debug)]
pub enum UpdateSource<'a> {
    /// The registry, pinned to `version` when there is one. `registry` is the
    /// `--registry` URL the operator passed, which the printed command passes
    /// again; a registry chosen by the environment is inherited without it.
    Registry {
        version: Option<&'a str>,
        registry: Option<&'a str>,
    },
    /// A local package directory.
    Local(&'a Path),
}

/// The `plugin update` command for `name` from `source`, accepting
/// `increase` with `--allow` when it is not empty, quoted for this host's
/// shell like every other printed command.
#[must_use]
pub fn update_command(
    config_dir: &Path,
    name: &str,
    source: UpdateSource<'_>,
    increase: &[AuthorityItem],
) -> String {
    update_command_for(ShellDialect::host(), config_dir, name, source, increase)
}

/// The `plugin install` command for `name` from `source`: the local
/// directory, or the registry entry, pinned when there is a version, quoted
/// like every other printed command.
#[must_use]
pub fn install_command(config_dir: &Path, name: &str, source: UpdateSource<'_>) -> String {
    install_command_for(ShellDialect::host(), config_dir, name, source)
}

/// [`install_command`] rendered for an explicit shell dialect.
#[must_use]
pub fn install_command_for(
    dialect: ShellDialect,
    config_dir: &Path,
    name: &str,
    source: UpdateSource<'_>,
) -> String {
    let (target, registry) = match source {
        UpdateSource::Local(dir) => (dir.to_string_lossy().into_owned(), None),
        UpdateSource::Registry { version, registry } => (
            version.map_or_else(|| name.to_string(), |version| format!("{name}@{version}")),
            registry,
        ),
    };
    let dir = config_dir.to_string_lossy();
    let mut values = vec![dir.as_ref(), target.as_str()];
    if let Some(url) = registry {
        values.push(url);
    }
    let (dialect, marker) = dialect.command_form(&values);
    let registry = registry
        .map(|url| format!(" --registry {}", dialect.quote_literal(url)))
        .unwrap_or_default();
    format!(
        "{marker}{} plugin install {}{registry}",
        zeroclaw_invocation_for(dialect, config_dir),
        dialect.quote_literal(&target),
    )
}

/// [`update_command`] rendered for an explicit shell dialect.
///
/// The version, a `provides` id and a publisher key come from the replacement's
/// manifest, which is publisher-controlled text, so every value is one quoted
/// literal argument, and a line no Windows form can pass literally is rendered
/// as the marked PowerShell form, as the egress commands are.
#[must_use]
pub fn update_command_for(
    dialect: ShellDialect,
    config_dir: &Path,
    name: &str,
    source: UpdateSource<'_>,
    increase: &[AuthorityItem],
) -> String {
    let allowed = increase
        .iter()
        .map(AuthorityItem::token)
        .collect::<Vec<_>>()
        .join(",");
    let target = match source {
        UpdateSource::Registry {
            version: Some(version),
            ..
        } => format!("{name}@{version}"),
        UpdateSource::Registry { version: None, .. } | UpdateSource::Local(_) => name.to_string(),
    };
    let origin = match source {
        UpdateSource::Registry {
            registry: Some(url),
            ..
        } => Some(("--registry", url.to_string())),
        UpdateSource::Registry { registry: None, .. } => None,
        UpdateSource::Local(dir) => Some(("--from", dir.to_string_lossy().into_owned())),
    };

    let dir = config_dir.to_string_lossy();
    let mut values = vec![dir.as_ref(), target.as_str(), allowed.as_str()];
    if let Some((_, value)) = &origin {
        values.push(value.as_str());
    }
    let (dialect, marker) = dialect.command_form(&values);
    let origin = origin
        .map(|(flag, value)| format!(" {flag} {}", dialect.quote_literal(&value)))
        .unwrap_or_default();
    let allow = if increase.is_empty() {
        String::new()
    } else {
        format!(" --allow {}", dialect.quote_literal(&allowed))
    };
    format!(
        "{marker}{} plugin update {}{origin}{allow}",
        zeroclaw_invocation_for(dialect, config_dir),
        dialect.quote_literal(&target),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::egress_ceremony::POWERSHELL_ONLY_MARKER;

    fn manifest(extra: &str) -> PluginManifest {
        toml::from_str(&format!(
            "name = \"weather\"\nversion = \"1.0.0\"\nwasm_path = \"plugin.wasm\"\n{extra}"
        ))
        .expect("test manifest must parse")
    }

    #[test]
    fn a_new_permission_capability_or_provides_id_is_an_increase() {
        let installed = manifest("capabilities = [\"tool\"]\npermissions = [\"config_read\"]\n");
        let candidate = manifest(
            "capabilities = [\"tool\", \"channel\"]\n\
             permissions = [\"config_read\", \"http_client\", \"websocket_client\", \"http_client\"]\n\
             provides = \"telegram\"\n",
        );

        let increase = authority_increase(&installed, &candidate);

        assert_eq!(
            increase,
            [
                AuthorityItem::Permission(PluginPermission::HttpClient),
                AuthorityItem::Permission(PluginPermission::WebSocketClient),
                AuthorityItem::Capability(PluginCapability::Channel),
                AuthorityItem::Provides("telegram".to_string()),
            ]
        );
        assert_eq!(
            increase
                .iter()
                .map(AuthorityItem::token)
                .collect::<Vec<_>>(),
            [
                "permission:http_client",
                "permission:websocket_client",
                "capability:channel",
                "provides:telegram",
            ]
        );
    }

    #[test]
    fn narrowing_and_gaining_a_signature_are_not_increases() {
        let installed = manifest(
            "capabilities = [\"tool\", \"channel\"]\npermissions = [\"http_client\"]\nprovides = \"telegram\"\n",
        );
        let candidate =
            manifest("capabilities = [\"tool\"]\npermissions = []\npublisher_key = \"aa\"\n");

        assert!(authority_increase(&installed, &candidate).is_empty());
    }

    #[test]
    fn another_publisher_or_none_where_there_was_one_is_an_increase() {
        let installed = manifest("capabilities = [\"tool\"]\npublisher_key = \"aa\"\n");
        let rekeyed = manifest("capabilities = [\"tool\"]\npublisher_key = \"bb\"\n");
        let unsigned = manifest("capabilities = [\"tool\"]\n");

        assert_eq!(
            authority_increase(&installed, &rekeyed),
            [AuthorityItem::Publisher(Some("bb".to_string()))]
        );
        assert_eq!(
            authority_increase(&installed, &unsigned)
                .iter()
                .map(AuthorityItem::token)
                .collect::<Vec<_>>(),
            ["publisher:unsigned"]
        );
        assert!(authority_increase(&installed, &installed).is_empty());
    }

    #[test]
    fn every_item_must_be_accepted_by_its_exact_token() {
        let increase = [
            AuthorityItem::Permission(PluginPermission::HttpClient),
            AuthorityItem::Capability(PluginCapability::Channel),
        ];
        let allow = |items: &[&str]| {
            items
                .iter()
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
        };

        assert!(accepts_all(
            &increase,
            &allow(&["capability:channel", "permission:http_client"])
        ));
        assert!(!accepts_all(&increase, &allow(&["permission:http_client"])));
        assert!(!accepts_all(
            &increase,
            &allow(&["permission:HttpClient", "capability:channel"])
        ));
        assert!(accepts_all(&[], &[]));
    }

    #[test]
    fn allow_items_need_a_known_kind_and_a_value() {
        let items = |items: &[&str]| {
            items
                .iter()
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            malformed_allow_item(&items(&[
                "permission:http_client",
                "capability:tool",
                "provides:telegram",
                "publisher:unsigned",
            ])),
            None
        );
        for bad in ["http_client", "permission:", "grant:http_client", ":tool"] {
            assert_eq!(
                malformed_allow_item(&items(&["permission:http_client", bad])),
                Some(bad)
            );
        }
    }

    #[test]
    fn the_repeated_command_pins_the_version_and_quotes_manifest_text_literally() {
        let increase = [
            AuthorityItem::Permission(PluginPermission::HttpClient),
            AuthorityItem::Provides("$(id)".to_string()),
        ];
        let command = update_command_for(
            ShellDialect::Posix,
            Path::new("/srv/zeroclaw profile"),
            "weather",
            UpdateSource::Registry {
                version: Some("2.0.0'; rm -rf ~; '"),
                registry: Some("https://registry.example.com/index.json"),
            },
            &increase,
        );

        assert_eq!(
            command,
            "zeroclaw --config-dir '/srv/zeroclaw profile' plugin update \
             'weather@2.0.0'\\''; rm -rf ~; '\\''' \
             --registry 'https://registry.example.com/index.json' \
             --allow 'permission:http_client,provides:$(id)'"
        );
    }

    #[test]
    fn a_local_source_is_named_with_from_and_no_version() {
        let command = update_command_for(
            ShellDialect::Posix,
            Path::new("/cfg"),
            "weather",
            UpdateSource::Local(Path::new("/work/weather-plugin")),
            &[AuthorityItem::Capability(PluginCapability::Channel)],
        );

        assert_eq!(
            command,
            "zeroclaw --config-dir '/cfg' plugin update 'weather' \
             --from '/work/weather-plugin' --allow 'capability:channel'"
        );
    }

    #[test]
    fn a_pointer_without_authority_names_only_the_source() {
        let registry = update_command_for(
            ShellDialect::Posix,
            Path::new("/cfg"),
            "weather",
            UpdateSource::Registry {
                version: None,
                registry: None,
            },
            &[],
        );
        assert_eq!(
            registry,
            "zeroclaw --config-dir '/cfg' plugin update 'weather'"
        );

        let local = update_command_for(
            ShellDialect::Posix,
            Path::new("/cfg"),
            "weather",
            UpdateSource::Local(Path::new("/work/weather-plugin")),
            &[],
        );
        assert_eq!(
            local,
            "zeroclaw --config-dir '/cfg' plugin update 'weather' --from '/work/weather-plugin'"
        );
    }

    #[test]
    fn an_older_version_is_one_lower_by_semver_precedence() {
        assert!(is_older_version("1.2.5", "2.0.0"));
        assert!(is_older_version("2.0.0-rc.1", "2.0.0"));
        assert!(!is_older_version("2.0.0", "1.2.5"));
        assert!(!is_older_version("2.0.0", "2.0.0"));
        assert!(!is_older_version("2.0.0+build.1", "2.0.0+build.2"));
        assert!(!is_older_version("nightly", "2.0.0"));
        assert!(!is_older_version("1.0", "2.0.0"));
    }

    #[test]
    fn control_and_bidi_characters_are_printed_as_escapes() {
        assert_eq!(printable("2.0.0 plugin-é"), "2.0.0 plugin-é");
        assert_eq!(printable("tele\u{1b}[2Kgram"), "tele\\u{1b}[2Kgram");
        assert_eq!(printable("a\nb"), "a\\u{a}b");
        assert_eq!(printable("x\u{202e}y\u{2066}z"), "x\\u{202e}y\\u{2066}z");
    }

    /// Line and paragraph separators break a line as surely as a newline, and
    /// invisible format characters make two different strings look the same.
    #[test]
    fn separators_and_invisible_format_characters_are_printed_as_escapes() {
        assert_eq!(printable("a\u{2028}b\u{2029}c"), "a\\u{2028}b\\u{2029}c");
        assert_eq!(printable("2.0\u{200b}.0"), "2.0\\u{200b}.0");
        assert_eq!(printable("\u{feff}tool"), "\\u{feff}tool");
        assert_eq!(printable("id\u{e0041}"), "id\\u{e0041}");
        assert_eq!(printable("soft\u{ad}hyphen"), "soft\\u{ad}hyphen");
        assert_eq!(printable("日本語 ✓"), "日本語 ✓");
    }

    #[test]
    fn the_install_pointer_names_the_same_source() {
        let local = install_command_for(
            ShellDialect::Posix,
            Path::new("/cfg"),
            "weather",
            UpdateSource::Local(Path::new("/work/weather-plugin")),
        );
        assert_eq!(
            local,
            "zeroclaw --config-dir '/cfg' plugin install '/work/weather-plugin'"
        );

        let pinned = install_command_for(
            ShellDialect::Posix,
            Path::new("/cfg"),
            "weather",
            UpdateSource::Registry {
                version: Some("1.2.0"),
                registry: Some("https://registry.example.com/index.json"),
            },
        );
        assert_eq!(
            pinned,
            "zeroclaw --config-dir '/cfg' plugin install 'weather@1.2.0' --registry 'https://registry.example.com/index.json'"
        );
    }

    #[test]
    fn a_registry_chosen_by_the_environment_is_not_repeated() {
        let command = update_command_for(
            ShellDialect::Posix,
            Path::new("/cfg"),
            "weather",
            UpdateSource::Registry {
                version: Some("2.0.0"),
                registry: None,
            },
            &[AuthorityItem::Permission(PluginPermission::StateRead)],
        );

        assert!(!command.contains("--registry"), "{command}");
        assert!(
            command.ends_with("--allow 'permission:state_read'"),
            "{command}"
        );
    }

    #[test]
    fn a_value_windows_cannot_pass_literally_gets_the_marked_powershell_form() {
        let plain = update_command_for(
            ShellDialect::Windows,
            Path::new(r"C:\zeroclaw"),
            "weather",
            UpdateSource::Registry {
                version: Some("2.0.0"),
                registry: None,
            },
            &[AuthorityItem::Permission(PluginPermission::HttpClient)],
        );
        assert!(!plain.starts_with(POWERSHELL_ONLY_MARKER), "{plain}");
        assert!(plain.contains("\"weather@2.0.0\""), "{plain}");

        let hostile = update_command_for(
            ShellDialect::Windows,
            Path::new(r"C:\zeroclaw"),
            "weather",
            UpdateSource::Registry {
                version: Some("2.0.0"),
                registry: None,
            },
            &[AuthorityItem::Provides("%PATH%".to_string())],
        );
        assert!(hostile.starts_with(POWERSHELL_ONLY_MARKER), "{hostile}");
        assert!(hostile.contains("'provides:%PATH%'"), "{hostile}");
    }
}
