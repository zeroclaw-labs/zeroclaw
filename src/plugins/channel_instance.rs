//! Plugin instance rows, channel bindings included: the pure half.
//!
//! A plugin package's host-owned state, its private `config` map and its
//! egress grant, lives on one `[[plugins.entries]]` row per *instance*, keyed
//! by `PluginInstanceScope::config_entry_key` over `(package, capability,
//! binding)`. The default tool binding's binding is the package name. A
//! channel instance's binding is the alias of the `[channels.plugin.<alias>]`
//! table that names the package, so which channel instances exist is live
//! config, read at use time: this module stores and caches nothing.
//!
//! [`instance_rows`] is the one enumeration of a package's rows. Every key it
//! yields comes from the constructor the runtime's activation plan admits the
//! same binding through, so a key the CLI prints is the key the runtime
//! resolves.
//!
//! The binding ceremony's decisions live here too: [`plan_channel_binding`]
//! decides, without writing anything, whether an alias can be bound and what
//! the binding and its row need, and [`apply_channel_binding`] carries a plan
//! out on a `Config` for the caller's one save.
//!
//! Like `egress_ceremony`, this module owns only decisions; every user-facing
//! string stays in the CLI so it routes through Fluent.

use std::path::Path;

use serde_json::Value;
use zeroclaw_config::schema::{Config, PluginEntryConfig};
use zeroclaw_plugins::error::PluginError;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};
use zeroclaw_runtime::plugin_runtime::PLUGIN_CHANNEL_FAMILY;

use super::egress_ceremony::{
    ShellDialect, canonical_hosts, egress_hosts_path, zeroclaw_invocation_for,
};

/// One `[[plugins.entries]]` row a package's instance owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstanceRow {
    /// The capability world the instance serves: `Tool` for the default tool
    /// binding, `Channel` for a bound alias.
    pub capability: PluginCapability,
    /// The package name for the default tool binding; the configured alias
    /// for a channel binding.
    pub binding: String,
    /// The `zpi1_` config entry key, from
    /// `PluginInstanceScope::config_entry_key`.
    pub key: String,
}

impl PluginInstanceRow {
    /// Whether this row belongs to a `[channels.plugin.<alias>]` binding
    /// rather than to the default tool binding.
    #[must_use]
    pub fn is_channel(&self) -> bool {
        self.capability == PluginCapability::Channel
    }

    /// The name the CLI prints for this instance.
    ///
    /// The default tool binding prints the bare `package`, so tool output is
    /// exactly what it was before channel rows were enumerated. A channel row
    /// prints `package (plugin.<alias>)`, which tells two instances of one
    /// package apart and names the binding the operator edits.
    #[must_use]
    pub fn display_name(&self, package: &str) -> String {
        if self.is_channel() {
            format!("{package} ({PLUGIN_CHANNEL_FAMILY}.{})", self.binding)
        } else {
            package.to_string()
        }
    }

    /// The row names a pre-typed-config install could carry for this
    /// instance, as `egress_ceremony::resolve_grant_state` takes them.
    ///
    /// The default tool binding's legacy row was keyed by the package name. A
    /// channel row has none: channel construction postdates typed config, so
    /// no runtime ever read an alias-named row. Offering the package name here
    /// would point the rename step at the tool binding's legacy row and move
    /// that instance's grant onto this one.
    #[must_use]
    pub fn legacy_candidates(&self, package: &str) -> Vec<String> {
        if self.is_channel() {
            Vec::new()
        } else {
            vec![package.to_string()]
        }
    }
}

