//! Thin CLI adapter for the read-only plugin package catalog.

use zeroclaw::plugins::catalog::{
    OriginCatalogEntry as PluginCatalogEntry, origin_package_catalog as package_catalog,
};
use zeroclaw::plugins::host::PluginHost;
use zeroclaw::plugins::registry::read_cached_registry_collection;
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};

pub(crate) fn print(config: &crate::config::schema::Config, host: &PluginHost) {
    let installed = host.list_plugins();
    let cached_registry = match read_cached_registry_collection(&config.data_dir) {
        Ok(index) => index.filter(zeroclaw::plugins::registry::cache_matches_environment),
        Err(error) => {
            let error = error.to_string();
            eprintln!(
                "{}",
                get_required_cli_string_with_args(
                    "cli-plugin-catalog-cache-failed",
                    &[("error", error.as_str())],
                )
            );
            None
        }
    };
    let entries = package_catalog(&installed, cached_registry.as_ref());
    render(&entries, crate::egress_command_config_dir(config));
}

fn render(entries: &[PluginCatalogEntry<'_>], config_dir: &std::path::Path) {
    if entries.is_empty() {
        println!("{}", get_required_cli_string("cli-plugin-catalog-empty"));
        return;
    }

    println!("{}", get_required_cli_string("cli-plugin-catalog-heading"));
    for entry in entries {
        let missing_description = get_required_cli_string("cli-plugin-no-description");
        let description = entry
            .installed()
            .and_then(|plugin| plugin.description.as_deref())
            .or_else(|| {
                entry
                    .available()
                    .and_then(|plugin| plugin.description.as_deref())
            })
            .unwrap_or(missing_description.as_str());

        let line = match (entry.installed(), entry.available()) {
            (Some(installed), Some(available)) if installed.version == available.version => {
                get_required_cli_string_with_args(
                    "cli-plugin-catalog-installed-listed",
                    &[
                        ("name", entry.name()),
                        ("version", installed.version.as_str()),
                        ("description", description),
                    ],
                )
            }
            (Some(installed), Some(available)) => get_required_cli_string_with_args(
                "cli-plugin-catalog-installed-other-version",
                &[
                    ("name", entry.name()),
                    ("installed_version", installed.version.as_str()),
                    ("available_version", available.version.as_str()),
                    ("description", description),
                ],
            ),
            (Some(installed), None) => get_required_cli_string_with_args(
                "cli-plugin-catalog-installed",
                &[
                    ("name", entry.name()),
                    ("version", installed.version.as_str()),
                    ("description", description),
                ],
            ),
            (None, Some(available)) => get_required_cli_string_with_args(
                "cli-plugin-catalog-available",
                &[
                    ("name", entry.name()),
                    ("version", available.version.as_str()),
                    ("description", description),
                ],
            ),
            (None, None) => continue,
        };
        println!("{line}");
        if let Some(available) = entry.available() {
            let command = zeroclaw::plugins::registry::registry_install_command_for(
                zeroclaw::plugins::registry::ShellDialect::host(),
                available,
                entry.origin,
                Some(config_dir),
                false,
            );
            let hint = match command {
                Some(command) => get_required_cli_string_with_args(
                    "cli-plugin-registry-command",
                    &[("command", &command)],
                ),
                None => get_required_cli_string("cli-plugin-registry-command-unavailable"),
            };
            println!("{hint}");
        }
    }
}
