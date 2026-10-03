//! The Plugins step of `zeroclaw quickstart`: tool plugins picked from the
//! registry, installed and configured when the agent is created, then
//! activated with the operator's consent.
//!
//! Picking downloads no package and writes no config. On Create the agent step
//! is dry-run first, so a submission it would refuse stops Create before any
//! config is changed or any plugin installed. The dry run can still create
//! the agent's workspace directory when the submission carries personality
//! files, as the agent step's own validation does. Then each selected package
//! goes through the pipeline `zeroclaw plugin install` uses (registry
//! download, admission, the load check, then the one publish-and-seed
//! transaction), with three differences
//! an unattended install does not have: the registry entry must carry an
//! archive digest, the operator decides whether the destinations the manifest
//! declares are granted, and the instance's own settings are prompted for and
//! validated before anything is written.
//!
//! Nothing is stored beyond what those steps already own: the plugins
//! directory, the `[[plugins.entries]]` row, and the two activation flags. The
//! selection lives in the Quickstart form until Create and is never persisted.

mod model;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use zeroclaw::plugins::host::PluginHost;
use zeroclaw::plugins::instance::PluginInstanceScope;
use zeroclaw::plugins::{PluginCapability, PluginManifest};
use zeroclaw_config::presets::BuilderSubmission;
use zeroclaw_runtime::plugin_runtime::ToolInstanceAdmission;
use zeroclaw_runtime::quickstart::{FieldDescriptor, QuickstartError, Surface};

use crate::config::schema::Config;
use crate::plugin_registry::{RegistryClient, RegistryTimeouts};
use crate::plugins::egress_ceremony::{
    EgressDecision, ShellDialect, canonical_hosts, egress_create_command, egress_set_command,
    zeroclaw_invocation_for,
};
use crate::qta;
use model::{
    ActivatedInstance, ActivationInputs, ActivationPreview, ChannelBinding, ChoiceState,
    ConfigField, FailureStage, PackageOutcome, PluginChoice, Refusal, ValueKind,
    activation_preview, capability_names, config_fields, encode_value, field_descriptor,
    missing_required_settings, nonportable_required_settings, permission_names, plugin_choices,
    terminal_safe, terminal_safe_detail, undeclared_required_settings,
};

/// Why a prompt produced no answer.
#[derive(Debug)]
pub(crate) enum PromptError {
    /// Ctrl+C. Quickstart reports what it already installed and exits.
    Interrupted,
    /// Any other terminal failure.
    Failed(anyhow::Error),
}

type PromptResult<T> = Result<T, PromptError>;

/// Every question the Plugins step asks, and every line it prints.
///
/// Production answers come from the terminal through dialoguer and the shared
/// Quickstart field prompt; tests script them. Each question returns `None`
/// when the operator backs out (Esc), which is not an interruption.
pub(crate) trait QuickstartPrompter {
    /// Pick one of `items`.
    fn select(
        &mut self,
        prompt: &str,
        items: &[String],
        default: usize,
    ) -> PromptResult<Option<usize>>;
    /// Toggle any of `items`, starting from `checked`.
    fn multi_select(
        &mut self,
        prompt: &str,
        items: &[String],
        checked: &[bool],
    ) -> PromptResult<Option<Vec<usize>>>;
    /// Yes or no.
    fn confirm(&mut self, prompt: &str, default: bool) -> PromptResult<Option<bool>>;
    /// One configuration value, through the prompt every Quickstart field uses.
    fn field(&mut self, field: &FieldDescriptor) -> PromptResult<Option<String>>;
    /// Print one line.
    fn say(&mut self, line: &str);
}

/// The terminal: dialoguer prompts on stderr, lines on stdout.
struct TerminalPrompter;

impl QuickstartPrompter for TerminalPrompter {
    fn select(
        &mut self,
        prompt: &str,
        items: &[String],
        default: usize,
    ) -> PromptResult<Option<usize>> {
        dialoguer::Select::new()
            .with_prompt(prompt)
            .items(items)
            .default(default)
            .interact_opt()
            .map_err(dialoguer_error)
    }

    fn multi_select(
        &mut self,
        prompt: &str,
        items: &[String],
        checked: &[bool],
    ) -> PromptResult<Option<Vec<usize>>> {
        dialoguer::MultiSelect::new()
            .with_prompt(prompt)
            .items_checked(items.iter().zip(checked.iter().copied()))
            .interact_opt()
            .map_err(dialoguer_error)
    }

    fn confirm(&mut self, prompt: &str, default: bool) -> PromptResult<Option<bool>> {
        dialoguer::Confirm::new()
            .with_prompt(prompt)
            .default(default)
            .interact_opt()
            .map_err(dialoguer_error)
    }

    fn field(&mut self, field: &FieldDescriptor) -> PromptResult<Option<String>> {
        // `prompt_for_field` already maps Ctrl+C in a text or secret input to
        // `None`; only its choice list can still surface the interruption.
        crate::prompt_for_field(field, None).map_err(|error| {
            if is_interrupted(&error) {
                PromptError::Interrupted
            } else {
                PromptError::Failed(error)
            }
        })
    }

    fn say(&mut self, line: &str) {
        println!("{line}");
    }
}

fn dialoguer_error(error: dialoguer::Error) -> PromptError {
    let io = std::io::Error::from(error);
    if io.kind() == std::io::ErrorKind::Interrupted {
        PromptError::Interrupted
    } else {
        PromptError::Failed(io.into())
    }
}

fn is_interrupted(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::Interrupted)
            || cause.downcast_ref::<dialoguer::Error>().is_some_and(
                |prompt| matches!(prompt, dialoguer::Error::IO(io) if io.kind() == std::io::ErrorKind::Interrupted),
            )
    })
}

fn indented(line: &str) -> String {
    format!("  {line}")
}

/// The Plugins checklist row: whether it was opened, and what was picked.
///
/// Optional, like Channels: Create never waits for it.
#[derive(Default)]
pub(crate) struct PluginsRow {
    visited: bool,
    selected: Vec<PluginChoice>,
}

impl PluginsRow {
    /// Whether the operator opened the row and left it with a choice.
    pub(crate) fn visited(&self) -> bool {
        self.visited
    }

    /// The checklist summary: not yet visited, none, or the picked names.
    pub(crate) fn summary(&self) -> String {
        if !self.visited {
            return qta("cli-quickstart-summary-not-yet-visited", &[]);
        }
        if self.selected.is_empty() {
            return qta("cli-quickstart-plugins-summary-none", &[]);
        }
        self.selected
            .iter()
            .map(|choice| {
                let name = terminal_safe(&choice.name);
                if choice.is_installed() {
                    qta(
                        "cli-quickstart-plugins-summary-installed",
                        &[("name", &name)],
                    )
                } else {
                    name
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// How opening the Plugins row ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowExit {
    /// Back to the checklist.
    Done,
    /// Ctrl+C. Nothing was written; the caller exits as the checklist does.
    Interrupted,
}

/// Open the Plugins row: fetch the registry, list the tool packages, and let
/// the operator pick. Downloads no package and writes no config; only the
/// registry index cache is refreshed, as `plugin search` refreshes it. When
/// the `[plugins]` section of the config file could not be read, the row says
/// so before the registry is contacted and offers nothing to pick.
pub(crate) async fn open_row(config: &Config, row: &mut PluginsRow) -> anyhow::Result<RowExit> {
    let registry = RegistryClient::new(RegistryTimeouts::default())?;
    let registry_url = crate::plugin_registry::registry_url(None);
    Box::pin(open_row_with(
        config,
        row,
        &registry,
        &registry_url,
        &mut TerminalPrompter,
    ))
    .await
}

async fn open_row_with<P: QuickstartPrompter>(
    config: &Config,
    row: &mut PluginsRow,
    registry: &RegistryClient,
    registry_url: &str,
    prompter: &mut P,
) -> anyhow::Result<RowExit> {
    if let Some(line) = unreadable_plugins_section_line(config) {
        prompter.say(&line);
        row.visited = true;
        row.selected.clear();
        return Ok(RowExit::Done);
    }
    let index = loop {
        match registry.fetch_index(registry_url).await {
            Ok(index) => break index,
            Err(error) => {
                prompter.say(&registry_unavailable_line(&error));
                let options = [
                    qta("cli-quickstart-plugins-registry-retry", &[]),
                    qta("cli-quickstart-plugins-registry-continue", &[]),
                ];
                match prompter.select(
                    &qta("cli-quickstart-plugins-registry-prompt", &[]),
                    &options,
                    0,
                ) {
                    Ok(Some(0)) => {}
                    Ok(Some(_)) => {
                        row.visited = true;
                        row.selected.clear();
                        return Ok(RowExit::Done);
                    }
                    Ok(None) => return Ok(RowExit::Done),
                    Err(PromptError::Interrupted) => return Ok(RowExit::Interrupted),
                    Err(PromptError::Failed(error)) => return Err(error),
                }
            }
        }
    };
    if let Err(error) = zeroclaw::plugins::registry::write_cached_registry_index(
        &config.data_dir,
        registry_url,
        &index,
    ) {
        // The cache only speeds up later listings; a failure to refresh it
        // must not block the choice.
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "quickstart.surface": "cli",
                    "error": format!("{error:#}"),
                })),
            "quickstart plugins: could not refresh the registry index cache"
        );
    }

    let host = match crate::plugin_host_with_configured_security(config) {
        Ok(host) => host,
        Err(error) => {
            prompter.say(&qta(
                "cli-quickstart-plugins-row-failed",
                &[("error", &terminal_safe_detail(&format!("{error:#}")))],
            ));
            return Ok(RowExit::Done);
        }
    };
    let installed = host.list_plugins();
    let catalog = zeroclaw::plugins::catalog::package_catalog(&installed, Some(&index));
    let choices = plugin_choices(&catalog);
    if choices.is_empty() {
        prompter.say(&qta("cli-quickstart-plugins-none-available", &[]));
        row.visited = true;
        row.selected.clear();
        return Ok(RowExit::Done);
    }

    let labels: Vec<String> = choices.iter().map(choice_label).collect();
    let checked: Vec<bool> = choices
        .iter()
        .map(|choice| row.selected.iter().any(|kept| kept.name == choice.name))
        .collect();
    match prompter.multi_select(
        &qta("cli-quickstart-plugins-select-prompt", &[]),
        &labels,
        &checked,
    ) {
        Ok(Some(picked)) => {
            row.selected = picked
                .into_iter()
                .filter_map(|index| choices.get(index).cloned())
                .collect();
            row.visited = true;
            Ok(RowExit::Done)
        }
        Ok(None) => Ok(RowExit::Done),
        Err(PromptError::Interrupted) => Ok(RowExit::Interrupted),
        Err(PromptError::Failed(error)) => Err(error),
    }
}

/// The one line the Plugins step shows, in place of anything to pick or
/// install, when the `[plugins]` section of the config file could not be read
/// (see [`crate::plugins_section_degraded`]); `None` when it loaded.
///
/// The section this run holds is then the defaults, not the operator's: the
/// install pipeline seeds no config row into it, so a package installed now
/// could not be configured, and the commands printed for it would fail. The
/// row and Create both decide here, so they cannot disagree.
fn unreadable_plugins_section_line(config: &Config) -> Option<String> {
    crate::plugins_section_degraded(config).then(|| {
        qta(
            "cli-quickstart-plugins-section-unreadable",
            &[("path", &config.config_path.display().to_string())],
        )
    })
}

/// Why the registry index could not be fetched, in Quickstart's terms.
///
/// The default registry's "not populated yet" error names the `--registry`
/// flag of `plugin install` and `plugin search`, which Quickstart does not
/// have, so that case gets its own line naming the environment variable that
/// picks another registry here. Every other error is printed as it reads.
fn registry_unavailable_line(error: &anyhow::Error) -> String {
    if error
        .downcast_ref::<crate::plugin_registry::DefaultRegistryUnpopulated>()
        .is_some()
    {
        qta("cli-quickstart-plugins-registry-unpopulated", &[])
    } else {
        qta(
            "cli-quickstart-plugins-registry-unavailable",
            &[("error", &terminal_safe_detail(&format!("{error:#}")))],
        )
    }
}

fn choice_label(choice: &PluginChoice) -> String {
    let name = terminal_safe(&choice.name);
    let version = terminal_safe(&choice.version);
    let description = choice
        .description
        .as_deref()
        .map(terminal_safe)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| qta("cli-plugin-no-description", &[]));
    match &choice.state {
        ChoiceState::Available => qta(
            "cli-quickstart-plugins-choice-available",
            &[
                ("name", &name),
                ("version", &version),
                ("description", &description),
            ],
        ),
        ChoiceState::Installed => qta(
            "cli-quickstart-plugins-choice-installed",
            &[
                ("name", &name),
                ("version", &version),
                ("description", &description),
            ],
        ),
        ChoiceState::InstalledOtherVersion { registry_version } => qta(
            "cli-quickstart-plugins-choice-installed-other",
            &[
                ("name", &name),
                ("version", &version),
                ("registry_version", &terminal_safe(registry_version)),
                ("description", &description),
            ],
        ),
    }
}

/// What the Create-time plugin phase did, kept for the lines Quickstart prints
/// after the agent step.
#[derive(Debug, Default)]
pub(crate) struct CreatePhase {
    outcomes: Vec<PackageOutcome>,
    /// This run turned `plugins.enabled` or `plugins.auto_discover` on.
    activation_changed: bool,
}

/// Why the plugin phase stopped before the agent step.
#[derive(Debug)]
pub(crate) enum PhaseHalt {
    /// The agent step would refuse the submission. It is dry-run before any
    /// plugin is touched, so no config was changed and nothing installed.
    AgentRejected(Vec<QuickstartError>),
    /// Ctrl+C at a prompt.
    Interrupted { outcomes: Vec<PackageOutcome> },
    /// A prompt failed for any other reason.
    Failed {
        outcomes: Vec<PackageOutcome>,
        error: anyhow::Error,
    },
}

impl PhaseHalt {
    fn from_prompt(error: PromptError, outcomes: Vec<PackageOutcome>) -> Self {
        match error {
            PromptError::Interrupted => Self::Interrupted { outcomes },
            PromptError::Failed(error) => Self::Failed { outcomes, error },
        }
    }

    /// Print why Create stopped and return the error Quickstart ends with, or
    /// `None` for Ctrl+C, which exits with status 130 like the checklist.
    pub(crate) fn report(self) -> Option<anyhow::Error> {
        match self {
            // The plugin step changed no config and installed nothing, so the
            // agent step's own report applies, as without a plugin step.
            Self::AgentRejected(errors) => Some(crate::report_agent_not_created(&errors, None)),
            Self::Interrupted { outcomes } => {
                print_progress(&outcomes);
                None
            }
            Self::Failed { outcomes, error } => {
                print_progress(&outcomes);
                Some(error)
            }
        }
    }
}

/// Say what this run installed or configured before it stopped. Every package
/// it names went through the whole publish-and-seed transaction, except one
/// whose failed publish could not be rolled back, which stays on disk and is
/// named for that reason.
fn print_progress(outcomes: &[PackageOutcome]) {
    let names = changed_names(outcomes);
    if names.is_empty() {
        eprintln!("{}", qta("cli-quickstart-plugins-stopped-none", &[]));
    } else {
        eprintln!(
            "{}",
            qta("cli-quickstart-plugins-stopped", &[("names", &names)])
        );
    }
}

fn changed_names(outcomes: &[PackageOutcome]) -> String {
    outcomes
        .iter()
        .filter(|outcome| outcome.changed_state())
        .map(|outcome| terminal_safe(outcome.name()))
        .collect::<Vec<_>>()
        .join(", ")
}

impl CreatePhase {
    /// After the agent step succeeded: the status of each installed package,
    /// from the same activation plan every registry build derives and the
    /// config row its instance reads its settings from, then the restart note.
    ///
    /// Quickstart writes the config file and never signals a running daemon,
    /// which keeps the configuration it loaded until it restarts or reloads.
    /// The note names the one CLI command that restarts it, the service
    /// restart; no CLI command asks a daemon to reload.
    pub(crate) async fn print_readiness(&self, config: &Config) {
        for line in self.readiness_lines(config).await {
            println!("{line}");
        }
    }

    async fn readiness_lines(&self, config: &Config) -> Vec<String> {
        let installed: Vec<&PackageOutcome> = self
            .outcomes
            .iter()
            .filter(|outcome| outcome.is_installed())
            .collect();
        if installed.is_empty() {
            return Vec::new();
        }
        let mut lines = vec![
            String::new(),
            qta("cli-quickstart-plugins-readiness-heading", &[]),
        ];
        // A fresh host, so every status describes the plugins directory and
        // the config as they now are.
        match crate::plugin_host_with_configured_security(config) {
            Ok(host) => {
                for outcome in installed {
                    let name = outcome.name();
                    // A package this run installed passed the load check
                    // moments ago; one installed before it may never have.
                    let load_failure = if outcome.installed_before_run() {
                        Box::pin(load_failure_line(config, &host, name)).await
                    } else {
                        None
                    };
                    lines.extend(package_readiness(config, &host, name, load_failure));
                }
            }
            Err(error) => lines.push(indented(&qta(
                "cli-quickstart-plugins-readiness-unavailable",
                &[("error", &terminal_safe_detail(&format!("{error:#}")))],
            ))),
        }
        lines.push(qta(
            "cli-quickstart-plugins-restart-note",
            &[("command", &zeroclaw_command(config, "service restart"))],
        ));
        lines
    }

    /// After the agent step failed: the lines that open its failure report
    /// when this run already changed the machine, in place of the ones that
    /// say nothing on disk was changed. They say what stays in place and give
    /// the command that removes each package this run installed. `None` when
    /// this run installed, configured and activated nothing, so the usual
    /// report is still true.
    pub(crate) fn apply_failed_headline(&self, config: &Config) -> Option<Vec<String>> {
        let names = changed_names(&self.outcomes);
        let state = match (names.is_empty(), self.activation_changed) {
            (true, false) => return None,
            (false, false) => qta(
                "cli-quickstart-plugins-apply-failed-state",
                &[("names", &names)],
            ),
            (false, true) => qta(
                "cli-quickstart-plugins-apply-failed-state-activated",
                &[("names", &names)],
            ),
            (true, true) => qta("cli-quickstart-plugins-apply-failed-activated", &[]),
        };
        let mut lines = vec![qta("cli-quickstart-plugins-agent-not-created", &[]), state];
        // Only a package this run published gets a removal command, including
        // one whose failed publish could not be rolled back. One that was
        // installed before this run stays, whatever happened to its row.
        let published: Vec<&str> = self
            .outcomes
            .iter()
            .filter(|outcome| outcome.published_by_run())
            .map(PackageOutcome::name)
            .collect();
        if !published.is_empty() {
            lines.push(qta("cli-quickstart-plugins-remove-heading", &[]));
            // A package name is lowercase letters, digits, `.`, `-` and `_`,
            // which every supported shell passes as written.
            lines.extend(
                published.iter().map(|name| {
                    indented(&zeroclaw_command(config, &format!("plugin remove {name}")))
                }),
            );
        }
        lines.push(qta("cli-quickstart-plugins-fix-and-rerun", &[]));
        Some(lines)
    }
}