/// Whether an instance of `manifest` owns host state, and so is owed a row.
///
/// A row is owed when the instance has host-owned state to hold: a private
/// config object (`config_schema`), a declared egress destination, or a
/// governed transport whose reach the operator must grant. A manifest with
/// none of those owns no state and gets no row. The rule is the same for every
/// instance of the package, tool or channel.
///
/// The transport arm is deliberate. The second grant path is the plugin whose
/// destination *is* deployment configuration, a self-hosted Gitea or a LAN
/// Nextcloud, which its author cannot declare, so it ships a transport with no
/// `[egress]` table and often no `config_schema`. Without a row there is
/// nowhere to author that grant: `config set plugins.entries.<key>.egress_hosts`
/// only resolves keys already present in live config, and `plugin info` would
/// not even print the opaque key to address. Pinned by
/// `a_network_permission_alone_earns_a_row_so_the_operator_can_grant_reach`.
#[must_use]
pub fn manifest_owns_instance_state(manifest: &PluginManifest) -> bool {
    manifest.config_schema.is_some()
        || !manifest.egress.hosts.is_empty()
        || manifest_has_governed_transport(manifest)
}

/// Whether `manifest` requests a transport the plugin egress authority
/// governs: `http_client`, `websocket_client` or `socket_client`, the
/// permissions `zeroclaw_plugins::egress` requires for HTTP, for WebSocket, and
/// for TCP, TLS or STARTTLS reach. Each reaches only the destinations its
/// instance row grants.
#[must_use]
pub fn manifest_has_governed_transport(manifest: &PluginManifest) -> bool {
    manifest.permissions.iter().any(|permission| {
        matches!(
            permission,
            PluginPermission::HttpClient
                | PluginPermission::WebSocketClient
                | PluginPermission::SocketClient
        )
    })
}

/// The config entry key of the channel instance that the binding `alias`
/// makes of `manifest`'s package.
///
/// Derived with `PluginInstanceScope::from_manifest(manifest, Channel, alias,
/// [])`, the constructor the activation plan admits a
/// `[channels.plugin.<alias>]` binding through. The key covers `(package,
/// capability, binding)` and never the grant set, so the empty grant set here
/// derives the key of the scope the runtime builds with the manifest's
/// permissions.
///
/// # Errors
///
/// The constructor's refusal: a manifest that does not declare `channel`, or
/// an alias the instance identity rules reject.
pub fn channel_instance_key(manifest: &PluginManifest, alias: &str) -> Result<String, PluginError> {
    PluginInstanceScope::from_manifest(
        manifest,
        PluginCapability::Channel,
        alias,
        std::iter::empty(),
    )?
    .id()
    .config_entry_key()
}

/// The aliases of every `[channels.plugin.<alias>]` binding whose `package` is
/// `package`, sorted.
///
/// Disabled bindings are included. `enabled = false` keeps an instance from
/// starting; it does not end the instance, and its row keeps the instance's
/// config and grant for when it is enabled again.
#[must_use]
pub fn bound_channel_aliases(config: &Config, package: &str) -> Vec<String> {
    let mut aliases: Vec<String> = config
        .channels
        .plugin
        .iter()
        .filter(|(_, binding)| binding.package == package)
        .map(|(alias, _)| alias.clone())
        .collect();
    aliases.sort_unstable();
    aliases
}

/// Every `[[plugins.entries]]` row `manifest`'s package owns: the default tool
/// binding first, then one row per bound channel alias, sorted by alias.
///
/// Both kinds follow [`manifest_owns_instance_state`]. The tool row needs the
/// `tool` capability. A channel row needs the `channel` capability and a
/// `[channels.plugin.<alias>]` binding that names the package: without one a
/// channel package has no instance, so it yields no row rather than a
/// package-level key nothing reads.
///
/// A bound alias no instance can be keyed by, one the instance identity rules
/// reject, yields no row. Config validation only warns about such an alias,
/// so it can reach live config, and it must not take the package's other
/// rows down with it: [`unkeyed_channel_aliases`] reports it instead.
///
/// # Errors
///
/// A key derivation failure for the default tool binding.
pub fn instance_rows(
    config: &Config,
    manifest: &PluginManifest,
) -> Result<Vec<PluginInstanceRow>, PluginError> {
    if !manifest_owns_instance_state(manifest) {
        return Ok(Vec::new());
    }

    let mut rows = Vec::new();
    if manifest.capabilities.contains(&PluginCapability::Tool) {
        let scope = PluginInstanceScope::for_package_binding(
            manifest,
            PluginCapability::Tool,
            std::iter::empty(),
        )?;
        rows.push(PluginInstanceRow {
            capability: PluginCapability::Tool,
            binding: scope.id().binding().to_string(),
            key: scope.id().config_entry_key()?,
        });
    }
    if manifest.capabilities.contains(&PluginCapability::Channel) {
        for alias in bound_channel_aliases(config, &manifest.name) {
            if let Ok(row) = channel_instance_row(manifest, &alias) {
                rows.extend(row);
            }
        }
    }
    Ok(rows)
}

