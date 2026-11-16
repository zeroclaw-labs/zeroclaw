//! `zeroclaw status` — show system status.

use anyhow::Result;

use crate::config::Config;
use crate::cost;
use crate::gateway_helpers::{t, ta};
use crate::print_companion_outbox_line;
use crate::service;

/// Print full system status, or exit 0/1 for Docker HEALTHCHECK (`--format exit-code`).
pub async fn handle(config: &Config, format: Option<String>) -> Result<()> {
    if format.as_deref() == Some("exit-code") {
        // Lightweight health probe for Docker HEALTHCHECK
        let port = config.gateway.port;
        let host = if config.gateway.host == "[::]" || config.gateway.host == "0.0.0.0" {
            "127.0.0.1"
        } else {
            &config.gateway.host
        };
        let url = format!("http://{}:{}/health", host, port);
        match reqwest::Client::new()
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                std::process::exit(0);
            }
            _ => {
                std::process::exit(1);
            }
        }
    }
    println!("{}", t("cli-status-title", "🦀 ZeroClaw Status"));
    println!();
    println!(
        "{}",
        ta(
            "cli-status-version",
            &[("v", env!("CARGO_PKG_VERSION"))],
            "Version"
        )
    );
    println!(
        "{}",
        ta(
            "cli-status-workspace",
            &[("v", &config.data_dir.display().to_string())],
            "Workspace"
        )
    );
    println!(
        "{}",
        ta(
            "cli-status-config",
            &[("v", &config.config_path.display().to_string())],
            "Config"
        )
    );
    println!();
    let mut shown_provider = false;
    for (family, alias, entry) in config.providers.models.iter_entries() {
        let model = entry.model.as_deref().unwrap_or("(none)");
        if shown_provider {
            println!(
                "{}",
                ta(
                    "cli-status-provider-indent",
                    &[("family", family), ("alias", alias)],
                    "ModelProvider"
                )
            );
            println!("{}", ta("cli-status-model", &[("model", model)], "Model"));
        } else {
            println!(
                "{}",
                ta(
                    "cli-status-provider",
                    &[("family", family), ("alias", alias)],
                    "ModelProvider"
                )
            );
            println!("{}", ta("cli-status-model", &[("model", model)], "Model"));
            shown_provider = true;
        }
    }
    if !shown_provider {
        println!(
            "{}",
            t(
                "cli-status-provider-none",
                "🤖 ModelProvider:      (none configured)"
            )
        );
    }
    println!(
        "{}",
        ta(
            "cli-status-observability",
            &[("v", config.observability.backend.as_wire())],
            "Observability"
        )
    );
    let trace_storage_mode = config.observability.log_persistence.as_wire().to_string();
    let trace_storage_path = config.observability.log_persistence_path.to_string();
    let trace_storage_fallback = format!(
        "🧾 Trace storage:  {} ({})",
        trace_storage_mode, trace_storage_path
    );
    println!(
        "{}",
        ta(
            "cli-status-trace-storage",
            &[("mode", &trace_storage_mode), ("path", &trace_storage_path),],
            &trace_storage_fallback
        )
    );
    // Per-agent autonomy: each enabled agent picks its own
    // risk_profile, so list them rather than collapsing to one.
    let mut agent_aliases: Vec<&String> = config
        .agents
        .iter()
        .filter(|(_, a)| a.enabled)
        .map(|(alias, _)| alias)
        .collect();
    agent_aliases.sort();
    if agent_aliases.is_empty() {
        println!(
            "{}",
            t(
                "cli-status-agents-none",
                "🛡️  Agents:        (none configured)"
            )
        );
    } else {
        let summary: Vec<String> = agent_aliases
            .iter()
            .map(|alias| match config.risk_profile_for_agent(alias) {
                Some(p) => format!("{alias}={:?}", p.level),
                None => format!("{alias}=<no risk_profile>"),
            })
            .collect();
        println!(
            "{}",
            ta("cli-status-agents", &[("v", &summary.join(", "))], "Agents")
        );
    }
    println!(
        "{}",
        ta(
            "cli-status-runtime",
            &[("v", config.runtime.kind.as_wire())],
            "Runtime"
        )
    );
    if service::is_running(config) {
        println!(
            "{}",
            t("cli-status-service-running", "🟢 Service:       running")
        );
    } else {
        println!(
            "{}",
            t("cli-status-service-stopped", "🔴 Service:       stopped")
        );
    }
    let effective_memory_backend = self::effective_memory_backend(config);
    let heartbeat_value = if config.heartbeat.enabled {
        let interval_minutes = config.heartbeat.interval_minutes.to_string();
        let heartbeat_every_fallback = format!("every {}min", interval_minutes);
        ta(
            "cli-status-heartbeat-every-minutes",
            &[("minutes", &interval_minutes)],
            &heartbeat_every_fallback,
        )
    } else {
        t("cli-status-word-disabled", "disabled")
    };
    let heartbeat_fallback = format!("💓 Heartbeat:      {}", heartbeat_value);
    println!(
        "{}",
        ta(
            "cli-status-heartbeat",
            &[("v", &heartbeat_value)],
            &heartbeat_fallback
        )
    );
    let memory_backend = effective_memory_backend.to_string();
    let memory_auto_save = if config.memory.auto_save {
        t("cli-status-word-on", "on")
    } else {
        t("cli-status-word-off", "off")
    };
    let memory_fallback = format!(
        "🧠 Memory:         {} (auto-save: {})",
        memory_backend, memory_auto_save
    );
    println!(
        "{}",
        ta(
            "cli-status-memory",
            &[
                ("backend", &memory_backend),
                ("auto_save", &memory_auto_save),
            ],
            &memory_fallback
        )
    );
    print_companion_outbox_line(config);

    println!();
    // Per-agent security: each enabled agent's risk profile.
    for alias in &agent_aliases {
        let Some(profile) = config.risk_profile_for_agent(alias) else {
            println!(
                "{}",
                ta(
                    "cli-status-security-noprofile",
                    &[("alias", alias)],
                    "Security: no risk_profile"
                )
            );
            continue;
        };
        println!(
            "{}",
            ta("cli-status-security", &[("alias", alias)], "Security")
        );
        println!(
            "{}",
            ta(
                "cli-status-workspace-only",
                &[("v", &profile.workspace_only.to_string())],
                "Workspace only"
            )
        );
        let allowed_roots = if profile.allowed_roots.is_empty() {
            t("cli-status-word-none", "(none)")
        } else {
            profile.allowed_roots.join(", ")
        };
        let allowed_roots_fallback = format!("  Allowed roots:     {}", allowed_roots);
        println!(
            "{}",
            ta(
                "cli-status-allowed-roots",
                &[("v", &allowed_roots)],
                &allowed_roots_fallback
            )
        );
        let allowed_commands = profile.allowed_commands.join(", ");
        let allowed_commands_fallback = format!("  Allowed commands:  {}", allowed_commands);
        println!(
            "{}",
            ta(
                "cli-status-allowed-commands",
                &[("v", &allowed_commands)],
                &allowed_commands_fallback
            )
        );
        let actions_cap = config
            .runtime_profile_for_agent(alias)
            .map_or(0, |r| r.max_actions_per_hour);
        println!(
            "{}",
            ta(
                "cli-status-max-actions",
                &[("v", &actions_cap.to_string())],
                "Max actions/hour"
            )
        );
    }
    let cost_tracking = if config.cost.enabled {
        t("cli-status-word-enabled", "enabled")
    } else {
        t("cli-status-word-disabled", "disabled")
    };
    let cost_tracking_fallback = format!("  Cost tracking:     {}", cost_tracking);
    println!(
        "{}",
        ta(
            "cli-status-cost-tracking",
            &[("v", &cost_tracking)],
            &cost_tracking_fallback
        )
    );
    println!(
        "{}",
        ta(
            "cli-status-max-cost-day",
            &[("v", &format!("{:.2}", config.cost.daily_limit_usd))],
            "Max cost/day"
        )
    );
    println!(
        "{}",
        ta(
            "cli-status-max-cost-month",
            &[("v", &format!("{:.2}", config.cost.monthly_limit_usd))],
            "Max cost/month"
        )
    );
    if config.cost.enabled {
        match cost::CostTracker::new(config.cost.clone(), &config.data_dir) {
            Ok(tracker) => match tracker.get_summary() {
                Ok(summary) => {
                    let spent_today = format!("{:.4}", summary.daily_cost_usd);
                    let daily_limit = format!("{:.2}", config.cost.daily_limit_usd);
                    let spent_today_fallback =
                        format!("  Spent today:       ${spent_today} / ${daily_limit}");
                    println!(
                        "{}",
                        ta(
                            "cli-status-spent-today",
                            &[("spent", &spent_today), ("limit", &daily_limit)],
                            &spent_today_fallback
                        )
                    );
                    let spent_month = format!("{:.4}", summary.monthly_cost_usd);
                    let monthly_limit = format!("{:.2}", config.cost.monthly_limit_usd);
                    let spent_month_fallback =
                        format!("  Spent this month:  ${spent_month} / ${monthly_limit}");
                    println!(
                        "{}",
                        ta(
                            "cli-status-spent-month",
                            &[("spent", &spent_month), ("limit", &monthly_limit)],
                            &spent_month_fallback
                        )
                    );
                }
                Err(e) => {
                    eprintln!(
                        "{}",
                        ta(
                            "cli-warn-cost-usage",
                            &[("err", &e.to_string())],
                            "Could not load cost usage"
                        )
                    );
                }
            },
            Err(e) => {
                eprintln!(
                    "{}",
                    ta(
                        "cli-warn-cost-tracker",
                        &[("err", &e.to_string())],
                        "Could not init cost tracker"
                    )
                );
            }
        }
    }
    println!(
        "{}",
        ta(
            "cli-status-otp",
            &[("v", &config.security.otp.enabled.to_string())],
            "OTP enabled"
        )
    );
    println!(
        "{}",
        ta(
            "cli-status-estop",
            &[("v", &config.security.estop.enabled.to_string())],
            "E-stop enabled"
        )
    );
    println!();
    println!("{}", t("cli-status-channels", "Channels:"));
    println!("{}", t("cli-status-cli-always", "  CLI:      ✅ always"));
    for entry in crate::channels::listing::compiled_channels(&config.channels) {
        let channel_status = if entry.configured {
            t("cli-status-word-configured", "configured")
        } else {
            t("cli-status-word-not-configured", "not configured")
        };
        println!(
            "  {:9} {}",
            entry.name,
            if entry.configured {
                format!("✅ {}", channel_status)
            } else {
                format!("❌ {}", channel_status)
            }
        );
    }
    let uncompiled = crate::channels::listing::configured_uncompiled_channels(&config.channels);
    if !uncompiled.is_empty() {
        println!(
            "{}",
            t(
                "cli-channels-not-compiled-header",
                "  Configured but not compiled in this binary:"
            )
        );
        for entry in &uncompiled {
            println!(
                "  {:9} {}",
                entry.name,
                t(
                    "cli-status-channel-not-compiled",
                    "🚫 configured, not compiled"
                )
            );
        }
        println!(
            "{}",
            t(
                "cli-channels-build-hint",
                "  Build from source with `./install.sh --source --preset full`, `--features channels-full`, or the specific `channel-*` feature."
            )
        );
    }
    let retired = crate::channels::listing::configured_retired_channels(&config.channels);
    if !retired.is_empty() {
        println!(
            "{}",
            t(
                "cli-channels-retired-header",
                "  Configured sections of retired transports (no runner exists):"
            )
        );
        for entry in &retired {
            println!(
                "  {:9} {}",
                entry.name,
                t(
                    "cli-status-channel-retired",
                    "⚠️ section parses, nothing runs; remove it"
                )
            );
        }
        println!(
            "{}",
            t(
                "cli-channels-retired-hint",
                "  Live surfaces: Telegram bridge, webhook ingress, Email tools, Discord archive search."
            )
        );
    }

    Ok(())
}