/// The line that stands in for any claim that `name`, a package installed
/// before this run, is active, when the load check `zeroclaw plugin info`
/// runs finds that its component does not load against this host; `None`
/// when it loads or ships no component.
///
/// Such a package may have been installed with `--no-verify` or may predate
/// this host, and the daemon skips it at startup whatever its config says.
/// The line gives the `plugin info` command that prints the diagnostic. When
/// the check itself cannot run, the status is reported unavailable instead.
async fn load_failure_line(config: &Config, host: &PluginHost, name: &str) -> Option<String> {
    let info = host.get_plugin(name)?;
    let limits = zeroclaw_runtime::plugin_runtime::plugin_limits(config);
    match crate::installed_plugin_load_status(host, &info, limits).await {
        Ok(status) if !status.is_load_failure() => None,
        // An installed package name is lowercase letters, digits, `.`, `-`
        // and `_`, which every supported shell passes as written.
        Ok(_) => Some(qta(
            "cli-quickstart-plugins-ready-does-not-load",
            &[
                ("name", &terminal_safe(name)),
                (
                    "command",
                    &zeroclaw_command(config, &format!("plugin info {}", info.name)),
                ),
            ],
        )),
        Err(error) => Some(readiness_line(name, &Err(error))),
    }
}

/// One installed package's readiness lines, read from `host` and `config`.
///
/// `load_failure` is the [`load_failure_line`] of a package whose component
/// does not load. It comes first, and such a package is never reported
/// active. A verdict that holds the instance back is reported next. Whatever
/// the verdict, the instance is reported active only when the runtime's own
/// resolver accepts the row it reads, under the scope the activation plan
/// grants it: a row the resolver rejects fails every call. The resolver's
/// reason is then shown once, followed by the required settings the row
/// lacks, each with its `config set` command when its name is a portable
/// plugin key, the only names Quickstart prints a command for, and by any
/// required name the schema does not declare, which no row can satisfy.
/// An instance whose manifest owes it a row that does not exist gets no
/// setting command, since `config set` resolves only rows that exist: its line
/// gives the command that creates the row with no grant, and a manifest that
/// declares destinations adds the separate command that grants them. When the
/// row cannot be read, the status is reported unavailable rather than active.
fn package_readiness(
    config: &Config,
    host: &PluginHost,
    name: &str,
    load_failure: Option<String>,
) -> Vec<String> {
    let verdict = zeroclaw_runtime::plugin_runtime::tool_instance_admission(config, host, name)
        .map_err(anyhow::Error::from);
    let admitted = matches!(verdict, Ok(ToolInstanceAdmission::Admitted { .. }));
    // The daemon skips a package that does not load, whatever its verdict
    // and its row say.
    let loads = load_failure.is_none();
    let mut lines: Vec<String> = load_failure
        .map(|line| indented(&line))
        .into_iter()
        .collect();
    if !admitted {
        lines.push(indented(&readiness_line(name, &verdict)));
    }
    let display_name = terminal_safe(name);
    match instance_settings(config, host, name) {
        Ok(InstanceSettings::NoRow { instance_key }) => {
            // The same commands a skipped row gets: one that creates the row
            // with no grant, then, apart from it, the one that grants the
            // declared destinations.
            lines.push(indented(&qta(
                "cli-quickstart-plugins-ready-no-row",
                &[
                    ("name", &display_name),
                    ("command", &create_row_command(config, &instance_key)),
                ],
            )));
            lines.extend(
                declared_grant_line(
                    config,
                    name,
                    &instance_key,
                    &crate::declared_egress_hosts(host, name),
                )
                .map(|line| indented(&line)),
            );
        }
        Ok(InstanceSettings::Rejected {
            instance_key,
            portable,
            nonportable,
            undeclared,
            reason,
        }) => {
            lines.push(indented(&qta(
                "cli-quickstart-plugins-ready-rejected",
                &[
                    ("name", &display_name),
                    ("error", &terminal_safe_detail(&reason)),
                ],
            )));
            if !portable.is_empty() {
                lines.push(indented(&qta(
                    "cli-quickstart-plugins-ready-missing-settings",
                    &[("name", &display_name), ("keys", &portable.join(", "))],
                )));
                // Instance settings are secret, so `config set` asks for each
                // value with masked input and the command carries none.
                lines.extend(portable.iter().map(|key| {
                    indented(&indented(&zeroclaw_command(
                        config,
                        &format!("config set {}", setting_path(&instance_key, key)),
                    )))
                }));
            }
            if !nonportable.is_empty() {
                lines.push(indented(&qta(
                    "cli-quickstart-plugins-ready-missing-nonportable",
                    &[("name", &display_name), ("keys", &nonportable.join(", "))],
                )));
            }
            if !undeclared.is_empty() {
                lines.push(indented(&qta(
                    "cli-quickstart-plugins-ready-undeclared-required",
                    &[("name", &display_name), ("keys", &undeclared.join(", "))],
                )));
            }
        }
        // The active line names the instance's config entry, which a tool
        // that owns no state never gets.
        Ok(InstanceSettings::AcceptedWithoutRow) if admitted && loads => {
            lines.push(indented(&qta(
                "cli-quickstart-plugins-ready-no-entry-needed",
                &[("name", &display_name)],
            )));
        }
        Ok(InstanceSettings::Accepted) if admitted && loads => {
            lines.push(indented(&readiness_line(name, &verdict)));
        }
        Ok(_) => {}
        Err(error) => lines.push(indented(&readiness_line(name, &Err(error)))),
    }
    lines
}

/// What the runtime's resolver makes of `name`'s tool instance settings, as
/// the plugins directory and the config hold them now.
enum InstanceSettings {
    /// The package is not installed, or provides no tool.
    NoInstance,
    /// The manifest owes the instance a row, and the config holds none.
    NoRow { instance_key: String },
    /// The manifest owns no settings and no network reach, so install seeds
    /// no row for the instance, and the resolver accepts it without one.
    AcceptedWithoutRow,
    /// The resolver accepts the instance's row.
    Accepted,
    /// The resolver rejects the instance's row, so every call fails.
    Rejected {
        instance_key: String,
        /// Required settings the row lacks whose names are portable plugin
        /// keys, each printed with its `config set` command.
        portable: Vec<String>,
        /// Required settings the row lacks whose declared names are outside
        /// the portable grammar, terminal-safe, printed without a command.
        nonportable: Vec<String>,
        /// Required names the schema does not declare, terminal-safe: no row
        /// satisfies the schema while it has any.
        undeclared: Vec<String>,
        /// The resolver's reason, which names properties and schema paths,
        /// never values.
        reason: String,
    },
}

/// Resolve `name`'s tool instance settings as the runtime does on every call:
/// the manifest `host` admitted, the scope the activation plan grants the
/// instance, and the row `config` holds under its key. The verdict does not
/// matter, so an instance held back today still reports what it lacks.
///
/// # Errors
///
/// When more than one row holds the instance key, or the scope or its key
/// cannot be derived from the manifest.
fn instance_settings(
    config: &Config,
    host: &PluginHost,
    name: &str,
) -> anyhow::Result<InstanceSettings> {
    let Some(manifest) = host
        .manifest(name)
        .filter(|manifest| manifest.capabilities.contains(&PluginCapability::Tool))
    else {
        return Ok(InstanceSettings::NoInstance);
    };
    let scope = tool_instance_scope(manifest)?;
    let instance_key = scope.id().config_entry_key()?;
    let row = config.plugins.entry_config(&instance_key)?;
    if row.is_none() && !crate::manifest_config_entries(manifest)?.is_empty() {
        return Ok(InstanceSettings::NoRow { instance_key });
    }
    Ok(
        match zeroclaw::plugins::config::resolve_plugin_config(manifest, &scope, row) {
            Ok(_) if row.is_none() => InstanceSettings::AcceptedWithoutRow,
            Ok(_) => InstanceSettings::Accepted,
            Err(error) => InstanceSettings::Rejected {
                portable: missing_required_settings(manifest, row),
                nonportable: nonportable_required_settings(manifest, row),
                undeclared: undeclared_required_settings(manifest),
                reason: error.to_string(),
                instance_key,
            },
        },
    )
}

/// The scope the activation plan admits `manifest`'s tool instance under: the
/// package binding, holding every permission the manifest requests. A row
/// resolved under it is read as the running instance reads it.
fn tool_instance_scope(manifest: &PluginManifest) -> anyhow::Result<PluginInstanceScope> {
    Ok(PluginInstanceScope::for_package_binding(
        manifest,
        PluginCapability::Tool,
        manifest.permissions.iter().copied(),
    )?)
}

fn readiness_line(name: &str, verdict: &anyhow::Result<ToolInstanceAdmission>) -> String {
    let name = terminal_safe(name);
    match verdict {
        Ok(ToolInstanceAdmission::Admitted { instance_key }) => qta(
            "cli-quickstart-plugins-ready",
            &[("name", &name), ("key", instance_key)],
        ),
        Ok(ToolInstanceAdmission::PluginsDisabled) => qta(
            "cli-quickstart-plugins-ready-plugins-disabled",
            &[("name", &name)],
        ),
        Ok(ToolInstanceAdmission::AutoDiscoverDisabled) => qta(
            "cli-quickstart-plugins-ready-auto-discover-disabled",
            &[("name", &name)],
        ),
        Ok(ToolInstanceAdmission::PackageNotInstalled) => qta(
            "cli-quickstart-plugins-ready-not-installed",
            &[("name", &name)],
        ),
        Ok(ToolInstanceAdmission::PackageNotATool) => qta(
            "cli-quickstart-plugins-ready-not-a-tool",
            &[("name", &name)],
        ),
        Ok(ToolInstanceAdmission::CeilingReached {
            max_active_instances,
        }) => qta(
            "cli-quickstart-plugins-ready-ceiling",
            &[("name", &name), ("max", &max_active_instances.to_string())],
        ),
        Err(error) => qta(
            "cli-quickstart-plugins-ready-unknown",
            &[
                ("name", &name),
                ("error", &terminal_safe_detail(&format!("{error:#}"))),
            ],
        ),
    }
}

/// Install, configure and offer to activate the packages picked in the
/// Plugins row. Runs on Create, before the agent step, and does nothing when
/// the row holds no selection. When the `[plugins]` section of the config
/// file could not be read, it says so and installs nothing.
///
/// # Errors
///
/// A [`PhaseHalt`] when the agent step would refuse `submission`, which is
/// checked before any plugin is touched, or when a prompt is interrupted or
/// fails. Every package the halt reports as installed went through the whole
/// publish-and-seed transaction, or failed it without being rolled back, and
/// the halt says which; the rest of the selection was not touched.
pub(crate) async fn run_create_phase(
    config: &mut Config,
    row: &PluginsRow,
    submission: &BuilderSubmission,
) -> Result<CreatePhase, PhaseHalt> {
    if row.selected.is_empty() {
        return Ok(CreatePhase::default());
    }
    let registry =
        RegistryClient::new(RegistryTimeouts::default()).map_err(|error| PhaseHalt::Failed {
            outcomes: Vec::new(),
            error,
        })?;
    Box::pin(create_phase_with(
        config,
        &row.selected,
        submission,
        &registry,
        &mut TerminalPrompter,
    ))
    .await
}

/// The Create-time phase for a non-empty selection: the agent step's dry run,
/// then the plugins. Nothing at all when the `[plugins]` section of the
/// config file could not be read.
///
/// The agent step runs after the plugins, which change the machine. A
/// submission it would refuse is therefore refused here, while the plugin
/// step has changed no config and installed nothing, so the agent step's
/// usual report applies.
async fn create_phase_with<P: QuickstartPrompter>(
    config: &mut Config,
    selection: &[PluginChoice],
    submission: &BuilderSubmission,
    registry: &RegistryClient,
    prompter: &mut P,
) -> Result<CreatePhase, PhaseHalt> {
    // Checked again rather than trusted from the row: this is the only place
    // plugins are installed, and the config it installs against decides.
    if let Some(line) = unreadable_plugins_section_line(config) {
        prompter.say("");
        prompter.say(&line);
        return Ok(CreatePhase::default());
    }
    zeroclaw_runtime::quickstart::validate_only_with_surface(submission, config, Surface::Cli)
        .map_err(PhaseHalt::AgentRejected)?;
    Box::pin(install_and_activate(config, selection, registry, prompter)).await
}

/// The run-scoped attributes every event of one plugin phase carries: a run id
/// and package names, never configuration keys or values.
struct PhaseLog {
    run_id: String,
}

impl PhaseLog {
    fn new() -> Self {
        let run_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| format!("{:x}{:x}", elapsed.as_secs(), elapsed.subsec_nanos()))
            .unwrap_or_else(|_| format!("{:x}", std::process::id()));
        Self { run_id }
    }

    fn attrs(&self, extra: serde_json::Value) -> serde_json::Value {
        let mut attrs = serde_json::json!({
            "quickstart.run_id": self.run_id,
            "quickstart.surface": "cli",
        });
        if let (Some(base), serde_json::Value::Object(extra)) = (attrs.as_object_mut(), extra) {
            base.extend(extra);
        }
        attrs
    }

    fn start(&self, selected: usize) {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start)
                .with_attrs(self.attrs(::serde_json::json!({ "selected": selected }))),
            "quickstart plugins: phase start"
        );
    }

    /// The attributes of one package's event: the package name and the kind
    /// of outcome. Nothing a prompt collected can reach them.
    fn outcome_attrs(&self, outcome: &PackageOutcome) -> serde_json::Value {
        self.attrs(::serde_json::json!({
            "plugin": outcome.name(),
            "outcome": outcome.kind(),
        }))
    }

    fn outcome(&self, outcome: &PackageOutcome) {
        let attrs = self.outcome_attrs(outcome);
        match outcome {
            PackageOutcome::Failed { .. } => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(attrs),
                "quickstart plugins: package not installed"
            ),
            // Its install failed and could not be undone, so it stays.
            PackageOutcome::RollbackFailed { .. } => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(attrs),
                "quickstart plugins: package left installed after its install failed"
            ),
            PackageOutcome::Installed { .. }
            | PackageOutcome::AlreadyInstalled { .. }
            | PackageOutcome::AlreadyInstalledSkipped { .. }
            | PackageOutcome::Skipped { .. }
            | PackageOutcome::Refused { .. } => ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(attrs),
                "quickstart plugins: package handled"
            ),
        }
    }

    fn activation(&self, preview: &ActivationPreview, accepted: bool) {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Approve)
                .with_outcome(if accepted {
                    ::zeroclaw_log::EventOutcome::Success
                } else {
                    ::zeroclaw_log::EventOutcome::Unknown
                })
                .with_attrs(self.attrs(::serde_json::json!({
                    "enable_plugins": preview.enable_plugins,
                    "enable_auto_discover": preview.enable_auto_discover,
                    "activated": preview.activated.len(),
                    "accepted": accepted,
                }))),
            "quickstart plugins: activation decided"
        );
    }
}

async fn install_and_activate<P: QuickstartPrompter>(
    config: &mut Config,
    selection: &[PluginChoice],
    registry: &RegistryClient,
    prompter: &mut P,
) -> Result<CreatePhase, PhaseHalt> {
    let mut phase = CreatePhase::default();
    if selection.is_empty() {
        return Ok(phase);
    }
    let log = PhaseLog::new();
    log.start(selection.len());
    prompter.say("");
    prompter.say(&qta("cli-quickstart-plugins-installing", &[]));

    let mut host = match crate::plugin_host_with_configured_security(config) {
        Ok(host) => host,
        Err(error) => {
            // Nothing can be admitted without a host, and nothing was written.
            let detail = terminal_safe_detail(&format!("{error:#}"));
            for choice in selection {
                prompter.say(&indented(&qta(
                    "cli-quickstart-plugins-failed",
                    &[("name", &terminal_safe(&choice.name)), ("error", &detail)],
                )));
                let outcome = PackageOutcome::Failed {
                    name: choice.name.clone(),
                    stage: FailureStage::Admission,
                };
                log.outcome(&outcome);
                phase.outcomes.push(outcome);
            }
            return Ok(phase);
        }
    };

    for choice in selection {
        match Box::pin(install_one(config, &mut host, choice, registry, prompter)).await {
            Ok(outcome) => {
                log.outcome(&outcome);
                phase.outcomes.push(outcome);
            }
            Err(error) => return Err(PhaseHalt::from_prompt(error, phase.outcomes)),
        }
    }

    if phase.outcomes.iter().any(PackageOutcome::is_installed) {
        match Box::pin(activation_consent(
            config,
            &host,
            &phase.outcomes,
            &log,
            prompter,
        ))
        .await
        {
            Ok(changed) => phase.activation_changed = changed,
            Err(error) => return Err(PhaseHalt::from_prompt(error, phase.outcomes)),
        }
    }
    Ok(phase)
}

fn has_row(config: &Config, instance_key: &str) -> bool {
    config
        .plugins
        .entries
        .iter()
        .any(|entry| entry.name == instance_key)
}