/// The aliases bound to `manifest`'s package that no instance can be keyed
/// by, each with the instance identity rules' reason: the aliases
/// [`instance_rows`] skips. Sorted by alias; empty for a package without the
/// `channel` capability.
#[must_use]
pub fn unkeyed_channel_aliases(
    config: &Config,
    manifest: &PluginManifest,
) -> Vec<(String, String)> {
    if !manifest.capabilities.contains(&PluginCapability::Channel) {
        return Vec::new();
    }
    bound_channel_aliases(config, &manifest.name)
        .into_iter()
        .filter_map(|alias| {
            channel_instance_key(manifest, &alias)
                .err()
                .map(|error| (alias, error.to_string()))
        })
        .collect()
}

/// The row the channel instance `alias` makes of `manifest`'s package owns,
/// or `None` when such an instance owns no host state
/// ([`manifest_owns_instance_state`]).
///
/// This does not ask whether a binding exists: [`instance_rows`] enumerates
/// bound aliases, and the binding ceremony plans the row of an alias it is
/// about to bind.
///
/// # Errors
///
/// A key derivation failure, as [`channel_instance_key`] reports it.
pub fn channel_instance_row(
    manifest: &PluginManifest,
    alias: &str,
) -> Result<Option<PluginInstanceRow>, PluginError> {
    if !manifest_owns_instance_state(manifest) {
        return Ok(None);
    }
    Ok(Some(PluginInstanceRow {
        capability: PluginCapability::Channel,
        binding: alias.to_string(),
        key: channel_instance_key(manifest, alias)?,
    }))
}

/// Whether `row`'s instance holds a transport that can reach a declared
/// destination.
///
/// A declaration counts only with a transport that can use it. A row persists
/// across `plugin remove`, so a grant seeded for a version that could not
/// reach the network would silently become live reach when a later version of
/// the same package adds a transport. That later install meets an existing
/// row, which is never extended, so the operator grants it deliberately.
///
/// The two kinds of row count different transports, on purpose. A channel row
/// counts any governed transport ([`manifest_has_governed_transport`]): a
/// channel that speaks only over a socket, such as IRC, email or MQTT, or only
/// over a WebSocket relay would otherwise never have its declaration seeded or
/// its gap reported. The default tool binding keeps the rule the tool ceremony
/// shipped with, `http_client` only, which predates the WebSocket and socket
/// transports. Widening it changes what `plugin install` grants and what
/// `plugin list` reports for tool packages already installed, so it is left to
/// a change of its own. What `plugin install` and `plugin bind` seed and what
/// `plugin list` reports all come from [`declared_hosts_for_row`], so each
/// follows this rule and no other.
#[must_use]
pub fn row_has_usable_transport(manifest: &PluginManifest, row: &PluginInstanceRow) -> bool {
    if row.is_channel() {
        manifest_has_governed_transport(manifest)
    } else {
        manifest.permissions.contains(&PluginPermission::HttpClient)
    }
}

/// The declared destinations `row`'s instance can use: the manifest's
/// `[egress]` hosts when [`row_has_usable_transport`] holds, otherwise none.
/// Empty as well when the manifest declares nothing.
///
/// This is the declaration, never a grant: nothing here confers network reach.
#[must_use]
pub fn declared_hosts_for_row(manifest: &PluginManifest, row: &PluginInstanceRow) -> Vec<String> {
    if row_has_usable_transport(manifest, row) {
        manifest.egress.hosts.clone()
    } else {
        Vec::new()
    }
}