/// Resolve the memory backend label printed by `zeroclaw status`.
///
/// Runtime construction reads `config.memory.backend` directly (`tachi`,
/// `markdown`, `none`); `Config::resolve_active_storage` returns
/// `ActiveStorage::None` for `tachi` and alias-less `markdown` because only
/// explicit `[storage.markdown.<alias>]` entries appear under `config.storage`.
fn effective_memory_backend(config: &Config) -> &'static str {
    let backend = crate::memory::backend_kind_from_dotted(&config.memory.backend);
    if backend.is_empty() {
        return "none";
    }
    match crate::memory::classify_memory_backend(&backend) {
        #[cfg(feature = "memory-tachi")]
        crate::memory::MemoryBackendKind::Tachi => "tachi",
        crate::memory::MemoryBackendKind::Markdown => "markdown",
        crate::memory::MemoryBackendKind::None => "none",
        crate::memory::MemoryBackendKind::Retired(name) => name,
        crate::memory::MemoryBackendKind::Unknown => {
            if backend == "tachi" {
                "tachi"
            } else {
                "markdown"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::effective_memory_backend;
    use crate::config::Config;

    #[test]
    fn default_config_reports_tachi_not_none() {
        let config = Config::default();
        assert_eq!(effective_memory_backend(&config), "tachi");
    }

    #[test]
    fn markdown_and_dotted_markdown_report_markdown_without_storage_alias() {
        for raw in ["markdown", "markdown.notes", " MARKDOWN "] {
            let mut config = Config::default();
            config.memory.backend = raw.to_string();
            assert_eq!(effective_memory_backend(&config), "markdown");
        }
    }

    #[test]
    fn none_and_empty_report_none() {
        for raw in ["none", "", "   "] {
            let mut config = Config::default();
            config.memory.backend = raw.to_string();
            assert_eq!(effective_memory_backend(&config), "none");
        }
    }

    #[test]
    fn retired_backend_preserves_name_instead_of_masking_as_none() {
        let mut config = Config::default();
        config.memory.backend = "sqlite".to_string();
        assert_eq!(effective_memory_backend(&config), "sqlite");
    }
}