/// One selected package, from the registry entry to an installed, seeded and
/// configured instance. Every failure before the publish leaves the machine
/// exactly as it was; the publish itself is the canonical transaction, which
/// rolls its own package back. When that rollback fails too, the outcome
/// records that the package stayed.
async fn install_one<P: QuickstartPrompter>(
    config: &mut Config,
    host: &mut PluginHost,
    choice: &PluginChoice,
    registry: &RegistryClient,
    prompter: &mut P,
) -> PromptResult<PackageOutcome> {
    prompter.say("");
    let name = choice.name.clone();
    let display_name = terminal_safe(&name);
    // The plugins directory decides, not the state the row saw: a package
    // installed since then is left alone.
    if host.manifest(&name).is_some() {
        return Box::pin(keep_installed(config, host, &name, prompter)).await;
    }
    let Some(entry) = choice.registry_entry.as_ref() else {
        prompter.say(&indented(&qta(
            "cli-quickstart-plugins-not-available",
            &[("name", &display_name)],
        )));
        return Ok(PackageOutcome::Refused {
            name,
            reason: Refusal::NotAvailable,
        });
    };
    prompter.say(&qta(
        "cli-quickstart-plugins-resolving",
        &[
            ("name", &display_name),
            ("version", &terminal_safe(&entry.version)),
        ],
    ));
    // Onboarding fails closed on an unverifiable archive: `plugin install`
    // checks the digest only when the registry carries one, this path
    // requires it before the archive is even requested.
    if entry
        .sha256
        .as_deref()
        .is_none_or(|digest| digest.trim().is_empty())
    {
        prompter.say(&indented(&qta(
            "cli-quickstart-plugins-no-integrity-hash",
            &[("name", &display_name)],
        )));
        return Ok(PackageOutcome::Refused {
            name,
            reason: Refusal::NoIntegrityHash,
        });
    }

    // The extracted package lives in a temporary directory that must outlive
    // the publish, which copies from it.
    let downloaded = match registry.download_entry(entry).await {
        Ok(downloaded) => downloaded,
        Err(error) => {
            return Ok(failure(
                prompter,
                &name,
                FailureStage::Download,
                &format!("{error:#}"),
            ));
        }
    };
    if !downloaded
        .manifest()
        .capabilities
        .contains(&PluginCapability::Tool)
    {
        prompter.say(&indented(&qta(
            "cli-quickstart-plugins-not-a-tool",
            &[("name", &display_name)],
        )));
        return Ok(PackageOutcome::Refused {
            name,
            reason: Refusal::NotATool,
        });
    }
    let admitted = match host.admit_source(&downloaded.plugin_dir().display().to_string()) {
        Ok(admitted) => admitted,
        Err(error) => {
            return Ok(failure(
                prompter,
                &name,
                FailureStage::Admission,
                &error.to_string(),
            ));
        }
    };
    // The load check `plugin install` gates on, run against the exact bytes
    // admission read. Its refusal is Quickstart's own: Quickstart has no
    // `--no-verify`, so the text names the install command that has it,
    // addressed to this configuration like every command this step prints.
    let limits = zeroclaw_runtime::plugin_runtime::plugin_limits(config);
    if let Some(component) = admitted.component()
        && let Err(error) = zeroclaw::plugins::validate::verify_component_loads(
            component,
            admitted.manifest(),
            limits,
        )
        .await
    {
        // An admitted package name is lowercase letters, digits, `.`, `-`
        // and `_`, which every supported shell passes as written.
        let command = zeroclaw_command(
            config,
            &format!("plugin install {} --no-verify", admitted.manifest().name),
        );
        prompter.say(&indented(&qta(
            "cli-quickstart-plugins-load-check-failed",
            &[
                ("name", &display_name),
                ("error", &terminal_safe_detail(&format!("{error:#}"))),
                ("command", &command),
            ],
        )));
        return Ok(PackageOutcome::Failed {
            name,
            stage: FailureStage::LoadCheck,
        });
    }

    let manifest = admitted.manifest().clone();
    print_package_summary(&manifest, prompter);
    let entries = match crate::manifest_config_entries(&manifest) {
        Ok(entries) => entries,
        Err(error) => {
            return Ok(failure(
                prompter,
                &name,
                FailureStage::Admission,
                &format!("{error:#}"),
            ));
        }
    };
    let instance_key = entries.first().map(|(_, key)| key.clone());
    // An existing row keeps its grant and its settings: the decision and the
    // prompts are only for a row this run creates.
    let row_exists = instance_key
        .as_deref()
        .is_some_and(|key| has_row(config, key));

    let decision = if row_exists {
        prompter.say(&indented(&qta(
            "cli-quickstart-plugins-existing-row",
            &[("name", &display_name)],
        )));
        EgressDecision::Declared
    } else {
        match egress_decision(&manifest, prompter)? {
            Some(decision) => decision,
            None => {
                prompter.say(&indented(&qta(
                    "cli-quickstart-plugins-skipped",
                    &[("name", &display_name)],
                )));
                return Ok(PackageOutcome::Skipped { name });
            }
        }
    };
    let settings = if row_exists || instance_key.is_none() {
        CollectedSettings::default()
    } else {
        match collect_settings(&manifest, prompter)? {
            Some(settings) => settings,
            None => {
                prompter.say(&indented(&qta(
                    "cli-quickstart-plugins-skipped",
                    &[("name", &display_name)],
                )));
                return Ok(PackageOutcome::Skipped { name });
            }
        }
    };

    // The canonical transaction runs against a copy, so a refusal or a
    // failed save leaves the in-memory config exactly as it is on disk and
    // nothing half-seeded can reach disk later with the agent step.
    let mut staged = config.clone();
    let published = Box::pin(crate::publish_and_seed_plugin(
        host,
        &mut staged,
        admitted,
        decision,
        |_| {},
    ))
    .await;
    drop(downloaded);
    if let Err(error) = published {
        let (outcome, line) = publish_failure(config, &name, &error);
        prompter.say(&indented(&line));
        return Ok(outcome);
    }
    *config = staged;
    prompter.say(&indented(&qta(
        "cli-quickstart-plugins-installed",
        &[
            ("name", &display_name),
            ("version", &terminal_safe(&manifest.version)),
        ],
    )));

    if let Some(instance_key) = instance_key.as_deref() {
        if !settings.values.is_empty() {
            Box::pin(save_settings(
                config,
                &display_name,
                instance_key,
                &settings.values,
                prompter,
            ))
            .await;
        }
        if !row_exists
            && crate::manifest_requests_network(&manifest)
            && crate::declared_egress_for_manifest(&manifest).is_empty()
        {
            let command = egress_set_command(
                crate::egress_command_config_dir(config),
                instance_key,
                &["<host>".to_string()],
            );
            prompter.say(&indented(&qta(
                "cli-quickstart-plugins-no-declared-hosts",
                &[("name", &display_name), ("command", &command)],
            )));
        }
    }
    Ok(PackageOutcome::Installed { name })
}

/// A package that was installed before this run: no download, no publish, no
/// upgrade. The only write is the idempotent seeding of a row it lacks, which
/// creates rows only when absent and never touches an existing one.
///
/// A row this run creates is a new grant. For a package that declares
/// destinations it could reach, the operator gets the package summary and the
/// same egress decision a fresh install gets, and a skip leaves the row
/// absent, recorded as its own outcome so the activation question does not
/// count the package as picked. The skip prints the command that creates the
/// row with no grant and, apart from it, the one that grants the declaration.
/// Nothing is asked when the row already exists.
async fn keep_installed<P: QuickstartPrompter>(
    config: &mut Config,
    host: &PluginHost,
    name: &str,
    prompter: &mut P,
) -> PromptResult<PackageOutcome> {
    let display_name = terminal_safe(name);
    prompter.say(&indented(&qta(
        "cli-quickstart-plugins-already-installed",
        &[("name", &display_name)],
    )));
    let kept = |seeded_row| PackageOutcome::AlreadyInstalled {
        name: name.to_string(),
        seeded_row,
    };
    let entries = match crate::installed_plugin_config_entries(host, name) {
        Ok(entries) => entries,
        Err(error) => {
            return Ok(failure(
                prompter,
                name,
                FailureStage::Seed,
                &format!("{error:#}"),
            ));
        }
    };
    let Some(missing_key) = entries
        .iter()
        .map(|(_, key)| key)
        .find(|key| !has_row(config, key))
        .cloned()
    else {
        return Ok(kept(false));
    };
    let declared = crate::declared_egress_hosts(host, name);
    let decision = match host.manifest(name) {
        Some(manifest) if asks_for_egress(manifest) => {
            prompter.say(&indented(&qta(
                "cli-quickstart-plugins-missing-row",
                &[("name", &display_name)],
            )));
            print_package_summary(manifest, prompter);
            match egress_decision(manifest, prompter)? {
                Some(decision) => decision,
                None => {
                    // `config set` resolves only rows that exist, so the way
                    // back is the command that creates this one. The operator
                    // just declined the declared destinations, so that command
                    // grants none of them; granting them is a separate command.
                    prompter.say(&indented(&qta(
                        "cli-quickstart-plugins-row-skipped",
                        &[
                            ("name", &display_name),
                            ("command", &create_row_command(config, &missing_key)),
                        ],
                    )));
                    if let Some(line) = declared_grant_line(config, name, &missing_key, &declared) {
                        prompter.say(&indented(&line));
                    }
                    return Ok(PackageOutcome::AlreadyInstalledSkipped {
                        name: name.to_string(),
                    });
                }
            }
        }
        _ => EgressDecision::Declared,
    };
    let mut staged = config.clone();
    let seeded = Box::pin(crate::seed_plugin_config_entries(
        &mut staged,
        name,
        &entries,
        &declared,
        decision,
    ))
    .await;
    Ok(match seeded {
        Ok(()) => {
            *config = staged;
            kept(true)
        }
        Err(error) => failure(prompter, name, FailureStage::Seed, &format!("{error:#}")),
    })
}

/// Report a package that failed at `stage` and record it. Nothing durable
/// happened for it: every stage before the publish writes nothing, and the
/// publish rolls its own package back. A publish whose rollback failed too is
/// the one exception, which [`publish_failure`] reports.
fn failure<P: QuickstartPrompter>(
    prompter: &mut P,
    name: &str,
    stage: FailureStage,
    error: &str,
) -> PackageOutcome {
    prompter.say(&indented(&qta(
        "cli-quickstart-plugins-failed",
        &[
            ("name", &terminal_safe(name)),
            ("error", &terminal_safe_detail(error)),
        ],
    )));
    PackageOutcome::Failed {
        name: name.to_string(),
        stage,
    }
}

/// The outcome of a publish-and-seed transaction that failed for the package
/// `name`, and the line that reports it.
///
/// The transaction rolls its own package back, so the package was not
/// installed. When that rollback failed too, the error says so in a typed
/// layer, never only in its text: the package stays in the plugins directory
/// although the host no longer lists it. The line then says why the package
/// could not be configured, that it is still installed, and the command that
/// removes it, addressed to this configuration. The typed layer's own text,
/// which `plugin install` prints, is left out: its removal command names no
/// configuration directory.
fn publish_failure(config: &Config, name: &str, error: &anyhow::Error) -> (PackageOutcome, String) {
    let display_name = terminal_safe(name);
    let Some(rollback) = error.downcast_ref::<crate::PublishRollbackFailed>() else {
        return (
            PackageOutcome::Failed {
                name: name.to_string(),
                stage: FailureStage::Publish,
            },
            qta(
                "cli-quickstart-plugins-failed",
                &[
                    ("name", &display_name),
                    ("error", &terminal_safe_detail(&format!("{error:#}"))),
                ],
            ),
        );
    };
    let layer = rollback.to_string();
    let cause = error
        .chain()
        .map(ToString::to_string)
        .filter(|text| *text != layer)
        .collect::<Vec<_>>()
        .join(": ");
    // An installed package name is lowercase letters, digits, `.`, `-` and
    // `_`, which every supported shell passes as written.
    let command = zeroclaw_command(config, &format!("plugin remove {}", rollback.package));
    (
        PackageOutcome::RollbackFailed {
            name: name.to_string(),
        },
        qta(
            "cli-quickstart-plugins-rollback-failed",
            &[
                ("name", &display_name),
                ("error", &terminal_safe_detail(&cause)),
                (
                    "rollback_error",
                    &terminal_safe_detail(&rollback.rollback_error),
                ),
                ("command", &command),
            ],
        ),
    )
}

/// What the package is and what it asks for, printed before any question
/// about it. Every publisher-controlled string is made terminal-safe first.
fn print_package_summary<P: QuickstartPrompter>(manifest: &PluginManifest, prompter: &mut P) {
    let none = qta("cli-quickstart-plugins-about-none", &[]);
    let list = |items: Vec<String>| {
        if items.is_empty() {
            none.clone()
        } else {
            items.join(", ")
        }
    };
    prompter.say(&indented(&qta(
        "cli-quickstart-plugins-about-name",
        &[
            ("name", &terminal_safe(&manifest.name)),
            ("version", &terminal_safe(&manifest.version)),
        ],
    )));
    if let Some(author) = manifest
        .author
        .as_deref()
        .map(terminal_safe)
        .filter(|text| !text.is_empty())
    {
        prompter.say(&indented(&qta(
            "cli-quickstart-plugins-about-author",
            &[("author", &author)],
        )));
    }
    if let Some(description) = manifest
        .description
        .as_deref()
        .map(terminal_safe)
        .filter(|text| !text.is_empty())
    {
        prompter.say(&indented(&qta(
            "cli-quickstart-plugins-about-description",
            &[("description", &description)],
        )));
    }
    prompter.say(&indented(&qta(
        "cli-quickstart-plugins-about-capabilities",
        &[("list", &list(capability_names(&manifest.capabilities)))],
    )));
    prompter.say(&indented(&qta(
        "cli-quickstart-plugins-about-permissions",
        &[("list", &list(permission_names(&manifest.permissions)))],
    )));
    let destinations: Vec<String> = canonical_hosts(&manifest.egress.hosts)
        .iter()
        .map(|host| terminal_safe(host))
        .collect();
    prompter.say(&indented(&qta(
        "cli-quickstart-plugins-about-destinations",
        &[("list", &list(destinations))],
    )));
}

/// Whether a row created for `manifest` grants anything the operator must
/// decide on: only when the declaration `plugin install` would seed into it
/// is not empty. That rule, `http_client` and a declaration, is the root's
/// [`crate::declared_egress_for_manifest`]; this does not restate it.
fn asks_for_egress(manifest: &PluginManifest) -> bool {
    !canonical_hosts(&crate::declared_egress_for_manifest(manifest)).is_empty()
}

/// Ask whether the destinations the manifest declares are granted, when
/// [`asks_for_egress`] says there is anything to decide. `None` skips the
/// package.
fn egress_decision<P: QuickstartPrompter>(
    manifest: &PluginManifest,
    prompter: &mut P,
) -> PromptResult<Option<EgressDecision>> {
    // The answers, in the order the question lists them.
    const GRANT: usize = 0;
    const WITHHOLD: usize = 1;

    if !asks_for_egress(manifest) {
        return Ok(Some(EgressDecision::Declared));
    }
    let options = [
        qta("cli-quickstart-plugins-egress-grant", &[]),
        qta("cli-quickstart-plugins-egress-withhold", &[]),
        qta("cli-quickstart-plugins-egress-skip", &[]),
    ];
    let prompt = qta(
        "cli-quickstart-plugins-egress-prompt",
        &[("name", &terminal_safe(&manifest.name))],
    );
    // A new external surface starts closed: the cursor rests on installing
    // without network access, so accepting the default grants a publisher's
    // destinations nothing.
    Ok(match prompter.select(&prompt, &options, WITHHOLD)? {
        Some(GRANT) => Some(EgressDecision::Declared),
        Some(WITHHOLD) => Some(EgressDecision::Withheld),
        Some(_) | None => None,
    })
}

enum FieldAnswer {
    Value(String),
    Unset,
    Cancelled,
}

fn ask_field<P: QuickstartPrompter>(
    field: &ConfigField,
    prompter: &mut P,
) -> PromptResult<FieldAnswer> {
    let hint = match field.kind {
        ValueKind::Array if field.choices.is_none() => {
            Some(qta("cli-quickstart-plugins-field-json-array", &[]))
        }
        ValueKind::Object if field.choices.is_none() => {
            Some(qta("cli-quickstart-plugins-field-json-object", &[]))
        }
        _ => None,
    };
    let descriptor = field_descriptor(field, hint.as_deref());
    Ok(match prompter.field(&descriptor)? {
        Some(raw) => encode_value(field, &raw).map_or(FieldAnswer::Unset, FieldAnswer::Value),
        None => FieldAnswer::Cancelled,
    })
}

/// What the settings prompts produced for one instance.
#[derive(Default)]
struct CollectedSettings {
    /// Validated values, written once the package is published.
    values: BTreeMap<String, String>,
}

/// Prompt for the instance's settings and validate them against the
/// manifest schema with the resolver the runtime uses. `None` skips the
/// package. Values stay in memory until the publish succeeded and are never
/// printed or logged; only their key names are.
///
/// When the schema requires a setting no prompt here asks for, no answers can
/// satisfy it, so nothing is asked: the operator is told which settings those
/// are and decides at once whether to install the package without settings,
/// rather than typing values that could only be thrown away.
fn collect_settings<P: QuickstartPrompter>(
    manifest: &PluginManifest,
    prompter: &mut P,
) -> PromptResult<Option<CollectedSettings>> {
    let Some(schema) = manifest.config_schema.as_ref() else {
        return Ok(Some(CollectedSettings::default()));
    };
    let fields = config_fields(schema);
    let name = terminal_safe(&manifest.name);
    if !fields.required_unprompted.is_empty() {
        prompter.say(&indented(&qta(
            "cli-quickstart-plugins-config-required-unsupported",
            &[
                ("name", &name),
                ("keys", &fields.required_unprompted.join(", ")),
            ],
        )));
        return install_without_settings(&name, prompter);
    }
    if !fields.unsupported.is_empty() {
        prompter.say(&indented(&qta(
            "cli-quickstart-plugins-config-unsupported",
            &[("name", &name), ("keys", &fields.unsupported.join(", "))],
        )));
    }
    if fields.fields.is_empty() {
        return Ok(Some(CollectedSettings::default()));
    }
    prompter.say(&indented(&qta(
        "cli-quickstart-plugins-config-heading",
        &[("name", &name)],
    )));
    let (required, optional): (Vec<&ConfigField>, Vec<&ConfigField>) =
        fields.fields.iter().partition(|field| field.required);

    // One re-prompt after a validation failure, then the operator decides.
    for _attempt in 0..2 {
        let mut values = BTreeMap::new();
        for field in &required {
            match ask_field(field, prompter)? {
                FieldAnswer::Value(value) => {
                    values.insert(field.key.clone(), value);
                }
                FieldAnswer::Unset => {}
                FieldAnswer::Cancelled => {
                    prompter.say(&indented(&qta(
                        "cli-quickstart-plugins-config-cancelled",
                        &[("name", &name)],
                    )));
                    return Ok(None);
                }
            }
        }
        if !optional.is_empty()
            && prompter.confirm(
                &qta(
                    "cli-quickstart-plugins-config-optional-prompt",
                    &[("name", &name)],
                ),
                false,
            )? == Some(true)
        {
            for field in &optional {
                // Backing out of an optional setting leaves it unset.
                if let FieldAnswer::Value(value) = ask_field(field, prompter)? {
                    values.insert(field.key.clone(), value);
                }
            }
        }
        match validate_settings(manifest, &values) {
            Ok(()) => return Ok(Some(CollectedSettings { values })),
            Err(error) => prompter.say(&indented(&qta(
                "cli-quickstart-plugins-config-invalid",
                &[("name", &name), ("error", &terminal_safe_detail(&error))],
            ))),
        }
    }
    install_without_settings(&name, prompter)
}

/// Ask whether to install the package named `name` (terminal-safe) without
/// its settings, defaulting to no. `None` skips the package.
fn install_without_settings<P: QuickstartPrompter>(
    name: &str,
    prompter: &mut P,
) -> PromptResult<Option<CollectedSettings>> {
    let proceed = prompter.confirm(
        &qta(
            "cli-quickstart-plugins-config-defaults-prompt",
            &[("name", name)],
        ),
        false,
    )?;
    // Nothing is written, and the resolver fills in no defaults: every
    // required property stays unset until the operator sets it, and the
    // status printed after Create reads that from the row.
    Ok((proceed == Some(true)).then(CollectedSettings::default))
}

/// Validate a whole settings map as the runtime resolves it. The resolver's
/// errors name properties and schema paths, never values.
fn validate_settings(
    manifest: &PluginManifest,
    values: &BTreeMap<String, String>,
) -> Result<(), String> {
    let scope = tool_instance_scope(manifest).map_err(|error| error.to_string())?;
    let configured: HashMap<String, String> = values
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    zeroclaw::plugins::config::resolve_plugin_config(manifest, &scope, Some(&configured))
        .map(drop)
        .map_err(|error| error.to_string())
}

fn setting_path(instance_key: &str, key: &str) -> String {
    format!("plugins.entries.{instance_key}.config.{key}")
}