/// The operator's decision on the destinations a channel instance's manifest
/// declares.
///
/// It takes effect only when the binding ceremony creates the instance's row,
/// and the grant it writes there is the record of the decision. A row that
/// already exists is never extended, whatever the decision says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressDecision {
    /// Grant the declared destinations, in canonical form, on the new row.
    SeedDeclared,
    /// Create the row with an empty grant: no network reach until the operator
    /// grants some.
    Withhold,
}

/// Why the binding ceremony refuses an alias. Every refusal is decided before
/// anything is written, so a refused ceremony leaves config as it found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingRefusal {
    /// The manifest does not declare the `channel` capability.
    NotAChannelPackage,
    /// The alias fails the config alias grammar. Carries the grammar's own
    /// message.
    InvalidAlias(String),
    /// The loader dropped a section the ceremony would write into, or the
    /// whole config, because it is malformed on disk. Carries the section as
    /// the loader recorded it. Writing now would put the salvaged defaults
    /// over what the operator wrote.
    DegradedConfig { section: String },
    /// The alias is bound to another package. The ceremony never rewrites a
    /// binding's `package`.
    AliasOwnedByOtherPackage { owner: String },
    /// The row would be created and its manifest declares destinations the
    /// row can use, but no decision was given. Carries that declaration in
    /// canonical form, the list `--egress declared` would grant.
    EgressDecisionRequired { declared: Vec<String> },
}

/// What the ceremony does to the `[[plugins.entries]]` row of the instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowPlan {
    /// The instance's `zpi1_` config entry key.
    pub key: String,
    /// Whether the ceremony creates the row. `false` when it already exists,
    /// and then nothing about the row is written.
    pub create: bool,
    /// The canonical hosts to grant on the new row. Non-empty only when the
    /// row is created and the decision is [`EgressDecision::SeedDeclared`].
    pub seed: Vec<String>,
    /// The destinations the manifest declares for this row, as
    /// [`declared_hosts_for_row`] gives them.
    pub declared: Vec<String>,
    /// An egress decision was given, but the row exists, so it was not
    /// applied.
    pub decision_ignored: bool,
}

/// What binding `alias` to a package writes, decided from live config and the
/// package's manifest by [`plan_channel_binding`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelBindingPlan {
    /// The alias being bound: the instance is routed as `plugin.<alias>`.
    pub alias: String,
    /// Whether the ceremony creates `[channels.plugin.<alias>]`. `false` when
    /// the binding already names this package, and then it is left exactly
    /// as it is, `enabled = false` included.
    pub create_binding: bool,
    /// The instance's row, or `None` when the instance owns no host state.
    pub row: Option<RowPlan>,
}

impl ChannelBindingPlan {
    /// Whether applying the plan writes anything. A plan that finds the
    /// binding and its row already in place writes nothing, so a repeated
    /// ceremony leaves the config file byte-identical.
    #[must_use]
    pub fn writes_config(&self) -> bool {
        self.create_binding || self.row.as_ref().is_some_and(|row| row.create)
    }
}