/// Write validated settings through the primitives `zeroclaw config set`
/// uses: one `set_prop_persistent` per key, then one incremental save. The
/// package is already installed, so a failure is reported with the command
/// that finishes the job rather than rolled back.
async fn save_settings<P: QuickstartPrompter>(
    config: &mut Config,
    display_name: &str,
    instance_key: &str,
    settings: &BTreeMap<String, String>,
    prompter: &mut P,
) {
    let mut staged = config.clone();
    let written: anyhow::Result<()> = async {
        for (key, value) in settings {
            staged.set_prop_persistent(&setting_path(instance_key, key), value)?;
        }
        Box::pin(staged.save_dirty()).await
    }
    .await;
    let keys = settings.keys().cloned().collect::<Vec<_>>().join(", ");
    match written {
        Ok(()) => {
            *config = staged;
            prompter.say(&indented(&qta(
                "cli-quickstart-plugins-config-saved",
                &[("name", display_name), ("keys", &keys)],
            )));
        }
        // Instance settings are secret: `config set` asks for each value with
        // masked input, so the command carries no value to type into shell
        // history, as in the status lines.
        Err(error) => prompter.say(&indented(&qta(
            "cli-quickstart-plugins-config-save-failed",
            &[
                ("name", display_name),
                ("keys", &keys),
                ("error", &terminal_safe_detail(&format!("{error:#}"))),
                (
                    "command",
                    &zeroclaw_command(
                        config,
                        &format!("config set {}", setting_path(instance_key, "<key>")),
                    ),
                ),
            ],
        ))),
    }
}

/// `zeroclaw --config-dir '<dir>' <args>`: an operator command addressed to
/// the configuration this run loaded, as the grant ceremony addresses its
/// commands.
fn zeroclaw_command(config: &Config, args: &str) -> String {
    let config_dir = crate::egress_command_config_dir(config);
    let dir = config_dir.to_string_lossy();
    let (dialect, marker) = ShellDialect::host().command_form(&[&dir]);
    format!(
        "{marker}{} {args}",
        zeroclaw_invocation_for(dialect, config_dir)
    )
}

/// `zeroclaw --config-dir '<dir>' config set <path> <value>`.
fn config_set_command(config: &Config, path: &str, value: &str) -> String {
    zeroclaw_command(config, &format!("config set {path} {value}"))
}

/// The command that creates `instance_key`'s missing config row, which no
/// `config set` command can do. The row it creates grants no destination: a
/// command this step prints never grants network reach on the operator's
/// behalf, so reaching the declared destinations stays the separate step
/// [`declared_grant_line`] offers.
fn create_row_command(config: &Config, instance_key: &str) -> String {
    egress_create_command(crate::egress_command_config_dir(config), instance_key, &[])
}

/// The optional line that follows [`create_row_command`]: the command that
/// grants the destinations the manifest declares, which resolves once the row
/// exists. `declared` is the declaration `plugin install` would seed, which
/// counts only with `http_client` (see [`crate::declared_egress_hosts`]).
/// `None` when it is empty, since there is nothing to grant.
fn declared_grant_line(
    config: &Config,
    name: &str,
    instance_key: &str,
    declared: &[String],
) -> Option<String> {
    let hosts = canonical_hosts(declared);
    if hosts.is_empty() {
        return None;
    }
    Some(qta(
        "cli-quickstart-plugins-grant-declared-later",
        &[
            ("name", &terminal_safe(name)),
            ("count", &hosts.len().to_string()),
            (
                "command",
                &egress_set_command(
                    crate::egress_command_config_dir(config),
                    instance_key,
                    &hosts,
                ),
            ),
        ],
    ))
}

/// Enabled `[channels.plugin.<alias>]` declarations whose package is an
/// installed channel plugin; turning `plugins.enabled` on brings each up once
/// an enabled agent routes to it.
fn channel_bindings(config: &Config, host: &PluginHost) -> Vec<ChannelBinding> {
    config
        .channels
        .plugin
        .iter()
        .filter(|(_, declaration)| declaration.enabled)
        .filter(|(_, declaration)| {
            host.manifest(&declaration.package)
                .is_some_and(|manifest| manifest.capabilities.contains(&PluginCapability::Channel))
        })
        .map(|(alias, declaration)| ChannelBinding {
            alias: alias.clone(),
            package: declaration.package.clone(),
        })
        .collect()
}

fn activated_line(instance: &ActivatedInstance) -> String {
    match instance {
        ActivatedInstance::Tool { package } => qta(
            "cli-quickstart-plugins-activation-tool",
            &[("name", &terminal_safe(package))],
        ),
        ActivatedInstance::Skill { package } => qta(
            "cli-quickstart-plugins-activation-skill",
            &[("name", &terminal_safe(package))],
        ),
        ActivatedInstance::Channel { alias, package } => qta(
            "cli-quickstart-plugins-activation-channel",
            &[
                ("alias", &terminal_safe(alias)),
                ("name", &terminal_safe(package)),
            ],
        ),
    }
}

/// Ask once, after every package was handled, whether to turn plugin
/// activation on, listing every instance that would become active, including
/// packages installed before this run. Returns whether the flags changed.
///
/// The list is what a daemon would load: the plugins directory, discovered
/// afresh, rather than `host`, the host this run installed through. The two
/// can differ. A publish whose rollback failed leaves its package on disk
/// after the host dropped it from its loaded set, and the package activates
/// at the next start all the same. When the directory cannot be discovered,
/// the list falls back to `host`, may miss packages, and the answer defaults
/// to no.
async fn activation_consent<P: QuickstartPrompter>(
    config: &mut Config,
    host: &PluginHost,
    outcomes: &[PackageOutcome],
    log: &PhaseLog,
    prompter: &mut P,
) -> PromptResult<bool> {
    let (installed, bindings, undiscovered) = {
        let discovered = crate::plugin_host_with_configured_security(config);
        let (listing, undiscovered) = match &discovered {
            Ok(fresh) => (fresh, None),
            Err(error) => (host, Some(terminal_safe_detail(&format!("{error:#}")))),
        };
        (
            listing.list_plugins(),
            channel_bindings(config, listing),
            undiscovered,
        )
    };
    let preview = activation_preview(&ActivationInputs {
        plugins_enabled: config.plugins.enabled,
        auto_discover: config.plugins.auto_discover,
        installed: &installed,
        channel_bindings: &bindings,
    });
    if preview.changes_nothing() {
        return Ok(false);
    }

    let mut settings = Vec::new();
    if preview.enable_plugins {
        settings.push("plugins.enabled");
    }
    if preview.enable_auto_discover {
        settings.push("plugins.auto_discover");
    }
    prompter.say("");
    prompter.say(&qta(
        "cli-quickstart-plugins-activation-heading",
        &[("settings", &settings.join(", "))],
    ));
    for instance in &preview.activated {
        prompter.say(&indented(&activated_line(instance)));
    }
    // Only the packages the operator went ahead with count as this run's
    // selection. One whose row they skipped is still installed, so the
    // preview lists it, and waking it has to be opted into.
    let selected: BTreeSet<String> = outcomes
        .iter()
        .filter(|outcome| outcome.is_accepted())
        .map(|outcome| outcome.name().to_string())
        .collect();
    // A list that may be incomplete may activate other packages too.
    let only_selected = undiscovered.is_none() && preview.activates_only(&selected);
    match &undiscovered {
        Some(error) => prompter.say(&qta(
            "cli-quickstart-plugins-activation-unverified",
            &[("error", error)],
        )),
        None if !only_selected => {
            prompter.say(&qta("cli-quickstart-plugins-activation-others", &[]));
        }
        None => {}
    }
    let accepted = prompter.confirm(
        &qta("cli-quickstart-plugins-activation-prompt", &[]),
        only_selected,
    )? == Some(true);
    log.activation(&preview, accepted);
    let commands: Vec<String> = settings
        .iter()
        .map(|path| config_set_command(config, path, "true"))
        .collect();
    if !accepted {
        prompter.say(&qta("cli-quickstart-plugins-activation-declined", &[]));
        for command in &commands {
            prompter.say(&indented(command));
        }
        return Ok(false);
    }

    let mut staged = config.clone();
    let written: anyhow::Result<()> = async {
        for path in &settings {
            staged.set_prop_persistent(path, "true")?;
        }
        Box::pin(staged.save_dirty()).await
    }
    .await;
    match written {
        Ok(()) => {
            *config = staged;
            prompter.say(&qta("cli-quickstart-plugins-activation-enabled", &[]));
            Ok(true)
        }
        Err(error) => {
            prompter.say(&qta(
                "cli-quickstart-plugins-activation-save-failed",
                &[("error", &terminal_safe_detail(&format!("{error:#}")))],
            ));
            for command in &commands {
                prompter.say(&indented(command));
            }
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::time::Duration;

    use sha2::Digest as _;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw::plugins::catalog::package_catalog;
    use zeroclaw::plugins::registry::{PluginRegistryEntry, PluginRegistryIndex};
    use zeroclaw_runtime::quickstart::QuickstartStep;

    use crate::tests::tool_component_fixture;

    const FIXTURE_NAME: &str = "tool-fixture";
    const FIXTURE_VERSION: &str = "0.1.0";
    const ARCHIVE_PATH: &str = "/tool-fixture-0.1.0.zip";
    const INDEX_PATH: &str = "/registry.json";
    const DECLARED_HOST: &str = "api.example.com";
    /// A value no output line or log event may ever contain.
    const SECRET: &str = "tok-9f3a-never-print-me";

    /// The package the registry serves: the in-tree tool component under a
    /// manifest that requests `http_client` with a declared destination and
    /// `config_read` with a schema holding a secret, a required string and an
    /// optional integer.
    const FIXTURE_MANIFEST: &str = r#"name = "tool-fixture"
version = "0.1.0"
description = "Echo tool for the Quickstart plugin tests."
author = "ZeroClaw tests"
wasm_path = "tool-fixture.wasm"
capabilities = ["tool"]
permissions = ["http_client", "config_read"]

[config_schema]
"$schema" = "https://json-schema.org/draft/2020-12/schema"
type = "object"
additionalProperties = false
required = ["api_token", "label"]

[config_schema.properties.api_token]
type = "string"
x-secret = true

[config_schema.properties.label]
type = "string"
description = "Label shown in every echo."

[config_schema.properties.max_len]
type = "integer"
minimum = 0

[egress]
hosts = ["api.example.com"]
"#;

    /// The zipped package, laid out as a registry release ships it, around the
    /// in-tree tool component the binary's plugin tests share.
    fn archive() -> &'static [u8] {
        static ARCHIVE: OnceLock<Vec<u8>> = OnceLock::new();
        ARCHIVE.get_or_init(|| {
            use std::io::Write as _;
            let wasm =
                std::fs::read(tool_component_fixture::wasm()).expect("read the fixture component");
            let mut bytes = std::io::Cursor::new(Vec::new());
            {
                let mut writer = zip::ZipWriter::new(&mut bytes);
                let options = zip::write::SimpleFileOptions::default();
                writer
                    .start_file("tool-fixture/manifest.toml", options)
                    .expect("zip manifest");
                writer
                    .write_all(FIXTURE_MANIFEST.as_bytes())
                    .expect("zip manifest bytes");
                writer
                    .start_file("tool-fixture/tool-fixture.wasm", options)
                    .expect("zip component");
                writer.write_all(&wasm).expect("zip component bytes");
                writer.finish().expect("finish zip");
            }
            bytes.into_inner()
        })
    }

    fn archive_digest() -> String {
        hex::encode(sha2::Sha256::digest(archive()))
    }

    fn fixture_entry(server: &MockServer, sha256: Option<String>) -> PluginRegistryEntry {
        PluginRegistryEntry {
            name: FIXTURE_NAME.to_string(),
            version: FIXTURE_VERSION.to_string(),
            description: Some("Echo tool".to_string()),
            author: None,
            capabilities: vec!["tool".to_string()],
            url: format!("{}{ARCHIVE_PATH}", server.uri()),
            sha256,
        }
    }

    /// The Plugins row's selection for `entry`, built the way the row builds
    /// it.
    fn selection(entry: PluginRegistryEntry) -> Vec<PluginChoice> {
        let index = PluginRegistryIndex {
            plugins: vec![entry],
            registry_url: None,
        };
        let choices = plugin_choices(&package_catalog(&[], Some(&index)));
        assert_eq!(choices.len(), 1, "the fixture is offered");
        choices
    }

    async fn serve_archive(server: &MockServer, response: ResponseTemplate, hits: u64) {
        Mock::given(method("GET"))
            .and(path(ARCHIVE_PATH))
            .respond_with(response)
            .expect(hits)
            .mount(server)
            .await;
    }

    async fn serve_valid_archive(server: &MockServer, hits: u64) {
        serve_archive(
            server,
            ResponseTemplate::new(200).set_body_bytes(archive().to_vec()),
            hits,
        )
        .await;
    }

    fn fixture_instance_key() -> String {
        instance_key_of(FIXTURE_MANIFEST)
    }

    /// The command that creates the missing row `key`, built independently
    /// of the step: the row it creates holds an empty grant.
    fn empty_grant_create_command(config: &Config, key: &str) -> String {
        let command = egress_create_command(crate::egress_command_config_dir(config), key, &[]);
        assert!(
            command.contains(r#""value":[]"#),
            "the created row grants nothing: {command}"
        );
        command
    }

    /// The separate line that grants `name`'s one declared destination,
    /// [`DECLARED_HOST`], on the row `key` once it exists.
    fn declared_grant_line_for(config: &Config, name: &str, key: &str) -> String {
        let command = egress_set_command(
            crate::egress_command_config_dir(config),
            key,
            &[DECLARED_HOST.to_string()],
        );
        qta(
            "cli-quickstart-plugins-grant-declared-later",
            &[("name", name), ("count", "1"), ("command", &command)],
        )
    }

    /// The config row key of the tool instance `manifest` describes.
    fn instance_key_of(manifest: &str) -> String {
        let manifest: PluginManifest = toml::from_str(manifest).expect("the manifest parses");
        PluginInstanceScope::for_package_binding(
            &manifest,
            PluginCapability::Tool,
            std::iter::empty(),
        )
        .expect("scope derives")
        .id()
        .config_entry_key()
        .expect("instance key derives")
    }

    /// A throwaway ZeroClaw home: config file, data directory and plugins
    /// directory all inside one temporary directory.
    struct Workspace {
        dir: tempfile::TempDir,
        config: Config,
    }

    impl Workspace {
        /// The config file starts as a current-schema stub, so every save the
        /// phase makes takes the incremental path production takes.
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("workspace");
            let mut config = Config::default();
            config.config_path = dir.path().join("config.toml");
            config.data_dir = dir.path().join("data");
            config.plugins.plugins_dir = dir.path().join("plugins").display().to_string();
            config.secrets.encrypt = true;
            std::fs::write(
                &config.config_path,
                format!(
                    "schema_version = {}\n",
                    crate::config::migration::CURRENT_SCHEMA_VERSION
                ),
            )
            .expect("seed the config file");
            Self { dir, config }
        }

        fn plugins_dir(&self) -> PathBuf {
            self.dir.path().join("plugins")
        }

        fn package_dir(&self) -> PathBuf {
            self.plugins_dir().join(FIXTURE_NAME)
        }

        fn installed_packages(&self) -> Vec<String> {
            match std::fs::read_dir(self.plugins_dir()) {
                Ok(entries) => entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect(),
                Err(_) => Vec::new(),
            }
        }

        fn config_bytes(&self) -> Vec<u8> {
            std::fs::read(&self.config.config_path).expect("read config.toml")
        }

        fn config_on_disk(&self) -> toml::Table {
            toml::from_str(&String::from_utf8_lossy(&self.config_bytes()))
                .expect("config.toml stays valid TOML")
        }

        fn row(&self, key: &str) -> Option<&crate::config::schema::PluginEntryConfig> {
            self.config
                .plugins
                .entries
                .iter()
                .find(|entry| entry.name == key)
        }
    }

    fn row_on_disk(table: &toml::Table, key: &str) -> Option<toml::Table> {
        table
            .get("plugins")?
            .get("entries")?
            .as_array()?
            .iter()
            .find(|row| row.get("name").and_then(toml::Value::as_str) == Some(key))?
            .as_table()
            .cloned()
    }

    fn flag_on_disk(table: &toml::Table, flag: &str) -> bool {
        table
            .get("plugins")
            .and_then(|plugins| plugins.get(flag))
            .and_then(toml::Value::as_bool)
            .unwrap_or(false)
    }

    #[derive(Debug)]
    enum Answer {
        Select(Option<usize>),
        MultiSelect(Option<Vec<usize>>),
        Confirm(Option<bool>),
        Field(Option<String>),
        Interrupt,
    }

    /// Answers the Plugins step's questions from a script, in order, and keeps
    /// every line it was asked to print.
    #[derive(Default)]
    struct ScriptedPrompter {
        answers: VecDeque<Answer>,
        said: Vec<String>,
        fields: Vec<FieldDescriptor>,
        confirm_defaults: Vec<bool>,
        select_defaults: Vec<usize>,
        choice_lists: Vec<Vec<String>>,
    }

    impl ScriptedPrompter {
        fn new(answers: impl IntoIterator<Item = Answer>) -> Self {
            Self {
                answers: answers.into_iter().collect(),
                ..Self::default()
            }
        }

        fn next(&mut self, prompt: &str) -> Answer {
            self.answers
                .pop_front()
                .unwrap_or_else(|| panic!("no scripted answer for {prompt:?}"))
        }

        fn output(&self) -> String {
            self.said.join("\n")
        }

        fn assert_done(&self) {
            assert!(
                self.answers.is_empty(),
                "unused scripted answers: {:?}",
                self.answers
            );
        }
    }

    impl QuickstartPrompter for ScriptedPrompter {
        fn select(
            &mut self,
            prompt: &str,
            items: &[String],
            default: usize,
        ) -> PromptResult<Option<usize>> {
            self.choice_lists.push(items.to_vec());
            self.select_defaults.push(default);
            match self.next(prompt) {
                Answer::Select(answer) => Ok(answer),
                Answer::Interrupt => Err(PromptError::Interrupted),
                other => panic!("{prompt:?} is a select; the script answered {other:?}"),
            }
        }

        fn multi_select(
            &mut self,
            prompt: &str,
            items: &[String],
            _checked: &[bool],
        ) -> PromptResult<Option<Vec<usize>>> {
            self.choice_lists.push(items.to_vec());
            match self.next(prompt) {
                Answer::MultiSelect(answer) => Ok(answer),
                Answer::Interrupt => Err(PromptError::Interrupted),
                other => panic!("{prompt:?} is a multi-select; the script answered {other:?}"),
            }
        }

        fn confirm(&mut self, prompt: &str, default: bool) -> PromptResult<Option<bool>> {
            self.confirm_defaults.push(default);
            match self.next(prompt) {
                Answer::Confirm(answer) => Ok(answer),
                Answer::Interrupt => Err(PromptError::Interrupted),
                other => panic!("{prompt:?} is a confirm; the script answered {other:?}"),
            }
        }

        fn field(&mut self, field: &FieldDescriptor) -> PromptResult<Option<String>> {
            self.fields.push(field.clone());
            match self.next(&field.key) {
                Answer::Field(answer) => Ok(answer),
                Answer::Interrupt => Err(PromptError::Interrupted),
                other => panic!("{:?} is a field; the script answered {other:?}", field.key),
            }
        }

        fn say(&mut self, line: &str) {
            self.said.push(line.to_string());
        }
    }

    /// The answers that install the fixture with a valid configuration: the
    /// egress decision, the two required settings, and no optional ones.
    fn install_answers(egress: usize) -> Vec<Answer> {
        vec![
            Answer::Select(Some(egress)),
            Answer::Field(Some(SECRET.to_string())),
            Answer::Field(Some("demo".to_string())),
            Answer::Confirm(Some(false)),
        ]
    }

    const GRANT: usize = 0;
    const WITHHOLD: usize = 1;
    const SKIP: usize = 2;

    async fn run(
        workspace: &mut Workspace,
        selection: &[PluginChoice],
        prompter: &mut ScriptedPrompter,
    ) -> Result<CreatePhase, PhaseHalt> {
        run_with_timeouts(workspace, selection, RegistryTimeouts::default(), prompter).await
    }

    async fn run_with_timeouts(
        workspace: &mut Workspace,
        selection: &[PluginChoice],
        timeouts: RegistryTimeouts,
        prompter: &mut ScriptedPrompter,
    ) -> Result<CreatePhase, PhaseHalt> {
        let registry = RegistryClient::new(timeouts).expect("registry client");
        // Every run starts with the agent step's dry run, as Create does, for
        // a submission the agent step accepts.
        Box::pin(create_phase_with(
            &mut workspace.config,
            selection,
            &submission("bot"),
            &registry,
            prompter,
        ))
        .await
    }

    /// A checklist submission for an agent named `agent`, with a fresh
    /// provider, the built-in presets and SQLite memory. The agent step
    /// accepts it on a fresh workspace unless `agent` is empty.
    fn submission(agent: &str) -> BuilderSubmission {
        use zeroclaw_config::presets::{
            AgentIdentity, MemoryChoice, ModelProviderChoice, SelectorChoice,
        };
        BuilderSubmission {
            model_provider: SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: "anthropic".to_string(),
                alias: "anthropic".to_string(),
                model: "claude-sonnet-4-5".to_string(),
                fields: HashMap::from([("api_key".to_string(), "sk-test".to_string())]),
            }),
            risk_profile: SelectorChoice::Fresh("balanced".to_string()),
            runtime_profile: SelectorChoice::Fresh("balanced".to_string()),
            memory: SelectorChoice::Fresh(MemoryChoice::Sqlite),
            channels: Vec::new(),
            peer_groups: Vec::new(),
            agent: AgentIdentity {
                name: agent.to_string(),
                system_prompt: "You are helpful.".to_string(),
                personality_file: None,
                personality_files: Vec::new(),
            },
        }
    }

    fn verdict(config: &Config) -> ToolInstanceAdmission {
        verdict_for(config, FIXTURE_NAME)
    }

    fn verdict_for(config: &Config, package: &str) -> ToolInstanceAdmission {
        let host = crate::plugin_host_with_configured_security(config).expect("host builds");
        zeroclaw_runtime::plugin_runtime::tool_instance_admission(config, &host, package)
            .expect("the activation plan builds")
    }

    /// The prefix of the line that reports a row the resolver rejects, up to
    /// the resolver's reason.
    fn rejected_prefix(name: &str) -> String {
        let line = qta(
            "cli-quickstart-plugins-ready-rejected",
            &[("name", name), ("error", "<error>")],
        );
        let (prefix, _) = line
            .split_once("<error>")
            .expect("the line carries the resolver's reason");
        prefix.to_string()
    }

    /// The fixture's status after Create when its row lacks both required
    /// settings: never active, the resolver's reason once, the two settings
    /// named, and one command per setting that carries no value.
    fn assert_reported_missing(readiness: &[String], key: &str) {
        let text = readiness.join("\n");
        assert!(
            !text.contains(&qta(
                "cli-quickstart-plugins-ready",
                &[("name", FIXTURE_NAME), ("key", key)]
            )),
            "an instance the resolver rejects is not reported active: {text}"
        );
        assert_eq!(
            readiness
                .iter()
                .filter(|line| line
                    .trim_start()
                    .starts_with(&rejected_prefix(FIXTURE_NAME)))
                .count(),
            1,
            "the resolver's reason is shown once: {text}"
        );
        assert!(
            text.contains(&qta(
                "cli-quickstart-plugins-ready-missing-settings",
                &[("name", FIXTURE_NAME), ("keys", "api_token, label")]
            )),
            "{text}"
        );
        for setting in ["api_token", "label"] {
            let path = format!("config set plugins.entries.{key}.config.{setting}");
            assert!(
                readiness
                    .iter()
                    .any(|line| line.contains("--config-dir") && line.ends_with(&path)),
                "one command per setting, with no value on it: {text}"
            );
        }
    }

    #[tokio::test]
    async fn granting_the_declaration_installs_seeds_configures_and_activates() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 1).await;
        let mut workspace = Workspace::new();
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        let mut answers = install_answers(GRANT);
        answers.push(Answer::Confirm(Some(true)));
        let mut prompter = ScriptedPrompter::new(answers);

        let phase = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");

        prompter.assert_done();
        assert_eq!(
            phase.outcomes,
            vec![PackageOutcome::Installed {
                name: FIXTURE_NAME.to_string(),
            }]
        );
        assert!(phase.activation_changed);
        assert_eq!(
            prompter.choice_lists[0].len(),
            3,
            "grant, withhold or skip is offered for a declared destination"
        );
        assert_eq!(
            prompter.select_defaults,
            vec![WITHHOLD],
            "the question starts closed, on installing without network access"
        );
        assert!(
            workspace.package_dir().join("manifest.toml").is_file(),
            "the package is published into the plugins directory"
        );

        let key = fixture_instance_key();
        let row = workspace.row(&key).expect("the instance row is seeded");
        assert_eq!(row.egress_hosts, vec![DECLARED_HOST.to_string()]);
        let mut keys: Vec<&str> = row.config.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["api_token", "label"]);
        assert_eq!(
            row.config.get("label").map(String::as_str),
            Some("demo"),
            "a string setting is stored exactly as typed"
        );
        let secret_field = &prompter.fields[0];
        assert_eq!(secret_field.key, "api_token");
        assert!(secret_field.is_secret, "an x-secret property is masked");

        let on_disk = workspace.config_on_disk();
        let row = row_on_disk(&on_disk, &key).expect("the row is persisted");
        assert_eq!(
            row.get("egress_hosts")
                .and_then(toml::Value::as_array)
                .map(|hosts| hosts
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .collect::<Vec<_>>()),
            Some(vec![DECLARED_HOST]),
            "the grant is plaintext on disk"
        );
        let stored_token = row
            .get("config")
            .and_then(|config| config.get("api_token"))
            .and_then(toml::Value::as_str)
            .expect("the secret setting is persisted");
        assert!(
            stored_token.starts_with("enc2:"),
            "settings are encrypted at rest"
        );
        let raw = String::from_utf8_lossy(&workspace.config_bytes()).into_owned();
        assert!(
            !raw.contains(SECRET),
            "the secret never reaches disk in clear"
        );
        assert!(flag_on_disk(&on_disk, "enabled") && flag_on_disk(&on_disk, "auto_discover"));
        assert_eq!(
            prompter.confirm_defaults.last(),
            Some(&true),
            "activation defaults to yes when it activates only this run's selection"
        );

        // Redaction: the configured secret is named, never shown.
        let output = prompter.output();
        assert!(
            output.contains("api_token"),
            "the output names the setting: {output}"
        );
        assert!(
            !output.contains(SECRET),
            "the output never carries its value: {output}"
        );

        assert_eq!(
            verdict(&workspace.config),
            ToolInstanceAdmission::Admitted {
                instance_key: key.clone()
            }
        );
        let readiness = phase.readiness_lines(&workspace.config).await.join("\n");
        assert!(
            readiness.contains(&qta(
                "cli-quickstart-plugins-ready",
                &[("name", FIXTURE_NAME), ("key", &key)]
            )),
            "a configured, admitted instance is reported active: {readiness}"
        );
        // Quickstart never signals a running daemon: the closing note names
        // the service restart, addressed to the configuration this run wrote.
        let restart = zeroclaw_command(&workspace.config, "service restart");
        assert!(
            restart.contains("--config-dir") && readiness.contains(&restart),
            "the restart note carries the service restart command: {readiness}"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn withholding_network_access_seeds_an_empty_grant_and_declining_keeps_plugins_off() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 1).await;
        let mut workspace = Workspace::new();
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        let mut answers = install_answers(WITHHOLD);
        answers.push(Answer::Confirm(Some(false)));
        let mut prompter = ScriptedPrompter::new(answers);

        let phase = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");

        prompter.assert_done();
        assert_eq!(
            phase.outcomes,
            vec![PackageOutcome::Installed {
                name: FIXTURE_NAME.to_string(),
            }]
        );
        assert!(!phase.activation_changed);
        let key = fixture_instance_key();
        let row = workspace
            .row(&key)
            .expect("the instance row is still created");
        assert!(
            row.egress_hosts.is_empty(),
            "a withheld grant reaches nothing: {:?}",
            row.egress_hosts
        );
        assert!(workspace.package_dir().is_dir());

        let on_disk = workspace.config_on_disk();
        assert!(!flag_on_disk(&on_disk, "enabled"));
        assert!(!flag_on_disk(&on_disk, "auto_discover"));
        assert!(!workspace.config.plugins.enabled && !workspace.config.plugins.auto_discover);
        let output = prompter.output();
        assert!(
            output.contains("config set plugins.enabled true")
                && output.contains("config set plugins.auto_discover true"),
            "declining prints the exact activation commands: {output}"
        );
        assert_eq!(
            verdict(&workspace.config),
            ToolInstanceAdmission::PluginsDisabled
        );

        // With only discovery left off, the verdict names that gate instead.
        workspace.config.plugins.enabled = true;
        assert_eq!(
            verdict(&workspace.config),
            ToolInstanceAdmission::AutoDiscoverDisabled
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn skipping_a_plugin_installs_and_writes_nothing() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 1).await;
        let mut workspace = Workspace::new();
        let before = workspace.config_bytes();
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        let mut prompter = ScriptedPrompter::new([Answer::Select(Some(SKIP))]);

        let phase = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");

        prompter.assert_done();
        assert_eq!(
            phase.outcomes,
            vec![PackageOutcome::Skipped {
                name: FIXTURE_NAME.to_string()
            }]
        );
        assert!(workspace.installed_packages().is_empty());
        assert!(workspace.config.plugins.entries.is_empty());
        assert_eq!(workspace.config_bytes(), before);
        assert!(
            prompter.confirm_defaults.is_empty(),
            "nothing installed, so activation is not offered"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn an_entry_without_a_digest_is_refused_before_the_archive_is_requested() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 0).await;
        let mut workspace = Workspace::new();
        let before = workspace.config_bytes();
        let selection = selection(fixture_entry(&server, None));
        let mut prompter = ScriptedPrompter::new([]);

        let phase = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");

        assert_eq!(
            phase.outcomes,
            vec![PackageOutcome::Refused {
                name: FIXTURE_NAME.to_string(),
                reason: Refusal::NoIntegrityHash,
            }]
        );
        assert!(workspace.installed_packages().is_empty());
        assert_eq!(workspace.config_bytes(), before);
        assert!(
            server
                .received_requests()
                .await
                .expect("request recording is on")
                .is_empty(),
            "the archive must never be requested"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn a_bad_digest_a_server_error_or_a_stalled_download_changes_nothing() {
        // Only the stalled download runs against a short read bound, the one
        // that ends a stall. The other two fail on their own, and each has to
        // say why rather than pass by timing out under load.
        let stall_bound = RegistryTimeouts {
            read: Duration::from_millis(300),
            ..RegistryTimeouts::default()
        };
        /// One failed download: its name, the archive response, the digest
        /// the registry entry carries, the request bounds, and the cause the
        /// report must name.
        type DownloadCase = (
            &'static str,
            ResponseTemplate,
            Option<String>,
            RegistryTimeouts,
            &'static str,
        );
        let cases: [DownloadCase; 3] = [
            (
                "digest mismatch",
                ResponseTemplate::new(200).set_body_bytes(archive().to_vec()),
                Some("0".repeat(64)),
                RegistryTimeouts::default(),
                "sha256 mismatch",
            ),
            (
                "HTTP 500",
                ResponseTemplate::new(500),
                Some(archive_digest()),
                RegistryTimeouts::default(),
                "HTTP 500",
            ),
            (
                "stalled",
                ResponseTemplate::new(200)
                    .set_body_bytes(archive().to_vec())
                    .set_delay(Duration::from_secs(10)),
                Some(archive_digest()),
                stall_bound,
                "timed out",
            ),
        ];
        for (case, response, digest, timeouts, cause) in cases {
            let server = MockServer::start().await;
            serve_archive(&server, response, 1).await;
            let mut workspace = Workspace::new();
            let before = workspace.config_bytes();
            let selection = selection(fixture_entry(&server, digest));
            let mut prompter = ScriptedPrompter::new([]);

            let phase = run_with_timeouts(&mut workspace, &selection, timeouts, &mut prompter)
                .await
                .expect("the phase completes");

            assert_eq!(
                phase.outcomes,
                vec![PackageOutcome::Failed {
                    name: FIXTURE_NAME.to_string(),
                    stage: FailureStage::Download,
                }],
                "{case}"
            );
            let output = prompter.output();
            assert!(
                output.contains(cause),
                "{case}: the failure names its cause: {output}"
            );
            assert!(workspace.installed_packages().is_empty(), "{case}");
            assert!(workspace.config.plugins.entries.is_empty(), "{case}");
            assert_eq!(workspace.config_bytes(), before, "{case}");
            server.verify().await;
        }
    }

    #[tokio::test]
    async fn a_package_that_does_not_load_names_the_install_that_accepts_it() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 1).await;
        let mut workspace = Workspace::new();
        // Fewer instances than the component needs, so the load check that
        // `plugin install` runs refuses it.
        workspace.config.plugins.limits.max_instances = 1;
        let before = workspace.config_bytes();
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        let mut prompter = ScriptedPrompter::new([]);

        let phase = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");

        assert_eq!(
            phase.outcomes,
            vec![PackageOutcome::Failed {
                name: FIXTURE_NAME.to_string(),
                stage: FailureStage::LoadCheck,
            }]
        );
        let output = prompter.output();
        let install_anyway =
            zeroclaw_command(&workspace.config, "plugin install tool-fixture --no-verify");
        assert!(
            install_anyway.contains("--config-dir") && output.contains(&install_anyway),
            "the refusal names the install that accepts the package anyway, addressed to \
             this configuration: {output}"
        );
        assert!(workspace.installed_packages().is_empty());
        assert!(workspace.config.plugins.entries.is_empty());
        assert_eq!(workspace.config_bytes(), before);
        server.verify().await;
    }

    #[tokio::test]
    async fn running_the_same_selection_twice_downloads_once_and_rewrites_nothing() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 1).await;
        let mut workspace = Workspace::new();
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        let mut answers = install_answers(GRANT);
        answers.push(Answer::Confirm(Some(true)));
        let mut first = ScriptedPrompter::new(answers);
        run(&mut workspace, &selection, &mut first)
            .await
            .expect("the first run completes");
        first.assert_done();
        let after_first = workspace.config_bytes();

        // The same selection, still marked available: the plugins directory
        // decides, so nothing is downloaded, prompted or written again.
        let mut second = ScriptedPrompter::new([]);
        let phase = run(&mut workspace, &selection, &mut second)
            .await
            .expect("the second run completes");

        assert_eq!(
            phase.outcomes,
            vec![PackageOutcome::AlreadyInstalled {
                name: FIXTURE_NAME.to_string(),
                seeded_row: false,
            }]
        );
        assert_eq!(workspace.config_bytes(), after_first);
        assert_eq!(
            workspace.config.plugins.entries.len(),
            1,
            "one row, never two"
        );
        server.verify().await;
    }

    /// Publish the fixture into the workspace's plugins directory with no
    /// config row, the state a Ctrl+C between the publish and the seeding of
    /// an earlier install leaves behind.
    fn install_without_a_row(workspace: &Workspace) {
        install_with_manifest(workspace, FIXTURE_NAME, FIXTURE_MANIFEST);
    }

    /// Publish the in-tree tool component as package `name` under
    /// `manifest`, whose `wasm_path` is `tool-fixture.wasm`, with no config
    /// row.
    fn install_with_manifest(workspace: &Workspace, name: &str, manifest: &str) {
        let source = workspace.dir.path().join("source").join(name);
        std::fs::create_dir_all(&source).expect("package source directory");
        std::fs::copy(
            tool_component_fixture::wasm(),
            source.join("tool-fixture.wasm"),
        )
        .expect("copy the component");
        std::fs::write(source.join("manifest.toml"), manifest).expect("write the manifest");
        let mut host =
            crate::plugin_host_with_configured_security(&workspace.config).expect("plugin host");
        host.install(source.to_str().expect("UTF-8 temporary path"))
            .expect("install the package");
    }

    #[tokio::test]
    async fn an_installed_package_missing_its_row_gets_the_fresh_install_decision() {
        for (decision, skipped, granted) in [
            (GRANT, false, Some(vec![DECLARED_HOST.to_string()])),
            (WITHHOLD, false, Some(Vec::new())),
            (SKIP, true, None),
        ] {
            let server = MockServer::start().await;
            serve_valid_archive(&server, 0).await;
            let mut workspace = Workspace::new();
            install_without_a_row(&workspace);
            let before = workspace.config_bytes();
            let selection = selection(fixture_entry(&server, Some(archive_digest())));
            // The egress decision, then activation declined.
            let mut prompter = ScriptedPrompter::new([
                Answer::Select(Some(decision)),
                Answer::Confirm(Some(false)),
            ]);

            let phase = run(&mut workspace, &selection, &mut prompter)
                .await
                .expect("the phase completes");

            prompter.assert_done();
            let expected = if skipped {
                PackageOutcome::AlreadyInstalledSkipped {
                    name: FIXTURE_NAME.to_string(),
                }
            } else {
                PackageOutcome::AlreadyInstalled {
                    name: FIXTURE_NAME.to_string(),
                    seeded_row: true,
                }
            };
            assert_eq!(phase.outcomes, vec![expected], "decision {decision}");
            // Activation still wakes the installed package. After a skip that
            // is not what the operator went ahead with, so it is opted into.
            assert_eq!(
                prompter.confirm_defaults.last(),
                Some(&!skipped),
                "decision {decision}: the activation default"
            );
            assert_eq!(
                prompter.choice_lists[0].len(),
                3,
                "the fresh-install question is asked"
            );
            assert_eq!(
                prompter.select_defaults,
                vec![WITHHOLD],
                "the question starts closed, as for a fresh install"
            );
            let output = prompter.output();
            assert!(
                output.contains(&qta(
                    "cli-quickstart-plugins-about-destinations",
                    &[("list", DECLARED_HOST)]
                )),
                "the package summary comes first: {output}"
            );
            let key = fixture_instance_key();
            assert_eq!(
                workspace.row(&key).map(|row| row.egress_hosts.clone()),
                granted,
                "decision {decision}"
            );
            if skipped {
                assert_eq!(
                    workspace.config_bytes(),
                    before,
                    "a skip leaves the row absent"
                );
                assert_eq!(
                    phase.apply_failed_headline(&workspace.config),
                    None,
                    "a skipped package is not reported as installed or configured \
                     by this run"
                );
                // The skip hands back the command that creates the row, which
                // grants nothing the operator just declined, and, as its own
                // line, the command that grants the declared destination.
                let create = empty_grant_create_command(&workspace.config, &key);
                let grant = declared_grant_line_for(&workspace.config, FIXTURE_NAME, &key);
                let skipped_line = indented(&qta(
                    "cli-quickstart-plugins-row-skipped",
                    &[("name", FIXTURE_NAME), ("command", &create)],
                ));
                let skip_lines = &prompter.said;
                let at = skip_lines
                    .iter()
                    .position(|line| *line == skipped_line)
                    .unwrap_or_else(|| panic!("the skip line creates an empty grant: {output}"));
                assert_eq!(
                    skip_lines.get(at + 1),
                    Some(&indented(&grant)),
                    "the grant follows as a separate line: {output}"
                );
                // The status reads the absent row: it gives the same two
                // commands, and no setting command, since `config set` only
                // resolves rows that exist.
                let readiness = phase.readiness_lines(&workspace.config).await;
                let no_row = indented(&qta(
                    "cli-quickstart-plugins-ready-no-row",
                    &[("name", FIXTURE_NAME), ("command", &create)],
                ));
                let at = readiness
                    .iter()
                    .position(|line| *line == no_row)
                    .unwrap_or_else(|| {
                        panic!(
                            "the status names the missing row and how to create it: {readiness:?}"
                        )
                    });
                assert_eq!(
                    readiness.get(at + 1),
                    Some(&indented(&grant)),
                    "{readiness:?}"
                );
                let readiness = readiness.join("\n");
                assert!(
                    !readiness.contains(&format!("plugins.entries.{key}.config.")),
                    "no setting command for a row that does not exist: {readiness}"
                );
            } else {
                assert!(
                    row_on_disk(&workspace.config_on_disk(), &key).is_some(),
                    "decision {decision}: the row is persisted"
                );
            }
            server.verify().await;
        }
    }

    #[tokio::test]
    async fn a_tool_without_a_config_row_is_reported_by_what_its_manifest_needs() {
        // A tool that requires no setting but requests network access: install
        // owes it a row, where the operator's grant lives.
        let networked = r#"name = "net-tool"
version = "0.1.0"
wasm_path = "tool-fixture.wasm"
capabilities = ["tool"]
permissions = ["http_client"]

[egress]
hosts = ["api.example.com"]
"#;
        // Network access with no declared destination: the row is owed, and
        // there is no declaration to grant.
        let undeclared = r#"name = "open-tool"
version = "0.1.0"
wasm_path = "tool-fixture.wasm"
capabilities = ["tool"]
permissions = ["http_client"]
"#;
        // A declaration without `http_client`, the one transport it counts
        // with: the row is owed, and nothing is granted from it.
        let offline = r#"name = "offline-tool"
version = "0.1.0"
wasm_path = "tool-fixture.wasm"
capabilities = ["tool"]

[egress]
hosts = ["api.example.com"]
"#;
        // A tool with nothing to configure and no network access: install
        // seeds it no row, and it needs none.
        let stateless = r#"name = "pure-tool"
version = "0.1.0"
wasm_path = "tool-fixture.wasm"
capabilities = ["tool"]
"#;
        let packages = [
            ("net-tool", networked),
            ("open-tool", undeclared),
            ("offline-tool", offline),
            ("pure-tool", stateless),
        ];
        let mut workspace = Workspace::new();
        for (name, manifest) in packages {
            install_with_manifest(&workspace, name, manifest);
        }
        workspace.config.plugins.enabled = true;
        workspace.config.plugins.auto_discover = true;
        let phase = CreatePhase {
            outcomes: packages
                .iter()
                .map(|(name, _)| PackageOutcome::Installed {
                    name: (*name).to_string(),
                })
                .collect(),
            activation_changed: false,
        };

        let readiness = phase.readiness_lines(&workspace.config).await;

        let text = readiness.join("\n");
        for (name, manifest) in packages {
            let key = instance_key_of(manifest);
            assert_eq!(
                verdict_for(&workspace.config, name),
                ToolInstanceAdmission::Admitted {
                    instance_key: key.clone()
                }
            );
            assert!(
                !text.contains(&qta(
                    "cli-quickstart-plugins-ready",
                    &[("name", name), ("key", &key)]
                )),
                "{name}: the active line names a config entry that does not exist: {text}"
            );
            assert!(
                !text.contains(&format!("plugins.entries.{key}.config.")),
                "{name}: no setting command for a row that does not exist: {text}"
            );
        }

        // Each row the instance is owed comes with the command that creates
        // it with an empty grant. Only the manifest that declares a
        // destination `http_client` can reach adds the separate grant line.
        for (name, manifest, granted) in [
            ("net-tool", networked, true),
            ("open-tool", undeclared, false),
            ("offline-tool", offline, false),
        ] {
            let key = instance_key_of(manifest);
            let no_row = indented(&qta(
                "cli-quickstart-plugins-ready-no-row",
                &[
                    ("name", name),
                    (
                        "command",
                        &empty_grant_create_command(&workspace.config, &key),
                    ),
                ],
            ));
            let at = readiness
                .iter()
                .position(|line| *line == no_row)
                .unwrap_or_else(|| {
                    panic!("{name}: a missing row owed to the instance gets the command that creates it: {text}")
                });
            let grant_line = indented(&declared_grant_line_for(&workspace.config, name, &key));
            assert_eq!(
                readiness.get(at + 1) == Some(&grant_line),
                granted,
                "{name}: the grant line: {text}"
            );
            assert_eq!(
                text.contains(&format!("plugins.entries.{key}.egress_hosts")),
                granted,
                "{name}: a grant command only for a declaration `http_client` reaches: {text}"
            );
        }
        assert!(
            readiness.contains(&indented(&qta(
                "cli-quickstart-plugins-ready-no-entry-needed",
                &[("name", "pure-tool")]
            ))),
            "a tool with nothing to configure is active without a row: {text}"
        );
    }

    /// Run a printed command through `sh`, with `zeroclaw` replaced by a
    /// function that records its arguments and its standard input, and return
    /// both.
    #[cfg(unix)]
    fn run_printed_command(dir: &std::path::Path, command: &str) -> (Vec<String>, String) {
        let args_file = dir.join("captured-args");
        let stdin_file = dir.join("captured-stdin");
        let script = format!(
            "zeroclaw() {{ printf '%s\\n' \"$@\" > '{}'; cat > '{}'; }}\n{command}\n",
            args_file.display(),
            stdin_file.display()
        );
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .stdin(std::process::Stdio::null())
            .status()
            .expect("run sh");
        assert!(status.success(), "the printed command runs: {command}");
        let args = std::fs::read_to_string(&args_file)
            .expect("captured arguments")
            .lines()
            .map(str::to_string)
            .collect();
        let input = std::fs::read_to_string(&stdin_file).expect("captured input");
        (args, input)
    }

    /// The two commands a missing row is handed back with, run as printed and
    /// applied the way `config patch` and `config set` apply them: the first
    /// creates the row with no destination, and only the second, run after
    /// it, grants the declared one.
    #[tokio::test]
    #[cfg(unix)]
    async fn the_printed_create_command_grants_nothing_and_the_grant_is_its_own_command() {
        let mut workspace = Workspace::new();
        install_without_a_row(&workspace);
        let key = fixture_instance_key();
        let dir = crate::egress_command_config_dir(&workspace.config)
            .to_string_lossy()
            .into_owned();
        let egress_path = format!("plugins.entries.{key}.egress_hosts");
        let hosts_on_disk = |workspace: &Workspace| -> Option<Vec<String>> {
            row_on_disk(&workspace.config_on_disk(), &key).map(|row| {
                row.get("egress_hosts")
                    .and_then(toml::Value::as_array)
                    .map(|hosts| {
                        hosts
                            .iter()
                            .filter_map(toml::Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default()
            })
        };

        // The grant resolves only once the row exists.
        let mut early = workspace.config.clone();
        assert!(
            early
                .set_prop_persistent(&egress_path, DECLARED_HOST)
                .is_err(),
            "`config set` cannot write the grant of a row that does not exist"
        );

        let (args, input) = run_printed_command(
            workspace.dir.path(),
            &create_row_command(&workspace.config, &key),
        );
        assert_eq!(
            args,
            vec!["--config-dir", dir.as_str(), "config", "patch", "-"]
        );
        let ops: Vec<serde_json::Value> = serde_json::from_str(&input).expect("a JSON Patch");
        assert_eq!(ops.len(), 1, "{input}");
        assert_eq!(ops[0]["op"], "add", "{input}");
        assert_eq!(ops[0]["value"], serde_json::json!([]), "{input}");
        let path = ops[0]["path"]
            .as_str()
            .and_then(|pointer| pointer.strip_prefix('/'))
            .expect("a JSON Pointer path")
            .replace('/', ".");
        assert_eq!(path, egress_path);
        assert!(
            !workspace.config.ensure_map_or_list_key_for_path(&path),
            "the row key is creatable: {path}"
        );
        let value = crate::json_value_to_setprop_string(
            &ops[0]["value"],
            &workspace.config,
            &path,
            0,
            false,
        )
        .expect("the empty list converts like any patched value");
        workspace
            .config
            .set_prop_persistent(&path, &value)
            .expect("the patch applies");
        Box::pin(workspace.config.save_dirty())
            .await
            .expect("the patch saves");
        assert_eq!(
            hosts_on_disk(&workspace),
            Some(Vec::new()),
            "the created row exists and grants nothing"
        );

        let grant = egress_set_command(
            crate::egress_command_config_dir(&workspace.config),
            &key,
            &[DECLARED_HOST.to_string()],
        );
        let (args, _) = run_printed_command(workspace.dir.path(), &grant);
        assert_eq!(
            args,
            vec![
                "--config-dir",
                dir.as_str(),
                "config",
                "set",
                egress_path.as_str(),
                DECLARED_HOST
            ]
        );
        workspace
            .config
            .set_prop_persistent(&args[4], &args[5])
            .expect("the grant applies to the row that now exists");
        Box::pin(workspace.config.save_dirty())
            .await
            .expect("the grant saves");
        assert_eq!(
            hosts_on_disk(&workspace),
            Some(vec![DECLARED_HOST.to_string()])
        );
    }

    /// A `[plugins]` section the loader could not read, reset to defaults for
    /// this run: the section alone, or with the whole config file.
    fn degrade_plugins_section(config: &mut Config, whole_config: bool) {
        if whole_config {
            config
                .degraded_security
                .push(crate::config::migration::WHOLE_CONFIG_SENTINEL.to_string());
        } else {
            config.degraded_sections.push("plugins".to_string());
        }
    }

    #[tokio::test]
    async fn an_unreadable_plugins_section_offers_nothing_and_installs_nothing() {
        for whole_config in [false, true] {
            let server = MockServer::start().await;
            let entry = fixture_entry(&server, Some(archive_digest()));
            serve_index(
                &server,
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "plugins": [entry.clone()] })),
                0,
            )
            .await;
            serve_valid_archive(&server, 0).await;
            let mut workspace = Workspace::new();
            degrade_plugins_section(&mut workspace.config, whole_config);
            let before = workspace.config_bytes();
            let unreadable = qta(
                "cli-quickstart-plugins-section-unreadable",
                &[("path", &workspace.config.config_path.display().to_string())],
            );

            // The row says why in one line and offers nothing to pick, before
            // the registry is contacted.
            let registry = RegistryClient::new(RegistryTimeouts::default()).expect("client");
            let registry_url = format!("{}{INDEX_PATH}", server.uri());
            let mut row = PluginsRow::default();
            let mut prompter = ScriptedPrompter::new([]);
            let exit = open_row_with(
                &workspace.config,
                &mut row,
                &registry,
                &registry_url,
                &mut prompter,
            )
            .await
            .expect("the row closes");

            assert_eq!(exit, RowExit::Done, "whole config: {whole_config}");
            assert_eq!(prompter.said, vec![unreadable.clone()]);
            assert!(prompter.choice_lists.is_empty(), "nothing is offered");
            assert!(row.visited() && row.selected.is_empty(), "none is picked");

            // Create, holding a selection the row could not have offered,
            // publishes nothing and asks nothing.
            let mut prompter = ScriptedPrompter::new([]);
            let phase = run(&mut workspace, &selection(entry), &mut prompter)
                .await
                .expect("the phase completes");

            assert!(phase.outcomes.is_empty(), "whole config: {whole_config}");
            assert!(!phase.activation_changed);
            assert_eq!(prompter.said, vec![String::new(), unreadable]);
            assert!(
                prompter.choice_lists.is_empty()
                    && prompter.confirm_defaults.is_empty()
                    && prompter.fields.is_empty(),
                "nothing is asked"
            );
            assert!(
                !workspace.plugins_dir().exists(),
                "not even the plugins directory was created"
            );
            assert!(workspace.config.plugins.entries.is_empty());
            assert_eq!(workspace.config_bytes(), before);
            assert!(phase.readiness_lines(&workspace.config).await.is_empty());
            assert!(
                server
                    .received_requests()
                    .await
                    .expect("request recording is on")
                    .is_empty(),
                "neither the index nor the archive is requested"
            );
            server.verify().await;
        }
    }

    #[tokio::test]
    async fn an_existing_row_and_secret_references_stay_byte_identical() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 1).await;
        let dir = tempfile::tempdir().expect("workspace");
        let key = fixture_instance_key();
        let plugins_dir = toml::Value::String(dir.path().join("plugins").display().to_string());
        // A row left behind by `plugin remove`, holding encrypted settings and
        // a grant, next to a 1Password reference, with activation off so the
        // run saves the config file. The grant names a host the manifest does
        // not declare, so extending it with the declaration could not go
        // unnoticed.
        let unrelated_host = "unrelated.example.org";
        let encrypted_token = r#"api_token = "enc2:bm90LWEtcmVhbC1jaXBoZXJ0ZXh0""#;
        let encrypted_label = r#"label = "enc2:YWxzby1ub3QtcmVhbA""#;
        let onepassword = r#"api_key = "op://zeroclaw/provider/openai-api-key""#;
        let text = format!(
            r#"schema_version = {version}

[providers.models.openai.default]
model = "gpt-5"
{onepassword}

[plugins]
enabled = false
auto_discover = false
plugins_dir = {plugins_dir}

[[plugins.entries]]
name = "{key}"
egress_hosts = ["{unrelated_host}"]

[plugins.entries.config]
{encrypted_token}
{encrypted_label}
"#,
            version = crate::config::migration::CURRENT_SCHEMA_VERSION,
        );
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, &text).expect("write config.toml");
        let mut config: Config = toml::from_str(&text).expect("the config parses");
        config.config_path = config_path;
        config.data_dir = dir.path().join("data");
        let mut workspace = Workspace { dir, config };
        let row_before = workspace.row(&key).cloned().expect("the row exists");
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        // The row exists, so nothing about it is asked; activation is turned
        // on, which saves both flags into the same file.
        let mut prompter = ScriptedPrompter::new([Answer::Confirm(Some(true))]);

        let phase = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");

        prompter.assert_done();
        assert_eq!(
            phase.outcomes,
            vec![PackageOutcome::Installed {
                name: FIXTURE_NAME.to_string(),
            }]
        );
        assert!(phase.activation_changed, "the run saved the config file");
        assert!(workspace.package_dir().is_dir(), "the package is installed");
        let after = String::from_utf8(workspace.config_bytes()).expect("config.toml is UTF-8");
        let on_disk = workspace.config_on_disk();
        assert!(
            flag_on_disk(&on_disk, "enabled") && flag_on_disk(&on_disk, "auto_discover"),
            "the save changed what it was meant to: {after}"
        );
        for untouched in [encrypted_token, encrypted_label, onepassword] {
            assert!(
                after.lines().any(|line| line == untouched),
                "{untouched} must survive the save byte for byte: {after}"
            );
        }
        assert_eq!(
            after,
            text.replace("enabled = false", "enabled = true")
                .replace("auto_discover = false", "auto_discover = true"),
            "the save rewrote only the two activation flags"
        );
        let row_after = workspace.row(&key).expect("the row remains");
        assert_eq!(row_after.config, row_before.config);
        assert_eq!(
            row_after.egress_hosts,
            vec![unrelated_host.to_string()],
            "the existing grant is never extended with the declared host"
        );
        let on_disk = row_on_disk(&workspace.config_on_disk(), &key).expect("the row is on disk");
        assert_eq!(
            on_disk
                .get("egress_hosts")
                .and_then(toml::Value::as_array)
                .map(|hosts| hosts
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .collect::<Vec<_>>()),
            Some(vec![unrelated_host])
        );
        // The resolver accepts the existing row, which holds both required
        // settings, so the instance is active; the status prints no value.
        let readiness = phase.readiness_lines(&workspace.config).await.join("\n");
        assert!(
            readiness.contains(&qta(
                "cli-quickstart-plugins-ready",
                &[("name", FIXTURE_NAME), ("key", &key)]
            )),
            "{readiness}"
        );
        server.verify().await;
    }

    /// Log events are asserted by shape rather than captured: installing the
    /// process-wide capture subscriber also routes every `log` record through
    /// tracing, which turns on Cranelift's trace logging for every component
    /// compile in this test binary and slows the whole suite by an order of
    /// magnitude.
    #[test]
    fn phase_log_events_carry_package_names_and_never_settings() {
        let log = PhaseLog::new();
        for outcome in [
            PackageOutcome::Installed {
                name: FIXTURE_NAME.to_string(),
            },
            PackageOutcome::Failed {
                name: FIXTURE_NAME.to_string(),
                stage: FailureStage::Publish,
            },
        ] {
            let attrs = log.outcome_attrs(&outcome);
            let mut keys: Vec<&str> = attrs
                .as_object()
                .expect("attributes are an object")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                vec![
                    "outcome",
                    "plugin",
                    "quickstart.run_id",
                    "quickstart.surface"
                ]
            );
            assert_eq!(attrs["plugin"], FIXTURE_NAME);
            assert_eq!(attrs["outcome"], outcome.kind());
        }
    }

    #[tokio::test]
    async fn invalid_settings_are_asked_again_and_never_echoed() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 1).await;
        let mut workspace = Workspace::new();
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        let bad_value = "not-a-number-7c1e";
        let mut prompter = ScriptedPrompter::new([
            Answer::Select(Some(GRANT)),
            Answer::Field(Some(SECRET.to_string())),
            Answer::Field(Some("demo".to_string())),
            Answer::Confirm(Some(true)),
            Answer::Field(Some(bad_value.to_string())),
            // Asked again after the schema refused the first answers.
            Answer::Field(Some(SECRET.to_string())),
            Answer::Field(Some("demo".to_string())),
            Answer::Confirm(Some(true)),
            Answer::Field(Some("12".to_string())),
            Answer::Confirm(Some(false)),
        ]);

        let phase = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");

        prompter.assert_done();
        assert_eq!(
            phase.outcomes,
            vec![PackageOutcome::Installed {
                name: FIXTURE_NAME.to_string(),
            }]
        );
        let row = workspace
            .row(&fixture_instance_key())
            .expect("the row is seeded");
        assert_eq!(
            row.config.get("max_len").map(String::as_str),
            Some("12"),
            "an integer is stored as its JSON text"
        );
        let output = prompter.output();
        assert!(output.contains("max_len"), "the refusal names the setting");
        assert!(!output.contains(bad_value) && !output.contains(SECRET));
        server.verify().await;
    }

    #[tokio::test]
    async fn a_secret_typed_with_surrounding_whitespace_is_stored_trimmed() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 1).await;
        let mut workspace = Workspace::new();
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        // A pasted token that brings a space and a line break along, which
        // `config set` trims from what its masked prompt reads.
        let mut prompter = ScriptedPrompter::new([
            Answer::Select(Some(WITHHOLD)),
            Answer::Field(Some(format!(" {SECRET} \r\n"))),
            Answer::Field(Some(" demo ".to_string())),
            Answer::Confirm(Some(false)),
            Answer::Confirm(Some(false)),
        ]);

        let phase = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");

        prompter.assert_done();
        assert_eq!(
            phase.outcomes,
            vec![PackageOutcome::Installed {
                name: FIXTURE_NAME.to_string(),
            }]
        );
        let key = fixture_instance_key();
        // Read back as the runtime reads it: decrypted from the file on disk.
        // The values are compared, never printed.
        let stored = row_on_disk(&workspace.config_on_disk(), &key)
            .and_then(|row| {
                row.get("config")?
                    .get("api_token")?
                    .as_str()
                    .map(str::to_string)
            })
            .expect("the secret setting is persisted");
        assert!(
            stored.starts_with("enc2:"),
            "the secret is encrypted at rest"
        );
        let decrypted = crate::security::SecretStore::new(workspace.dir.path(), true)
            .decrypt(&stored)
            .expect("the stored secret decrypts");
        assert!(
            decrypted == SECRET,
            "the stored secret is the token without the whitespace around it"
        );
        let row = workspace.row(&key).expect("the row is seeded");
        assert!(
            row.config
                .get("api_token")
                .is_some_and(|value| value == SECRET),
            "the config Quickstart keeps holds the trimmed secret too"
        );
        assert_eq!(
            row.config.get("label").map(String::as_str),
            Some(" demo "),
            "a setting that is not secret is still stored exactly as typed"
        );
        let output = prompter.output();
        assert!(!output.contains(SECRET), "the secret is never printed");
        server.verify().await;
    }

    #[tokio::test]
    async fn installing_without_required_settings_is_never_reported_active() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 1).await;
        let mut workspace = Workspace::new();
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        let bad_value = "not-a-number-5e0b";
        // Two attempts the schema refuses, then install without the settings
        // and turn activation on.
        let attempt = || {
            [
                Answer::Field(Some(SECRET.to_string())),
                Answer::Field(Some("demo".to_string())),
                Answer::Confirm(Some(true)),
                Answer::Field(Some(bad_value.to_string())),
            ]
        };
        let mut answers = vec![Answer::Select(Some(GRANT))];
        answers.extend(attempt());
        answers.extend(attempt());
        answers.push(Answer::Confirm(Some(true)));
        answers.push(Answer::Confirm(Some(true)));
        let mut prompter = ScriptedPrompter::new(answers);

        let phase = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");

        prompter.assert_done();
        let key = fixture_instance_key();
        assert_eq!(
            phase.outcomes,
            vec![PackageOutcome::Installed {
                name: FIXTURE_NAME.to_string(),
            }]
        );
        assert!(
            workspace
                .row(&key)
                .expect("the row is seeded")
                .config
                .is_empty(),
            "no setting was saved"
        );
        assert_eq!(
            verdict(&workspace.config),
            ToolInstanceAdmission::Admitted {
                instance_key: key.clone()
            },
            "the activation plan alone would admit the instance"
        );

        let readiness = phase.readiness_lines(&workspace.config).await;
        assert_reported_missing(&readiness, &key);
        let text = readiness.join("\n");
        let output = prompter.output();
        for printed in [&text, &output] {
            assert!(
                !printed.contains(SECRET) && !printed.contains(bad_value),
                "{printed}"
            );
        }

        // Held back by a flag, the verdict comes first and the missing
        // settings still follow it.
        workspace.config.plugins.enabled = false;
        let held_back = phase.readiness_lines(&workspace.config).await;
        assert_eq!(
            held_back.get(2),
            Some(&indented(&qta(
                "cli-quickstart-plugins-ready-plugins-disabled",
                &[("name", FIXTURE_NAME)]
            ))),
            "{held_back:?}"
        );
        assert_reported_missing(&held_back, &key);
        workspace.config.plugins.enabled = true;

        // The status reads the row, not a record of this run: once both
        // settings are present, the same phase reports the instance active.
        let row = workspace
            .config
            .plugins
            .entries
            .iter_mut()
            .find(|entry| entry.name == key)
            .expect("the row is seeded");
        row.config
            .insert("api_token".to_string(), "set-later".to_string());
        row.config.insert("label".to_string(), "demo".to_string());
        let readiness = phase.readiness_lines(&workspace.config).await.join("\n");
        assert!(
            readiness.contains(&qta(
                "cli-quickstart-plugins-ready",
                &[("name", FIXTURE_NAME), ("key", &key)]
            )),
            "{readiness}"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn settings_that_fail_to_save_are_reported_missing_rather_than_active() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 1).await;
        let mut workspace = Workspace::new();
        // A key file of the wrong length. The seeded row holds no secret, so
        // the publish saves; the first save that must encrypt a setting fails.
        std::fs::write(workspace.dir.path().join(".secret_key"), "00")
            .expect("write a broken key file");
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        let mut answers = install_answers(GRANT);
        answers.push(Answer::Confirm(Some(true)));
        let mut prompter = ScriptedPrompter::new(answers);

        let phase = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");

        prompter.assert_done();
        assert_eq!(
            phase.outcomes,
            vec![PackageOutcome::Installed {
                name: FIXTURE_NAME.to_string(),
            }]
        );
        let output = prompter.output();
        let failed = qta(
            "cli-quickstart-plugins-config-save-failed",
            &[
                ("name", FIXTURE_NAME),
                ("keys", "api_token, label"),
                ("error", "<error>"),
                ("command", "<command>"),
            ],
        );
        let (failed_prefix, _) = failed
            .split_once("<error>")
            .expect("the message carries the error");
        assert!(
            output.contains(failed_prefix),
            "the failed save is reported: {output}"
        );
        let key = fixture_instance_key();
        // `config set` asks for a secret value with masked input, so the
        // command carries none to type into shell history.
        let set_later = zeroclaw_command(
            &workspace.config,
            &format!("config set plugins.entries.{key}.config.<key>"),
        );
        assert!(
            output.contains(&set_later) && !output.contains("<value>"),
            "the command that finishes the job carries no value: {output}"
        );
        assert!(
            workspace
                .row(&key)
                .expect("the row is seeded")
                .config
                .is_empty(),
            "the row holds no setting"
        );
        let on_disk = row_on_disk(&workspace.config_on_disk(), &key).expect("the row is on disk");
        assert!(
            on_disk
                .get("config")
                .and_then(toml::Value::as_table)
                .is_none_or(toml::Table::is_empty),
            "no setting reached disk: {on_disk:?}"
        );
        assert!(workspace.config.plugins.enabled && workspace.config.plugins.auto_discover);
        assert_eq!(
            verdict(&workspace.config),
            ToolInstanceAdmission::Admitted {
                instance_key: key.clone()
            }
        );

        let readiness = phase.readiness_lines(&workspace.config).await;
        assert_reported_missing(&readiness, &key);
        let text = readiness.join("\n");
        for printed in [&text, &output] {
            assert!(!printed.contains(SECRET), "{printed}");
        }
        assert!(
            !String::from_utf8_lossy(&workspace.config_bytes()).contains(SECRET),
            "the typed secret never reaches disk"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn an_installed_package_whose_new_row_lacks_required_settings_is_not_reported_active() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 0).await;
        let mut workspace = Workspace::new();
        install_without_a_row(&workspace);
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        // Grant the declared destination, then turn activation on. A package
        // installed before this run is never asked for its settings.
        let mut prompter =
            ScriptedPrompter::new([Answer::Select(Some(GRANT)), Answer::Confirm(Some(true))]);

        let phase = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");

        prompter.assert_done();
        assert_eq!(
            phase.outcomes,
            vec![PackageOutcome::AlreadyInstalled {
                name: FIXTURE_NAME.to_string(),
                seeded_row: true,
            }]
        );
        assert!(prompter.fields.is_empty(), "no setting was asked for");
        let key = fixture_instance_key();
        assert!(
            workspace
                .row(&key)
                .expect("the row is seeded")
                .config
                .is_empty()
        );
        assert_eq!(
            verdict(&workspace.config),
            ToolInstanceAdmission::Admitted {
                instance_key: key.clone()
            }
        );
        assert_reported_missing(&phase.readiness_lines(&workspace.config).await, &key);
        server.verify().await;
    }

    #[tokio::test]
    async fn an_installed_package_that_does_not_load_is_never_reported_active() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 0).await;
        let mut workspace = Workspace::new();
        // Installed before this run without the load check, as
        // `plugin install --no-verify` installs it, with a row holding both
        // required settings and activation on: everything but the load
        // check says active.
        install_without_a_row(&workspace);
        let key = fixture_instance_key();
        workspace
            .config
            .plugins
            .entries
            .push(crate::config::schema::PluginEntryConfig {
                name: key.clone(),
                config: HashMap::from([
                    ("api_token".to_string(), "x".to_string()),
                    ("label".to_string(), "demo".to_string()),
                ]),
                ..Default::default()
            });
        workspace.config.plugins.enabled = true;
        workspace.config.plugins.auto_discover = true;
        // Fewer instances than the component needs, so the load check that
        // `plugin info` runs refuses it.
        workspace.config.plugins.limits.max_instances = 1;
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        let mut prompter = ScriptedPrompter::new([]);

        let phase = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");

        prompter.assert_done();
        assert_eq!(
            phase.outcomes,
            vec![PackageOutcome::AlreadyInstalled {
                name: FIXTURE_NAME.to_string(),
                seeded_row: false,
            }]
        );
        assert_eq!(
            verdict(&workspace.config),
            ToolInstanceAdmission::Admitted {
                instance_key: key.clone()
            },
            "the activation plan alone would admit the instance"
        );
        let active = qta(
            "cli-quickstart-plugins-ready",
            &[("name", FIXTURE_NAME), ("key", &key)],
        );
        let info = zeroclaw_command(&workspace.config, "plugin info tool-fixture");
        assert!(info.contains("--config-dir"), "{info}");

        let readiness = phase.readiness_lines(&workspace.config).await;

        let text = readiness.join("\n");
        assert!(
            !text.contains(&active),
            "a package that does not load is never reported active: {text}"
        );
        assert_eq!(
            readiness.get(2),
            Some(&indented(&qta(
                "cli-quickstart-plugins-ready-does-not-load",
                &[("name", FIXTURE_NAME), ("command", &info)]
            ))),
            "the status says it does not load and gives the command that shows why: {text}"
        );

        // With the instances the component needs, the same package loads, and
        // the status reports it active.
        workspace.config.plugins.limits.max_instances =
            Config::default().plugins.limits.max_instances;
        let readiness = phase.readiness_lines(&workspace.config).await.join("\n");
        assert!(readiness.contains(&active), "{readiness}");
        assert!(!readiness.contains(&info), "{readiness}");
        server.verify().await;
    }

    #[tokio::test]
    async fn a_duplicated_row_is_reported_unavailable_rather_than_active() {
        let mut workspace = Workspace::new();
        install_without_a_row(&workspace);
        workspace.config.plugins.enabled = true;
        workspace.config.plugins.auto_discover = true;
        let key = fixture_instance_key();
        // Config validation refuses this at load; the status must not
        // panic on it or call the instance active.
        let row = crate::config::schema::PluginEntryConfig {
            name: key.clone(),
            config: HashMap::from([
                ("api_token".to_string(), "x".to_string()),
                ("label".to_string(), "demo".to_string()),
            ]),
            ..Default::default()
        };
        workspace.config.plugins.entries = vec![row.clone(), row];
        let phase = CreatePhase {
            outcomes: vec![PackageOutcome::AlreadyInstalled {
                name: FIXTURE_NAME.to_string(),
                seeded_row: false,
            }],
            activation_changed: false,
        };

        let readiness = phase.readiness_lines(&workspace.config).await;

        assert_eq!(
            readiness.get(2),
            Some(&indented(&qta(
                "cli-quickstart-plugins-ready-unknown",
                &[
                    ("name", FIXTURE_NAME),
                    (
                        "error",
                        &zeroclaw_config::schema::DuplicatePluginConfigEntry.to_string()
                    ),
                ]
            ))),
            "{readiness:?}"
        );
        assert!(
            !readiness.join("\n").contains(&qta(
                "cli-quickstart-plugins-ready",
                &[("name", FIXTURE_NAME), ("key", &key)]
            )),
            "{readiness:?}"
        );
    }

    #[tokio::test]
    async fn a_row_with_every_required_key_but_a_value_the_schema_rejects_is_not_reported_active() {
        let mut workspace = Workspace::new();
        install_without_a_row(&workspace);
        workspace.config.plugins.enabled = true;
        workspace.config.plugins.auto_discover = true;
        let key = fixture_instance_key();
        let bad_value = "not-a-number-3d9a";
        // Both required settings are present; the optional integer holds text,
        // which the resolver refuses on every call.
        workspace
            .config
            .plugins
            .entries
            .push(crate::config::schema::PluginEntryConfig {
                name: key.clone(),
                config: HashMap::from([
                    ("api_token".to_string(), "x".to_string()),
                    ("label".to_string(), "demo".to_string()),
                    ("max_len".to_string(), bad_value.to_string()),
                ]),
                ..Default::default()
            });
        let phase = CreatePhase {
            outcomes: vec![PackageOutcome::AlreadyInstalled {
                name: FIXTURE_NAME.to_string(),
                seeded_row: false,
            }],
            activation_changed: false,
        };

        let readiness = phase.readiness_lines(&workspace.config).await;

        assert_eq!(
            verdict(&workspace.config),
            ToolInstanceAdmission::Admitted {
                instance_key: key.clone()
            },
            "the activation plan alone would admit the instance"
        );
        let text = readiness.join("\n");
        assert!(
            !text.contains(&qta(
                "cli-quickstart-plugins-ready",
                &[("name", FIXTURE_NAME), ("key", &key)]
            )),
            "{text}"
        );
        let reason = readiness.get(2).expect("the package's status line");
        assert!(
            reason
                .trim_start()
                .starts_with(&rejected_prefix(FIXTURE_NAME))
                && reason.contains("max_len"),
            "the resolver's reason names the property: {text}"
        );
        assert!(
            !text.contains("config set"),
            "no required setting is missing, so there is no command: {text}"
        );
        assert!(
            !text.contains(bad_value),
            "values are never printed: {text}"
        );
    }

    /// A tool whose schema requires a secret with a portable name, a setting
    /// declared under a name outside the portable grammar, and a name its
    /// `properties` map does not declare.
    const STRICT_MANIFEST: &str = r#"name = "strict-tool"