/// Decide whether `alias` can be bound to `manifest`'s package, and what the
/// binding and the instance's row need. Pure: nothing is written.
///
/// The checks run in this order and stop at the first refusal:
///
/// 1. The manifest declares the `channel` capability.
/// 2. The alias passes the config alias grammar.
/// 3. The loader dropped none of the sections the ceremony writes into: the
///    whole config, `plugins`, `channels`, `channels.plugin`, or the alias's
///    own table. The ceremony never writes over a section the loader could
///    not read.
/// 4. An absent binding is created; one that names this package is kept as
///    it is; one that names another package is refused.
/// 5. An instance that owns no host state gets no row. An existing row is left
///    untouched, and a decision given for it is reported as not applied. An
///    absent row is created: when the manifest declares destinations the row
///    can use, `egress` decides whether the new row grants them, and without
///    a decision the ceremony refuses; with nothing declared the row starts
///    with an empty grant and no decision is needed.
///
/// # Errors
///
/// The first [`BindingRefusal`] the checks reach.
pub fn plan_channel_binding(
    config: &Config,
    manifest: &PluginManifest,
    alias: &str,
    egress: Option<EgressDecision>,
) -> Result<ChannelBindingPlan, BindingRefusal> {
    if !manifest.capabilities.contains(&PluginCapability::Channel) {
        return Err(BindingRefusal::NotAChannelPackage);
    }
    zeroclaw_config::helpers::validate_alias_key(alias).map_err(BindingRefusal::InvalidAlias)?;
    if let Some(section) = degraded_binding_section(config, alias) {
        return Err(BindingRefusal::DegradedConfig { section });
    }

    let create_binding = match config.channels.plugin.get(alias) {
        None => true,
        Some(binding) if binding.package == manifest.name => false,
        Some(binding) => {
            return Err(BindingRefusal::AliasOwnedByOtherPackage {
                owner: binding.package.clone(),
            });
        }
    };

    // The alias grammar is stricter than the instance identity rules and the
    // package is one the host admitted, so this derivation does not fail in
    // practice. If the identity rules ever reject the binding, the refusal
    // is the alias's, with the constructor's own message.
    let row = channel_instance_row(manifest, alias)
        .map_err(|error| BindingRefusal::InvalidAlias(error.to_string()))?
        .map(|row| plan_row(config, manifest, &row, egress))
        .transpose()?;

    Ok(ChannelBindingPlan {
        alias: alias.to_string(),
        create_binding,
        row,
    })
}

/// Step 5 of [`plan_channel_binding`] for an instance that owns a row.
fn plan_row(
    config: &Config,
    manifest: &PluginManifest,
    row: &PluginInstanceRow,
    egress: Option<EgressDecision>,
) -> Result<RowPlan, BindingRefusal> {
    let declared = declared_hosts_for_row(manifest, row);
    if config
        .plugins
        .entries
        .iter()
        .any(|entry| entry.name == row.key)
    {
        return Ok(RowPlan {
            key: row.key.clone(),
            create: false,
            seed: Vec::new(),
            declared,
            decision_ignored: egress.is_some(),
        });
    }
    let seed = if declared.is_empty() {
        Vec::new()
    } else {
        match egress {
            Some(EgressDecision::SeedDeclared) => canonical_hosts(&declared),
            Some(EgressDecision::Withhold) => Vec::new(),
            None => {
                return Err(BindingRefusal::EgressDecisionRequired {
                    declared: canonical_hosts(&declared),
                });
            }
        }
    };
    Ok(RowPlan {
        key: row.key.clone(),
        create: true,
        seed,
        declared,
        decision_ignored: false,
    })
}

/// The section the loader dropped, if any, that binding `alias` would write
/// into, as `degraded_security` or `degraded_sections` records it.
fn degraded_binding_section(config: &Config, alias: &str) -> Option<String> {
    let own_table = format!("channels.plugin.{alias}");
    let written = [
        zeroclaw_config::migration::WHOLE_CONFIG_SENTINEL,
        "plugins",
        "channels",
        "channels.plugin",
        own_table.as_str(),
    ];
    config
        .degraded_security
        .iter()
        .chain(&config.degraded_sections)
        .find(|section| written.contains(&section.as_str()))
        .cloned()
}