version = "0.1.0"
wasm_path = "tool-fixture.wasm"
capabilities = ["tool"]
permissions = ["config_read"]

[config_schema]
"$schema" = "https://json-schema.org/draft/2020-12/schema"
type = "object"
additionalProperties = false
required = ["api_token", "bad key", "undeclared"]

[config_schema.properties.api_token]
type = "string"
x-secret = true

[config_schema.properties."bad key"]
type = "string"
"#;

    #[tokio::test]
    async fn a_required_setting_outside_the_portable_grammar_is_named_without_a_command() {
        let mut workspace = Workspace::new();
        install_with_manifest(&workspace, "strict-tool", STRICT_MANIFEST);
        workspace.config.plugins.enabled = true;
        workspace.config.plugins.auto_discover = true;
        let key = instance_key_of(STRICT_MANIFEST);
        // The one required setting with a portable name is set.
        workspace
            .config
            .plugins
            .entries
            .push(crate::config::schema::PluginEntryConfig {
                name: key.clone(),
                config: HashMap::from([("api_token".to_string(), "x".to_string())]),
                ..Default::default()
            });
        let phase = CreatePhase {
            outcomes: vec![PackageOutcome::Installed {
                name: "strict-tool".to_string(),
            }],
            activation_changed: false,
        };

        let readiness = phase.readiness_lines(&workspace.config).await;

        let text = readiness.join("\n");
        assert!(
            !text.contains(&qta(
                "cli-quickstart-plugins-ready",
                &[("name", "strict-tool"), ("key", &key)]
            )),
            "an instance the resolver rejects is not reported active: {text}"
        );
        assert!(
            readiness.iter().any(|line| line
                .trim_start()
                .starts_with(&rejected_prefix("strict-tool"))),
            "{text}"
        );
        // `config set` writes the setting when its path is quoted; Quickstart
        // prints commands only for portable names, so it names this one and
        // says where it is set instead.
        assert!(
            readiness.contains(&indented(&qta(
                "cli-quickstart-plugins-ready-missing-nonportable",
                &[("name", "strict-tool"), ("keys", "bad key")]
            ))),
            "the setting is named: {text}"
        );
        assert!(
            readiness.contains(&indented(&qta(
                "cli-quickstart-plugins-ready-undeclared-required",
                &[("name", "strict-tool"), ("keys", "undeclared")]
            ))),
            "a required name the schema does not declare is named apart: {text}"
        );
        assert!(
            !text.contains(&format!("plugins.entries.{key}.config.")),
            "no command is printed for a name outside the portable grammar: {text}"
        );
    }

    #[test]
    fn a_required_setting_no_prompt_asks_for_goes_straight_to_the_install_choice() {
        let manifest: PluginManifest =
            toml::from_str(STRICT_MANIFEST).expect("the manifest parses");
        let blocking = indented(&qta(
            "cli-quickstart-plugins-config-required-unsupported",
            &[("name", "strict-tool"), ("keys", "bad key, undeclared")],
        ));
        for install in [true, false] {
            let mut prompter = ScriptedPrompter::new([Answer::Confirm(Some(install))]);

            let settings = collect_settings(&manifest, &mut prompter).expect("no prompt fails");

            prompter.assert_done();
            assert!(
                prompter.fields.is_empty(),
                "no setting is prompted for, not even the one Quickstart could ask: {:?}",
                prompter.fields
            );
            assert_eq!(
                prompter.said,
                vec![blocking.clone()],
                "the settings no prompt asks for are named first"
            );
            assert_eq!(
                prompter.confirm_defaults,
                vec![false],
                "installing without settings is opted into"
            );
            match settings {
                Some(settings) => assert!(
                    install && settings.values.is_empty(),
                    "accepting installs the package with no settings"
                ),
                None => assert!(!install, "declining skips the package"),
            }
        }
    }

    #[tokio::test]
    async fn activation_defaults_to_no_when_it_would_wake_a_dormant_package() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 1).await;
        let mut workspace = Workspace::new();
        // A skill package installed long before this run and never activated.
        let dormant = workspace.plugins_dir().join("notes");
        std::fs::create_dir_all(dormant.join("skills/greet")).expect("skill dir");
        std::fs::write(
            dormant.join("manifest.toml"),
            "name = \"notes\"\nversion = \"1.0.0\"\ncapabilities = [\"skill\"]\n",
        )
        .expect("skill manifest");
        std::fs::write(
            dormant.join("skills/greet/SKILL.md"),
            "---\nname: greet\ndescription: say hello\n---\n\nHello.\n",
        )
        .expect("skill body");
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        let mut answers = install_answers(GRANT);
        answers.push(Answer::Confirm(Some(false)));
        let mut prompter = ScriptedPrompter::new(answers);

        run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");

        prompter.assert_done();
        assert_eq!(
            prompter.confirm_defaults.last(),
            Some(&false),
            "waking a package this run did not select must be opted into"
        );
        let output = prompter.output();
        assert!(
            output.contains("notes"),
            "the dormant package is listed: {output}"
        );
        server.verify().await;
    }

    /// Ask the activation question for a run that installed only the
    /// fixture, answering no, and return the prompter.
    async fn ask_activation(workspace: &mut Workspace, host: &PluginHost) -> ScriptedPrompter {
        let outcomes = [PackageOutcome::Installed {
            name: FIXTURE_NAME.to_string(),
        }];
        let mut prompter = ScriptedPrompter::new([Answer::Confirm(Some(false))]);
        let changed = Box::pin(activation_consent(
            &mut workspace.config,
            host,
            &outcomes,
            &PhaseLog::new(),
            &mut prompter,
        ))
        .await
        .expect("the question is answered");
        prompter.assert_done();
        assert!(!changed, "declining changes nothing");
        prompter
    }

    #[tokio::test]
    async fn the_activation_preview_lists_a_package_on_disk_that_the_install_host_dropped() {
        let mut workspace = Workspace::new();
        install_without_a_row(&workspace);
        // The host this run installed the fixture through.
        let host = crate::plugin_host_with_configured_security(&workspace.config).expect("host");

        // With the host and the plugins directory in step, the list is this
        // run's package alone, so the answer defaults to yes.
        let prompter = ask_activation(&mut workspace, &host).await;
        assert_eq!(prompter.confirm_defaults, vec![true]);

        // A package directory the host does not list, as a publish whose
        // rollback failed leaves one: the host drops the package from its
        // loaded set before deleting its directory, and a daemon started
        // later loads what the directory holds.
        let stranded = "stranded-tool";
        install_with_manifest(
            &workspace,
            stranded,
            &format!(
                "name = \"{stranded}\"\nversion = \"0.1.0\"\n\
                 wasm_path = \"tool-fixture.wasm\"\ncapabilities = [\"tool\"]\n"
            ),
        );
        assert!(host.manifest(stranded).is_none(), "the host never saw it");

        let prompter = ask_activation(&mut workspace, &host).await;

        let output = prompter.output();
        assert!(
            output.contains(&activated_line(&ActivatedInstance::Tool {
                package: stranded.to_string(),
            })),
            "the package on disk is in the preview: {output}"
        );
        assert!(
            output.contains(&qta("cli-quickstart-plugins-activation-others", &[])),
            "{output}"
        );
        assert_eq!(
            prompter.confirm_defaults,
            vec![false],
            "activating a package this run did not install must be opted into"
        );
    }

    #[tokio::test]
    async fn an_activation_preview_the_plugins_directory_cannot_confirm_defaults_to_no() {
        let mut workspace = Workspace::new();
        install_without_a_row(&workspace);
        let host = crate::plugin_host_with_configured_security(&workspace.config).expect("host");
        // The plugins directory can no longer be listed: its path now names a
        // regular file.
        let not_a_directory = workspace.dir.path().join("plugins-file");
        std::fs::write(&not_a_directory, "").expect("write a regular file");
        workspace.config.plugins.plugins_dir = not_a_directory.display().to_string();
        assert!(crate::plugin_host_with_configured_security(&workspace.config).is_err());

        let prompter = ask_activation(&mut workspace, &host).await;

        let output = prompter.output();
        assert!(
            output.contains(&activated_line(&ActivatedInstance::Tool {
                package: FIXTURE_NAME.to_string(),
            })),
            "the preview falls back to the install host: {output}"
        );
        let unverified = qta(
            "cli-quickstart-plugins-activation-unverified",
            &[("error", "<error>")],
        );
        let (unverified_prefix, _) = unverified
            .split_once("<error>")
            .expect("the line carries the discovery error");
        assert!(
            prompter
                .said
                .iter()
                .any(|line| line.starts_with(unverified_prefix)),
            "the list is said to be possibly incomplete: {output}"
        );
        assert_eq!(
            prompter.confirm_defaults,
            vec![false],
            "a preview that may miss packages defaults to no"
        );
    }

    #[tokio::test]
    async fn ctrl_c_after_an_install_reports_it_and_leaves_it_whole() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 1).await;
        let mut workspace = Workspace::new();
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        let mut answers = install_answers(GRANT);
        answers.push(Answer::Interrupt);
        let mut prompter = ScriptedPrompter::new(answers);

        let halt = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect_err("Ctrl+C at the activation prompt stops the phase");

        let PhaseHalt::Interrupted { outcomes } = &halt else {
            panic!("expected an interruption, got {halt:?}");
        };
        assert_eq!(
            outcomes,
            &vec![PackageOutcome::Installed {
                name: FIXTURE_NAME.to_string(),
            }]
        );
        assert!(workspace.package_dir().join("manifest.toml").is_file());
        assert!(workspace.row(&fixture_instance_key()).is_some());
        assert!(
            !workspace.config.plugins.enabled,
            "activation was never answered"
        );
        assert!(halt.report().is_none(), "Ctrl+C exits rather than erring");
        server.verify().await;
    }

    #[tokio::test]
    async fn a_submission_the_agent_step_refuses_stops_create_before_any_plugin_is_touched() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 0).await;
        let mut workspace = Workspace::new();
        let before = workspace.config_bytes();
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        let registry = RegistryClient::new(RegistryTimeouts::default()).expect("registry client");
        let mut prompter = ScriptedPrompter::new([]);

        let halt = Box::pin(create_phase_with(
            &mut workspace.config,
            &selection,
            &submission(""),
            &registry,
            &mut prompter,
        ))
        .await
        .expect_err("a submission the agent step refuses stops Create");

        let PhaseHalt::AgentRejected(errors) = &halt else {
            panic!("expected the agent step's refusal, got {halt:?}");
        };
        assert!(
            errors
                .iter()
                .any(|error| error.step == QuickstartStep::Agent),
            "the refusal is the agent step's own: {errors:?}"
        );
        assert!(
            server
                .received_requests()
                .await
                .expect("request recording is on")
                .is_empty(),
            "no archive was requested"
        );
        assert!(
            !workspace.plugins_dir().exists(),
            "not even the plugins directory was created"
        );
        assert_eq!(workspace.config_bytes(), before);
        assert!(
            prompter.said.is_empty() && prompter.choice_lists.is_empty(),
            "nothing was printed or asked: {:?}",
            prompter.said
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn a_failed_agent_step_after_an_install_never_says_nothing_changed() {
        let server = MockServer::start().await;
        serve_valid_archive(&server, 1).await;
        let mut workspace = Workspace::new();
        let selection = selection(fixture_entry(&server, Some(archive_digest())));
        let mut answers = install_answers(GRANT);
        answers.push(Answer::Confirm(Some(true)));
        let mut prompter = ScriptedPrompter::new(answers);

        let phase = run(&mut workspace, &selection, &mut prompter)
            .await
            .expect("the phase completes");
        prompter.assert_done();

        // The agent step fails after this point: its report opens with these
        // lines instead of the ones that say nothing on disk was changed.
        let headline = phase
            .apply_failed_headline(&workspace.config)
            .expect("this run installed a plugin")
            .join("\n");
        for false_claim in [
            qta("cli-agent-not-created", &[]),
            qta("cli-quickstart-fix-and-rerun", &[]),
        ] {
            assert!(!headline.contains(&false_claim), "{headline}");
        }
        for line in [
            qta("cli-quickstart-plugins-agent-not-created", &[]),
            qta(
                "cli-quickstart-plugins-apply-failed-state-activated",
                &[("names", FIXTURE_NAME)],
            ),
            qta("cli-quickstart-plugins-fix-and-rerun", &[]),
        ] {
            assert!(headline.contains(&line), "{line:?} missing from {headline}");
        }
        assert!(
            headline.contains("--config-dir") && headline.contains("plugin remove tool-fixture"),
            "the package this run installed comes with its removal command: {headline}"
        );
        server.verify().await;
    }

    #[test]
    fn the_failure_headline_follows_what_the_run_changed() {
        let config = Config::default();
        let phase = |outcomes: Vec<PackageOutcome>, activation_changed: bool| CreatePhase {
            outcomes,
            activation_changed,
        };
        let kept = |seeded_row: bool| PackageOutcome::AlreadyInstalled {
            name: "kept".to_string(),
            seeded_row,
        };

        // Nothing changed, so "nothing on disk was changed" stays true.
        assert_eq!(
            phase(Vec::new(), false).apply_failed_headline(&config),
            None
        );
        assert_eq!(
            phase(
                vec![
                    kept(false),
                    PackageOutcome::AlreadyInstalledSkipped {
                        name: "rowless".to_string()
                    },
                    PackageOutcome::Skipped {
                        name: "skipped".to_string()
                    },
                    PackageOutcome::Failed {
                        name: "broken".to_string(),
                        stage: FailureStage::Download,
                    },
                ],
                false,
            )
            .apply_failed_headline(&config),
            None
        );

        // A seeded row is a change, but the package was installed before:
        // there is nothing of this run's to remove.
        let seeded = phase(vec![kept(true)], false)
            .apply_failed_headline(&config)
            .expect("seeding a row changed the config")
            .join("\n");
        assert!(seeded.contains(&qta(
            "cli-quickstart-plugins-apply-failed-state",
            &[("names", "kept")]
        )));
        assert!(!seeded.contains("plugin remove"), "{seeded}");

        let activated = phase(vec![kept(false)], true)
            .apply_failed_headline(&config)
            .expect("turning activation on changed the config")
            .join("\n");
        assert!(activated.contains(&qta("cli-quickstart-plugins-apply-failed-activated", &[])));
        assert!(!activated.contains("plugin remove"), "{activated}");

        // A failed publish whose rollback failed too left its package on
        // disk: a change of this run's, named with the command that removes
        // it, never "nothing changed".
        let stranded = phase(
            vec![
                PackageOutcome::RollbackFailed {
                    name: "stuck".to_string(),
                },
                PackageOutcome::Failed {
                    name: "broken".to_string(),
                    stage: FailureStage::Publish,
                },
            ],
            false,
        )
        .apply_failed_headline(&config)
        .expect("a package left installed is a change")
        .join("\n");
        assert!(
            stranded.contains(&qta(
                "cli-quickstart-plugins-apply-failed-state",
                &[("names", "stuck")]
            )),
            "only the package left behind is named: {stranded}"
        );
        assert!(
            stranded.contains(&zeroclaw_command(&config, "plugin remove stuck")),
            "{stranded}"
        );
        assert!(!stranded.contains("plugin remove broken"), "{stranded}");
    }

    #[test]
    fn a_publish_whose_rollback_failed_is_reported_installed_with_its_removal_command() {
        let workspace = Workspace::new();
        let config = &workspace.config;
        let remove = zeroclaw_command(config, "plugin remove stuck");
        assert!(remove.contains("--config-dir"), "{remove}");
        let rollback = crate::PublishRollbackFailed {
            package: "stuck".to_string(),
            rollback_error: "Permission denied (os error 13)".to_string(),
        };
        let layer = rollback.to_string();
        let stranded = anyhow::Error::msg("config file is read-only").context(rollback);

        let (outcome, line) = publish_failure(config, "stuck", &stranded);

        assert_eq!(
            outcome,
            PackageOutcome::RollbackFailed {
                name: "stuck".to_string()
            }
        );
        assert_eq!(
            line,
            qta(
                "cli-quickstart-plugins-rollback-failed",
                &[
                    ("name", "stuck"),
                    ("error", "config file is read-only"),
                    ("rollback_error", "Permission denied (os error 13)"),
                    ("command", &remove),
                ]
            ),
            "the package is still installed, and the line says so with the command \
             that removes it from this configuration"
        );
        let not_installed = qta(
            "cli-quickstart-plugins-failed",
            &[("name", "stuck"), ("error", "<error>")],
        );
        let (not_installed_prefix, _) = not_installed
            .split_once("<error>")
            .expect("the line carries the error");
        assert!(
            !line.starts_with(not_installed_prefix),
            "a package left on disk is never reported as not installed: {line}"
        );
        assert!(
            !line.contains(&layer),
            "the install command's own text, whose removal command names no \
             configuration directory, is left out: {line}"
        );

        // A publish the transaction rolled back installed nothing.
        let rolled_back = anyhow::Error::msg("config file is read-only")
            .context("the plugin package 'broken' was rolled back");
        let (outcome, line) = publish_failure(config, "broken", &rolled_back);
        assert_eq!(
            outcome,
            PackageOutcome::Failed {
                name: "broken".to_string(),
                stage: FailureStage::Publish,
            }
        );
        assert_eq!(
            line,
            qta(
                "cli-quickstart-plugins-failed",
                &[
                    ("name", "broken"),
                    (
                        "error",
                        "the plugin package 'broken' was rolled back: config file is read-only"
                    ),
                ]
            )
        );
    }

    async fn serve_index(server: &MockServer, response: ResponseTemplate, hits: u64) {
        Mock::given(method("GET"))
            .and(path(INDEX_PATH))
            .respond_with(response)
            .expect(hits)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn the_plugins_row_offers_registry_tools_and_keeps_the_pick() {
        let server = MockServer::start().await;
        let entry = fixture_entry(&server, Some(archive_digest()));
        serve_index(
            &server,
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "plugins": [entry] })),
            1,
        )
        .await;
        let workspace = Workspace::new();
        let registry = RegistryClient::new(RegistryTimeouts::default()).expect("client");
        let registry_url = format!("{}{INDEX_PATH}", server.uri());
        let mut row = PluginsRow::default();
        let mut prompter = ScriptedPrompter::new([Answer::MultiSelect(Some(vec![0]))]);

        let exit = open_row_with(
            &workspace.config,
            &mut row,
            &registry,
            &registry_url,
            &mut prompter,
        )
        .await
        .expect("the row opens");

        assert_eq!(exit, RowExit::Done);
        assert!(row.visited());
        assert_eq!(row.selected.len(), 1);
        assert_eq!(row.selected[0].name, FIXTURE_NAME);
        assert!(prompter.choice_lists[0][0].contains(FIXTURE_NAME));
        assert!(
            workspace.installed_packages().is_empty(),
            "picking downloads nothing"
        );
        let cached =
            zeroclaw::plugins::registry::read_cached_registry_index(&workspace.config.data_dir)
                .expect("the cache reads")
                .expect("the index is cached");
        assert_eq!(cached.registry_url.as_deref(), Some(registry_url.as_str()));
        server.verify().await;
    }

    #[tokio::test]
    async fn an_unreachable_registry_offers_retry_then_continues_without_plugins() {
        let server = MockServer::start().await;
        serve_index(&server, ResponseTemplate::new(500), 2).await;
        let workspace = Workspace::new();
        let registry = RegistryClient::new(RegistryTimeouts::default()).expect("client");
        let registry_url = format!("{}{INDEX_PATH}", server.uri());
        let mut row = PluginsRow::default();
        let mut prompter =
            ScriptedPrompter::new([Answer::Select(Some(0)), Answer::Select(Some(1))]);

        let exit = open_row_with(
            &workspace.config,
            &mut row,
            &registry,
            &registry_url,
            &mut prompter,
        )
        .await
        .expect("the row closes");

        prompter.assert_done();
        assert_eq!(exit, RowExit::Done);
        assert!(row.visited(), "continuing without plugins is a choice");
        assert!(row.selected.is_empty());
        assert!(prompter.output().contains("HTTP 500"));
        server.verify().await;
    }

    #[test]
    fn the_default_registrys_missing_index_names_the_environment_variable_not_a_flag() {
        let unpopulated = anyhow::Error::from(crate::plugin_registry::DefaultRegistryUnpopulated);
        // `plugin install` and `plugin search` keep their text, which names
        // their flag.
        assert!(format!("{unpopulated:#}").contains("--registry <url>"));

        let line = registry_unavailable_line(&unpopulated);
        assert_eq!(
            line,
            qta("cli-quickstart-plugins-registry-unpopulated", &[])
        );
        assert!(
            line.contains("ZEROCLAW_PLUGIN_REGISTRY_URL") && !line.contains("--registry"),
            "Quickstart has no --registry flag: {line}"
        );

        let other = anyhow::Error::msg("plugin registry returned HTTP 500");
        assert_eq!(
            registry_unavailable_line(&other),
            qta(
                "cli-quickstart-plugins-registry-unavailable",
                &[("error", "plugin registry returned HTTP 500")]
            ),
            "every other failure is printed as it reads"
        );
    }

    #[test]
    fn a_row_summary_marks_installed_picks() {
        let mut row = PluginsRow::default();
        assert_eq!(
            row.summary(),
            qta("cli-quickstart-summary-not-yet-visited", &[])
        );
        row.visited = true;
        assert_eq!(
            row.summary(),
            qta("cli-quickstart-plugins-summary-none", &[])
        );
        let choice = |name: &str, state: ChoiceState| PluginChoice {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            description: None,
            state,
            registry_entry: None,
        };
        row.selected = vec![
            choice("fresh", ChoiceState::Available),
            choice("\u{1b}[31mkept", ChoiceState::Installed),
        ];
        assert_eq!(
            row.summary(),
            format!(
                "fresh, {}",
                qta(
                    "cli-quickstart-plugins-summary-installed",
                    &[("name", "kept")]
                )
            )
        );
    }

    /// Every English string of the Plugins step parses and formats. A Fluent
    /// syntax error would otherwise surface only at run time, as the `{key}`
    /// sentinel in place of the message.
    #[test]
    fn every_plugins_step_string_formats_with_its_placeables() {
        let english = include_str!("../../crates/zeroclaw-runtime/locales/en/cli.ftl");
        let mut checked = 0;
        for (key, value) in english.lines().filter_map(|line| line.split_once(" = ")) {
            if !key.starts_with("cli-quickstart-plugins-") && key != "cli-quickstart-row-plugins" {
                continue;
            }
            let placeables: Vec<&str> = value
                .match_indices("{$")
                .map(|(at, _)| {
                    let rest = &value[at + 2..];
                    &rest[..rest.find('}').expect("a placeable is closed")]
                })
                .collect();
            let args: Vec<(&str, &str)> = placeables.iter().map(|name| (*name, "x")).collect();
            let rendered = qta(key, &args);
            assert!(
                !rendered.contains("{$") && rendered != format!("{{{key}}}"),
                "{key} renders as {rendered:?}"
            );
            checked += 1;
        }
        assert!(checked > 0, "the catalogue holds the Plugins step strings");
    }
}