/// Carry out `plan` on `config` for `package`, marking every path it writes
/// dirty. Never saves: the caller owns the one `save_dirty`, which is what
/// lets `plugin install` persist a package's tool row and its channel
/// instance together.
///
/// A created binding is written with `package` and the schema default
/// `enabled = true`: the ceremony sets nothing else. A created row gets the
/// plan's seed as its `egress_hosts`, the same dirty-path write install
/// seeding uses.
/// `egress_allow_private` is never written: a private-address carve-out stays
/// operator-authored.
///
/// # Errors
///
/// A config write error, and a binding or row the plan found absent that
/// exists by now. Writing into either would take over another package's alias
/// or extend an existing grant, so the plan must be made again.
pub fn apply_channel_binding(
    config: &mut Config,
    package: &str,
    plan: &ChannelBindingPlan,
) -> anyhow::Result<()> {
    if plan.create_binding {
        let created = config
            .create_map_key("channels.plugin", &plan.alias)
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            created,
            "the channel binding plugin.{} appeared after it was planned",
            plan.alias
        );
        config.set_prop(&format!("channels.plugin.{}.package", plan.alias), package)?;
        config.mark_dirty(&format!("channels.plugin.{}", plan.alias));
    }
    if let Some(row) = plan.row.as_ref().filter(|row| row.create) {
        let created = config
            .create_map_key("plugins.entries", &row.key)
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            created,
            "the config entry '{}' appeared after it was planned",
            row.key
        );
        config.mark_dirty(&format!("plugins.entries.{}", row.key));
        if !row.seed.is_empty() {
            config.set_prop(&egress_hosts_path(&row.key), &row.seed.join(","))?;
        }
    }
    Ok(())
}

/// One entry of a manifest's `config_schema.required` list, as the readiness
/// report shows it. It carries a name and three facts, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredKey {
    /// The property name exactly as the manifest spells it: publisher text.
    pub name: String,
    /// The property is marked `x-secret: true`.
    pub secret: bool,
    /// The instance row's config map holds a value for it.
    pub set: bool,
    /// The name follows the portable plugin key grammar, so a printed
    /// `config set plugins.entries.<key>.config.<name>` can address it.
    /// Admission holds only secret names to that grammar.
    pub addressable: bool,
}

/// The keys `manifest`'s `config_schema` requires, in schema order, each
/// marked set or missing against `entry`, the instance's `[[plugins.entries]]`
/// row (`None` when it has none). A manifest without a schema requires
/// nothing.
#[must_use]
pub fn required_keys(
    manifest: &PluginManifest,
    entry: Option<&PluginEntryConfig>,
) -> Vec<RequiredKey> {
    let Some(schema) = manifest.config_schema.as_ref() else {
        return Vec::new();
    };
    let properties = schema.get("properties").and_then(Value::as_object);
    schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|name| RequiredKey {
            name: name.to_string(),
            secret: properties
                .and_then(|properties| properties.get(name))
                .and_then(|property| property.get("x-secret"))
                == Some(&Value::Bool(true)),
            set: entry.is_some_and(|entry| entry.config.contains_key(name)),
            addressable: zeroclaw_api::plugin_key::is_valid_portable_plugin_key(name),
        })
        .collect()
}

/// The runtime's own verdict on the configuration of `alias`'s instance.
///
/// Runs `zeroclaw_plugins::config::resolve_plugin_config` over the config map
/// of `entry`, the instance's `[[plugins.entries]]` row (`None` when it has
/// none), with the scope the activation plan builds for the binding: the
/// channel capability, the alias, and the manifest's permissions as the grant
/// set. What it accepts is what the instance receives when it starts.
///
/// # Errors
///
/// The resolver's message. It names schema paths and property names, never a
/// value. Control characters in it are escaped, because a property name is
/// publisher text and the message is printed to the operator's terminal.
pub fn config_verdict(
    manifest: &PluginManifest,
    alias: &str,
    entry: Option<&PluginEntryConfig>,
) -> Result<(), String> {
    PluginInstanceScope::from_manifest(
        manifest,
        PluginCapability::Channel,
        alias,
        manifest.permissions.iter().copied(),
    )
    .and_then(|scope| {
        zeroclaw_plugins::config::resolve_plugin_config(
            manifest,
            &scope,
            entry.map(|entry| &entry.config),
        )
    })
    .map(drop)
    .map_err(|error| escape_control_characters(&error.to_string()))
}

/// `text` with every control character written as its escape sequence.
fn escape_control_characters(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_control() {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

/// `zeroclaw --config-dir '<dir>' config set '<path>' ['<value>']`, rendered
/// for `dialect`: the command that sets one property of the configuration
/// the operator inspected.
///
/// Every argument is quoted literally, as the egress commands quote theirs:
/// a path can carry publisher text (a schema property name) or config text
/// (an alias). Without a value `config set` prompts for one, masked for a
/// secret field, so a secret never has to be typed on the command line.
#[must_use]
pub fn config_set_command_for(
    dialect: ShellDialect,
    config_dir: &Path,
    path: &str,
    value: Option<&str>,
) -> String {
    let dir = config_dir.to_string_lossy();
    let mut arguments = vec![dir.as_ref(), path];
    arguments.extend(value);
    let (dialect, marker) = dialect.command_form(&arguments);
    let mut command = format!(
        "{marker}{} config set {}",
        zeroclaw_invocation_for(dialect, config_dir),
        dialect.quote_literal(path)
    );
    if let Some(value) = value {
        command.push(' ');
        command.push_str(&dialect.quote_literal(value));
    }
    command
}

/// `zeroclaw --config-dir '<dir>' plugin bind '<package>' --channel-alias
/// <alias>`, rendered for `dialect`: the binding ceremony for `package`.
///
/// With `alias` the command binds that alias, quoted like every other
/// argument. Without one it ends in the literal placeholder `<alias>`, for the
/// operator to replace with the alias they choose.
///
/// With `egress_choice` the command also ends in `--egress <declared|none>`,
/// a placeholder the operator replaces with their decision. The ceremony
/// refuses to create a row whose manifest declares destinations it can use
/// without that decision, so a command printed for such a row must ask for it
/// rather than be refused as printed, and must not make the choice itself.
#[must_use]
pub fn plugin_bind_command_for(
    dialect: ShellDialect,
    config_dir: &Path,
    package: &str,
    alias: Option<&str>,
    egress_choice: bool,
) -> String {
    let dir = config_dir.to_string_lossy();
    let mut arguments = vec![dir.as_ref(), package];
    arguments.extend(alias);
    let (dialect, marker) = dialect.command_form(&arguments);
    let mut command = format!(
        "{marker}{} plugin bind {} --channel-alias {}",
        zeroclaw_invocation_for(dialect, config_dir),
        dialect.quote_literal(package),
        alias.map_or_else(
            || "<alias>".to_string(),
            |alias| dialect.quote_literal(alias)
        )
    );
    if egress_choice {
        command.push_str(" --egress <declared|none>");
    }
    command
}

#[cfg(test)]
mod tests {
    use super::{ShellDialect, config_set_command_for, plugin_bind_command_for};
    use std::path::Path;

    /// The marker a Windows line starts with when a value in it is not
    /// literal in `cmd.exe`, spelled out so the forms below are exact.
    const POWERSHELL_ONLY: &str = "# PowerShell only, cmd.exe cannot pass this value literally: ";

    /// POSIX: every argument single-quoted, an embedded quote closed, escaped
    /// and reopened, and a missing value left for `config set` to prompt for.
    #[test]
    fn config_set_commands_quote_every_argument_in_the_posix_form() {
        let dir = Path::new("/tmp/it's here/profile a");
        assert_eq!(
            config_set_command_for(ShellDialect::Posix, dir, "plugins.enabled", Some("true")),
            "zeroclaw --config-dir '/tmp/it'\\''s here/profile a' config set \
             'plugins.enabled' 'true'"
        );
        assert_eq!(
            config_set_command_for(
                ShellDialect::Posix,
                dir,
                "plugins.entries.zpi1_k.config.api_token",
                None
            ),
            "zeroclaw --config-dir '/tmp/it'\\''s here/profile a' config set \
             'plugins.entries.zpi1_k.config.api_token'"
        );
    }

    /// Windows: one double-quoted argument each, literal in both Windows
    /// shells; a profile path neither passes literally sends the whole line to
    /// the marked PowerShell form.
    #[test]
    fn config_set_commands_take_the_windows_form_or_the_marked_powershell_form() {
        assert_eq!(
            config_set_command_for(
                ShellDialect::Windows,
                Path::new(r"C:\Users\op erator\.zeroclaw"),
                "channels.plugin.operations.enabled",
                Some("true")
            ),
            concat!(
                r#"zeroclaw --config-dir "C:\Users\op erator\.zeroclaw" config set "#,
                r#""channels.plugin.operations.enabled" "true""#
            )
        );
        assert_eq!(
            config_set_command_for(
                ShellDialect::Windows,
                Path::new(r"C:\%USERPROFILE%\.zeroclaw"),
                "plugins.enabled",
                Some("true")
            ),
            format!(
                "{POWERSHELL_ONLY}{}",
                r"zeroclaw --config-dir 'C:\%USERPROFILE%\.zeroclaw' config set 'plugins.enabled' 'true'"
            )
        );
    }

    /// POSIX: the package and a chosen alias are quoted; the alias the
    /// operator has yet to choose and the egress decision they have yet to
    /// make stay bare placeholders.
    #[test]
    fn plugin_bind_commands_leave_the_operators_choices_as_placeholders_in_the_posix_form() {
        let dir = Path::new("/srv/zeroclaw/profile a");
        assert_eq!(
            plugin_bind_command_for(ShellDialect::Posix, dir, "chat-bridge", None, false),
            "zeroclaw --config-dir '/srv/zeroclaw/profile a' plugin bind 'chat-bridge' \
             --channel-alias <alias>"
        );
        assert_eq!(
            plugin_bind_command_for(ShellDialect::Posix, dir, "chat-bridge", None, true),
            "zeroclaw --config-dir '/srv/zeroclaw/profile a' plugin bind 'chat-bridge' \
             --channel-alias <alias> --egress <declared|none>"
        );
        assert_eq!(
            plugin_bind_command_for(
                ShellDialect::Posix,
                dir,
                "chat-bridge",
                Some("operations"),
                true
            ),
            "zeroclaw --config-dir '/srv/zeroclaw/profile a' plugin bind 'chat-bridge' \
             --channel-alias 'operations' --egress <declared|none>"
        );
    }

    /// Windows: the same command with double-quoted arguments, or the marked
    /// PowerShell form when the profile path is not literal in `cmd.exe`.
    #[test]
    fn plugin_bind_commands_take_the_windows_form_or_the_marked_powershell_form() {
        assert_eq!(
            plugin_bind_command_for(
                ShellDialect::Windows,
                Path::new(r"C:\Users\op erator\.zeroclaw"),
                "chat-bridge",
                Some("operations"),
                false
            ),
            concat!(
                r#"zeroclaw --config-dir "C:\Users\op erator\.zeroclaw" plugin bind "#,
                r#""chat-bridge" --channel-alias "operations""#
            )
        );
        assert_eq!(
            plugin_bind_command_for(
                ShellDialect::Windows,
                Path::new(r"C:\Users\op erator\.zeroclaw"),
                "chat-bridge",
                None,
                true
            ),
            concat!(
                r#"zeroclaw --config-dir "C:\Users\op erator\.zeroclaw" plugin bind "#,
                r#""chat-bridge" --channel-alias <alias> --egress <declared|none>"#
            )
        );
        assert_eq!(
            plugin_bind_command_for(
                ShellDialect::Windows,
                Path::new(r"C:\%USERPROFILE%\.zeroclaw"),
                "chat-bridge",
                Some("operations"),
                true
            ),
            format!(
                "{POWERSHELL_ONLY}{}",
                r"zeroclaw --config-dir 'C:\%USERPROFILE%\.zeroclaw' plugin bind 'chat-bridge' --channel-alias 'operations' --egress <declared|none>"
            )
        );
    }
}
