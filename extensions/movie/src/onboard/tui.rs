use crate::config::schema::{CloudflareTunnelConfig, NgrokTunnelConfig};
use crate::config::{
    default_model_fallback_for_provider, ChannelsConfig, Config, DiscordConfig, ProgressMode,
    StreamMode, TelegramConfig, TunnelConfig,
};
use crate::onboard::wizard::{run_quick_setup_with_migration, OpenClawOnboardMigrationOptions};
use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use console::style;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use reqwest::blocking::Client;
use serde_json::Value;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Duration;

const PROVIDER_OPTIONS: [&str; 5] = ["openrouter", "openai", "anthropic", "gemini", "ollama"];
const MEMORY_OPTIONS: [&str; 4] = ["sqlite", "lucid", "markdown", "none"];
const TUNNEL_OPTIONS: [&str; 3] = ["none", "cloudflare", "ngrok"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Welcome,
    Workspace,
    Provider,
    ProviderDiagnostics,
    Runtime,
    Channels,
    ChannelDiagnostics,
    Tunnel,
    TunnelDiagnostics,
    Review,
}

impl Step {
    const ORDER: [Self; 10] = [
        Self::Welcome,
        Self::Workspace,
        Self::Provider,
        Self::ProviderDiagnostics,
        Self::Runtime,
        Self::Channels,
        Self::ChannelDiagnostics,
        Self::Tunnel,
        Self::TunnelDiagnostics,
        Self::Review,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Welcome => "Welcome",
            Self::Workspace => "Workspace",
            Self::Provider => "AI Provider",
            Self::ProviderDiagnostics => "Provider Diagnostics",
            Self::Runtime => "Memory & Security",
            Self::Channels => "Channels",
            Self::ChannelDiagnostics => "Channel Diagnostics",
            Self::Tunnel => "Tunnel",
            Self::TunnelDiagnostics => "Tunnel Diagnostics",
            Self::Review => "Review & Apply",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Self::Welcome => "Review controls and continue to setup.",
            Self::Workspace => "Pick where config.toml and workspace files should live.",
            Self::Provider => "Select provider, API key, and default model.",
            Self::ProviderDiagnostics => "Run live checks against your selected provider.",
            Self::Runtime => "Choose memory backend and security defaults.",
            Self::Channels => "Optional: configure Telegram/Discord entry points.",
            Self::ChannelDiagnostics => "Run channel checks before writing config.",
            Self::Tunnel => "Optional: expose gateway with Cloudflare or ngrok.",
            Self::TunnelDiagnostics => "Probe tunnel credentials before apply.",
            Self::Review => "Validate final config and apply onboarding.",
        }
    }

    fn index(self) -> usize {
        Self::ORDER
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0)
    }

    fn next(self) -> Self {
        let idx = self.index();
        if idx + 1 >= Self::ORDER.len() {
            self
        } else {
            Self::ORDER[idx + 1]
        }
    }

    fn previous(self) -> Self {
        let idx = self.index();
        if idx == 0 {
            self
        } else {
            Self::ORDER[idx - 1]
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKey {
    Continue,
    WorkspacePath,
    ForceOverwrite,
    Provider,
    ApiKey,
    Model,
    MemoryBackend,
    DisableTotp,
    EnableTelegram,
    TelegramToken,
    TelegramAllowedUsers,
    EnableDiscord,
    DiscordToken,
    DiscordGuildId,
    DiscordAllowedUsers,
    AutostartChannels,
    TunnelProvider,
    CloudflareToken,
    NgrokAuthToken,
    NgrokDomain,
    RunProviderProbe,
    ProviderProbeResult,
    ProviderProbeDetails,
    ProviderProbeRemediation,
    RunTelegramProbe,
    TelegramProbeResult,
    TelegramProbeDetails,
    TelegramProbeRemediation,
    RunDiscordProbe,
    DiscordProbeResult,
    DiscordProbeDetails,
    DiscordProbeRemediation,
    RunCloudflareProbe,
    CloudflareProbeResult,
    CloudflareProbeDetails,
    CloudflareProbeRemediation,
    RunNgrokProbe,
    NgrokProbeResult,
    NgrokProbeDetails,
    NgrokProbeRemediation,
    AllowFailedDiagnostics,
    Apply,
}

#[derive(Debug, Clone)]
struct FieldView {
    key: FieldKey,
    label: &'static str,
    value: String,
    hint: &'static str,
    required: bool,
    editable: bool,
}

#[derive(Debug, Clone)]
enum CheckStatus {
    NotRun,
    Passed(String),
    Failed(String),
    Skipped(String),
}

impl CheckStatus {
    fn as_line(&self) -> String {
        match self {
            Self::NotRun => "not run".to_string(),
            Self::Passed(details) => format!("pass: {details}"),
            Self::Failed(details) => format!("fail: {details}"),
            Self::Skipped(details) => format!("skipped: {details}"),
        }
    }

    fn badge(&self) -> &'static str {
        match self {
            Self::NotRun => "idle",
            Self::Passed(_) => "pass",
            Self::Failed(_) => "fail",
            Self::Skipped(_) => "skip",
        }
    }

    fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

#[derive(Debug, Clone)]
struct TuiOnboardPlan {
    workspace_path: String,
    force_overwrite: bool,
    provider_idx: usize,
    api_key: String,
    model: String,
    memory_idx: usize,
    disable_totp: bool,
    enable_telegram: bool,
    telegram_token: String,
    telegram_allowed_users: String,
    enable_discord: bool,
    discord_token: String,
    discord_guild_id: String,
    discord_allowed_users: String,
    autostart_channels: bool,
    tunnel_idx: usize,
    cloudflare_token: String,
    ngrok_auth_token: String,
    ngrok_domain: String,
    allow_failed_diagnostics: bool,
}

impl TuiOnboardPlan {
    fn new(default_workspace: PathBuf, force: bool) -> Self {
        let provider = PROVIDER_OPTIONS[0];
        Self {
            workspace_path: default_workspace.display().to_string(),
            force_overwrite: force,
            provider_idx: 0,
            api_key: String::new(),
            model: provider_default_model(provider),
            memory_idx: 0,
            disable_totp: false,
            enable_telegram: false,
            telegram_token: String::new(),
            telegram_allowed_users: String::new(),
            enable_discord: false,
            discord_token: String::new(),
            discord_guild_id: String::new(),
            discord_allowed_users: String::new(),
            autostart_channels: true,
            tunnel_idx: 0,
            cloudflare_token: String::new(),
            ngrok_auth_token: String::new(),
            ngrok_domain: String::new(),
            allow_failed_diagnostics: false,
        }
    }

    fn provider(&self) -> &str {
        PROVIDER_OPTIONS[self.provider_idx]
    }

    fn memory_backend(&self) -> &str {
        MEMORY_OPTIONS[self.memory_idx]
    }

    fn tunnel_provider(&self) -> &str {
        TUNNEL_OPTIONS[self.tunnel_idx]
    }
}

#[derive(Debug, Clone)]
struct EditingState {
    key: FieldKey,
    value: String,
    secret: bool,
}

#[derive(Debug, Clone)]
struct TuiState {
    step: Step,
    focus: usize,
    editing: Option<EditingState>,
    status: String,
    plan: TuiOnboardPlan,
    model_touched: bool,
    provider_probe: CheckStatus,
    telegram_probe: CheckStatus,
    discord_probe: CheckStatus,
    cloudflare_probe: CheckStatus,
    ngrok_probe: CheckStatus,
}

impl TuiState {
    fn new(default_workspace: PathBuf, force: bool) -> Self {
        Self {
            step: Step::Welcome,
            focus: 0,
            editing: None,
            status: "Controls: arrows/jkhl + Enter. Use n/p for next/back steps, Ctrl+S to save edits, q to quit."
                .to_string(),
            plan: TuiOnboardPlan::new(default_workspace, force),
            model_touched: false,
            provider_probe: CheckStatus::NotRun,
            telegram_probe: CheckStatus::NotRun,
            discord_probe: CheckStatus::NotRun,
            cloudflare_probe: CheckStatus::NotRun,
            ngrok_probe: CheckStatus::NotRun,
        }
    }

    fn visible_fields(&self) -> Vec<FieldView> {
        match self.step {
            Step::Welcome => vec![FieldView {
                key: FieldKey::Continue,
                label: "Start",
                value: "Press Enter to begin onboarding".to_string(),
                hint: "Move to the first setup step.",
                required: false,
                editable: false,
            }],
            Step::Workspace => vec![
                FieldView {
                    key: FieldKey::WorkspacePath,
                    label: "Workspace path",
                    value: display_value(&self.plan.workspace_path, false),
                    hint: "~ is supported and will be expanded.",
                    required: true,
                    editable: true,
                },
                FieldView {
                    key: FieldKey::ForceOverwrite,
                    label: "Overwrite existing config",
                    value: bool_label(self.plan.force_overwrite),
                    hint: "Enable to overwrite existing config.toml. If launched with --force, this starts as yes.",
                    required: false,
                    editable: true,
                },
            ],
            Step::Provider => vec![
                FieldView {
                    key: FieldKey::Provider,
                    label: "Provider",
                    value: self.plan.provider().to_string(),
                    hint: "Pick your primary model provider.",
                    required: true,
                    editable: true,
                },
                FieldView {
                    key: FieldKey::ApiKey,
                    label: "API key",
                    value: display_value(&self.plan.api_key, true),
                    hint: "Optional for keyless/local providers.",
                    required: false,
                    editable: true,
                },
                FieldView {
                    key: FieldKey::Model,
                    label: "Default model",
                    value: display_value(&self.plan.model, false),
                    hint: "Used as default for `zeroclaw agent`.",
                    required: true,
                    editable: true,
                },
            ],
            Step::ProviderDiagnostics => vec![
                FieldView {
                    key: FieldKey::RunProviderProbe,
                    label: "Run provider probe",
                    value: "Press Enter to test connectivity".to_string(),
                    hint: "Uses provider-specific model-list/API health request.",
                    required: false,
                    editable: false,
                },
                FieldView {
                    key: FieldKey::ProviderProbeResult,
                    label: "Provider probe status",
                    value: self.provider_probe.as_line(),
                    hint: "Probe is advisory; apply is still allowed on failure.",
                    required: false,
                    editable: false,
                },
                FieldView {
                    key: FieldKey::ProviderProbeDetails,
                    label: "Provider check details",
                    value: self.provider_probe_details(),
                    hint: "Shows what was checked in this probe run.",
                    required: false,
                    editable: false,
                },
                FieldView {
                    key: FieldKey::ProviderProbeRemediation,
                    label: "Provider remediation",
                    value: self.provider_probe_remediation(),
                    hint: "Actionable next step for failures/skips.",
                    required: false,
                    editable: false,
                },
            ],
            Step::Runtime => vec![
                FieldView {
                    key: FieldKey::MemoryBackend,
                    label: "Memory backend",
                    value: self.plan.memory_backend().to_string(),
                    hint: "sqlite is the safest default for most setups.",
                    required: true,
                    editable: true,
                },
                FieldView {
                    key: FieldKey::DisableTotp,
                    label: "Disable TOTP",
                    value: bool_label(self.plan.disable_totp),
                    hint: "Keep off unless you explicitly want no OTP challenge.",
                    required: false,
                    editable: true,
                },
            ],
            Step::Channels => {
                let mut rows = vec![
                    FieldView {
                        key: FieldKey::EnableTelegram,
                        label: "Enable Telegram",
                        value: bool_label(self.plan.enable_telegram),
                        hint: "Adds Telegram bot channel config.",
                        required: false,
                        editable: true,
                    },
                    FieldView {
                        key: FieldKey::EnableDiscord,
                        label: "Enable Discord",
                        value: bool_label(self.plan.enable_discord),
                        hint: "Adds Discord bot channel config.",
                        required: false,
                        editable: true,
                    },
                    FieldView {
                        key: FieldKey::AutostartChannels,
                        label: "Autostart channels",
                        value: bool_label(self.plan.autostart_channels),
                        hint: "If enabled, channel server starts after onboarding.",
                        required: false,
                        editable: true,
                    },
                ];
                if self.plan.enable_telegram {
                    rows.insert(
                        1,
                        FieldView {
                            key: FieldKey::TelegramToken,
                            label: "Telegram bot token",
                            value: display_value(&self.plan.telegram_token, true),
                            hint: "Token from @BotFather.",
                            required: true,
                            editable: true,
                        },
                    );
                    rows.insert(
                        2,
                        FieldView {
                            key: FieldKey::TelegramAllowedUsers,
                            label: "Telegram allowlist",
                            value: display_value(&self.plan.telegram_allowed_users, false),
                            hint: "Comma-separated user IDs/usernames; empty blocks all.",
                            required: false,
                            editable: true,
                        },
                    );
                }
                if self.plan.enable_discord {
                    let base = if self.plan.enable_telegram { 4 } else { 2 };
                    rows.insert(
                        base,
                        FieldView {
                            key: FieldKey::DiscordToken,
                            label: "Discord bot token",
                            value: display_value(&self.plan.discord_token, true),
                            hint: "Bot token from Discord Developer Portal.",
                            required: true,
                            editable: true,
                        },
                    );
                    rows.insert(
                        base + 1,
                        FieldView {
                            key: FieldKey::DiscordGuildId,
                            label: "Discord guild ID",
                            value: display_value(&self.plan.discord_guild_id, false),
                            hint: "Optional server scope.",
                            required: false,
                            editable: true,
                        },
                    );
                    rows.insert(
                        base + 2,
                        FieldView {
                            key: FieldKey::DiscordAllowedUsers,
                            label: "Discord allowlist",
                            value: display_value(&self.plan.discord_allowed_users, false),
                            hint: "Comma-separated user IDs; empty blocks all.",
                            required: false,
                            editable: true,
                        },
                    );
                }
                rows
            }
            Step::ChannelDiagnostics => {
                let mut rows = Vec::new();
                if self.plan.enable_telegram {
                    rows.push(FieldView {
                        key: FieldKey::RunTelegramProbe,
                        label: "Run Telegram test",
                        value: "Press Enter to call getMe".to_string(),
                        hint: "Validates bot token with Telegram API.",
                        required: false,
                        editable: false,
                    });
                    rows.push(FieldView {
                        key: FieldKey::TelegramProbeResult,
                        label: "Telegram status",
                        value: self.telegram_probe.as_line(),
                        hint: "Requires internet connectivity.",
                        required: false,
                        editable: false,
                    });
                    rows.push(FieldView {
                        key: FieldKey::TelegramProbeDetails,
                        label: "Telegram check details",
                        value: self.telegram_probe_details(),
                        hint: "Connection + token health for Telegram bot.",
                        required: false,
                        editable: false,
                    });
                    rows.push(FieldView {
                        key: FieldKey::TelegramProbeRemediation,
                        label: "Telegram remediation",
                        value: self.telegram_probe_remediation(),
                        hint: "What to fix when Telegram checks fail.",
                        required: false,
                        editable: false,
                    });
                }
                if self.plan.enable_discord {
                    rows.push(FieldView {
                        key: FieldKey::RunDiscordProbe,
                        label: "Run Discord test",
                        value: "Press Enter to query bot guilds".to_string(),
                        hint: "Validates bot token and optional guild scope.",
                        required: false,
                        editable: false,
                    });
                    rows.push(FieldView {
                        key: FieldKey::DiscordProbeResult,
                        label: "Discord status",
                        value: self.discord_probe.as_line(),
                        hint: "Requires internet connectivity.",
                        required: false,
                        editable: false,
                    });
                    rows.push(FieldView {
                        key: FieldKey::DiscordProbeDetails,
                        label: "Discord check details",
                        value: self.discord_probe_details(),
                        hint: "Token + optional guild visibility checks.",
                        required: false,
                        editable: false,
                    });
                    rows.push(FieldView {
                        key: FieldKey::DiscordProbeRemediation,
                        label: "Discord remediation",
                        value: self.discord_probe_remediation(),
                        hint: "What to fix when Discord checks fail.",
                        required: false,
                        editable: false,
                    });
                }

                if rows.is_empty() {
                    rows.push(FieldView {
                        key: FieldKey::Continue,
                        label: "No checks configured",
                        value: "Enable Telegram or Discord in previous step".to_string(),
                        hint: "Use n or PageDown to continue.",
                        required: false,
                        editable: false,
                    });
                }

                rows
            }
            Step::Tunnel => {
                let mut rows = vec![FieldView {
                    key: FieldKey::TunnelProvider,
                    label: "Tunnel provider",
                    value: self.plan.tunnel_provider().to_string(),
                    hint: "none keeps ZeroClaw local-only.",
                    required: true,
                    editable: true,
                }];
                match self.plan.tunnel_provider() {
                    "cloudflare" => rows.push(FieldView {
                        key: FieldKey::CloudflareToken,
                        label: "Cloudflare token",
                        value: display_value(&self.plan.cloudflare_token, true),
                        hint: "Token from Cloudflare Zero Trust dashboard.",
                        required: true,
                        editable: true,
                    }),
                    "ngrok" => {
                        rows.push(FieldView {
                            key: FieldKey::NgrokAuthToken,
                            label: "ngrok auth token",
                            value: display_value(&self.plan.ngrok_auth_token, true),
                            hint: "Token from dashboard.ngrok.com.",
                            required: true,
                            editable: true,
                        });
                        rows.push(FieldView {
                            key: FieldKey::NgrokDomain,
                            label: "ngrok domain",
                            value: display_value(&self.plan.ngrok_domain, false),
                            hint: "Optional custom domain.",
                            required: false,
                            editable: true,
                        });
                    }
                    _ => {}
                }
                rows
            }
            Step::TunnelDiagnostics => match self.plan.tunnel_provider() {
                "cloudflare" => vec![
                    FieldView {
                        key: FieldKey::RunCloudflareProbe,
                        label: "Run Cloudflare token probe",
                        value: "Press Enter to decode token payload".to_string(),
                        hint: "Checks JWT-like tunnel token structure and claims.",
                        required: false,
                        editable: false,
                    },
                    FieldView {
                        key: FieldKey::CloudflareProbeResult,
                        label: "Cloudflare status",
                        value: self.cloudflare_probe.as_line(),
                        hint: "Probe is offline and does not call Cloudflare API.",
                        required: false,
                        editable: false,
                    },
                    FieldView {
                        key: FieldKey::CloudflareProbeDetails,
                        label: "Cloudflare check details",
                        value: self.cloudflare_probe_details(),
                        hint: "Token shape/claim diagnostics from local decode.",
                        required: false,
                        editable: false,
                    },
                    FieldView {
                        key: FieldKey::CloudflareProbeRemediation,
                        label: "Cloudflare remediation",
                        value: self.cloudflare_probe_remediation(),
                        hint: "How to recover from token parse failures.",
                        required: false,
                        editable: false,
                    },
                ],
                "ngrok" => vec![
                    FieldView {
                        key: FieldKey::RunNgrokProbe,
                        label: "Run ngrok API probe",
                        value: "Press Enter to verify API token".to_string(),
                        hint: "Calls ngrok API /tunnels with auth token.",
                        required: false,
                        editable: false,
                    },
                    FieldView {
                        key: FieldKey::NgrokProbeResult,
                        label: "ngrok status",
                        value: self.ngrok_probe.as_line(),
                        hint: "Probe is advisory; apply blocks on explicit failures.",
                        required: false,
                        editable: false,
                    },
                    FieldView {
                        key: FieldKey::NgrokProbeDetails,
                        label: "ngrok check details",
                        value: self.ngrok_probe_details(),
                        hint: "API token auth + active tunnel visibility.",
                        required: false,
                        editable: false,
                    },
                    FieldView {
                        key: FieldKey::NgrokProbeRemediation,
                        label: "ngrok remediation",
                        value: self.ngrok_probe_remediation(),
                        hint: "How to recover from ngrok auth/network failures.",
                        required: false,
                        editable: false,
                    },
                ],
                _ => vec![FieldView {
                    key: FieldKey::Continue,
                    label: "No tunnel diagnostics",
                    value: "Diagnostics available for cloudflare or ngrok providers".to_string(),
                    hint: "Use n or PageDown to continue.",
                    required: false,
                    editable: false,
                }],
            },
            Step::Review => vec![
                FieldView {
                    key: FieldKey::AllowFailedDiagnostics,
                    label: "Allow failed diagnostics",
                    value: bool_label(self.plan.allow_failed_diagnostics),
                    hint: "Keep off for production safety. Toggle only if failures are understood.",
                    required: false,
                    editable: true,
                },
                FieldView {
                    key: FieldKey::Apply,
                    label: "Apply onboarding",
                    value: "Press Enter (or a/s) to generate config".to_string(),
                    hint: "Use p/PageUp to revisit steps. Enter, a, or s applies.",
                    required: false,
                    editable: false,
                },
            ],
        }
    }

    fn current_field_key(&self) -> Option<FieldKey> {
        let fields = self.visible_fields();
        fields.get(self.focus).map(|field| field.key)
    }

    fn move_focus(&mut self, delta: isize) {
        let total = self.visible_fields().len();
        if total == 0 {
            self.focus = 0;
            return;
        }

        let mut next = self.focus as isize + delta;
        if next < 0 {
            next = total as isize - 1;
        }
        if next >= total as isize {
            next = 0;
        }
        self.focus = next as usize;
    }

    fn clamp_focus(&mut self) {
        let total = self.visible_fields().len();
        if total == 0 {
            self.focus = 0;
        } else if self.focus >= total {
            self.focus = total - 1;
        }
    }

    fn validate_step(&self, step: Step) -> Result<()> {
        match step {
            Step::Welcome => Ok(()),
            Step::Workspace => {
                if self.plan.workspace_path.trim().is_empty() {
                    bail!("Workspace path is required")
                }
                Ok(())
            }
            Step::Provider => {
                if self.plan.model.trim().is_empty() {
                    bail!("Default model is required")
                }
                Ok(())
            }
            Step::ProviderDiagnostics => Ok(()),
            Step::Runtime => Ok(()),
            Step::Channels => {
                if self.plan.enable_telegram && self.plan.telegram_token.trim().is_empty() {
                    bail!("Telegram is enabled but bot token is empty")
                }
                if self.plan.enable_discord && self.plan.discord_token.trim().is_empty() {
                    bail!("Discord is enabled but bot token is empty")
                }
                Ok(())
            }
            Step::ChannelDiagnostics => Ok(()),
            Step::Tunnel => match self.plan.tunnel_provider() {
                "cloudflare" if self.plan.cloudflare_token.trim().is_empty() => {
                    bail!("Cloudflare tunnel token is required when tunnel provider is cloudflare")
                }
                "ngrok" if self.plan.ngrok_auth_token.trim().is_empty() => {
                    bail!("ngrok auth token is required when tunnel provider is ngrok")
                }
                _ => Ok(()),
            },
            Step::TunnelDiagnostics => Ok(()),
            Step::Review => {
                self.validate_all()?;
                let failures = self.blocking_diagnostic_failures();
                if !failures.is_empty() && !self.plan.allow_failed_diagnostics {
                    bail!(
                        "Blocking diagnostics failed: {}. Re-run checks or enable 'Allow failed diagnostics' to continue.",
                        failures.join(", ")
                    );
                }
                if let Some(config_path) = self.selected_config_path()? {
                    if config_path.exists() && !self.plan.force_overwrite {
                        bail!(
                            "Config already exists at {}. Enable overwrite to continue.",
                            config_path.display()
                        )
                    }
                }
                Ok(())
            }
        }
    }

    fn validate_all(&self) -> Result<()> {
        for step in [
            Step::Workspace,
            Step::Provider,
            Step::ProviderDiagnostics,
            Step::Runtime,
            Step::Channels,
            Step::ChannelDiagnostics,
            Step::Tunnel,
            Step::TunnelDiagnostics,
        ] {
            self.validate_step(step)?;
        }
        Ok(())
    }

    fn blocking_diagnostic_failures(&self) -> Vec<String> {
        let mut failures = Vec::new();

        if self.provider_probe.is_failed() {
            failures.push("provider".to_string());
        }
        if self.plan.enable_telegram && self.telegram_probe.is_failed() {
            failures.push("telegram".to_string());
        }
        if self.plan.enable_discord && self.discord_probe.is_failed() {
            failures.push("discord".to_string());
        }
        match self.plan.tunnel_provider() {
            "cloudflare" if self.cloudflare_probe.is_failed() => {
                failures.push("cloudflare-tunnel".to_string());
            }
            "ngrok" if self.ngrok_probe.is_failed() => {
                failures.push("ngrok-tunnel".to_string());
            }
            _ => {}
        }

        failures
    }

    fn provider_probe_details(&self) -> String {
        let provider = self.plan.provider();
        match &self.provider_probe {
            CheckStatus::NotRun => {
                format!("{provider}: probe not run yet (model listing endpoint check).")
            }
            CheckStatus::Passed(details) => format!("{provider}: {details}"),
            CheckStatus::Failed(details) => format!("{provider}: {details}"),
            CheckStatus::Skipped(details) => format!("{provider}: {details}"),
        }
    }

    fn provider_probe_remediation(&self) -> String {
        let provider = self.plan.provider();
        match &self.provider_probe {
            CheckStatus::NotRun => "Run provider probe with Enter or r.".to_string(),
            CheckStatus::Passed(_) => "No provider remediation required.".to_string(),
            CheckStatus::Skipped(details) => {
                if details.contains("missing API key") {
                    format!(
                        "Set a {provider} API key in AI Provider step, or switch to ollama for local usage."
                    )
                } else {
                    format!("Review skip reason and re-run: {details}")
                }
            }
            CheckStatus::Failed(details) => {
                if provider == "ollama" {
                    "Start Ollama (`ollama serve`) and verify http://127.0.0.1:11434 is reachable."
                        .to_string()
                } else if contains_http_status(details, 401) || contains_http_status(details, 403) {
                    format!(
                        "Verify {provider} API key permissions and organization scope, then re-run."
                    )
                } else if looks_like_network_error(details) {
                    "Check network/firewall/proxy access to provider API, then re-run probe."
                        .to_string()
                } else {
                    format!("Resolve provider error and re-run probe: {details}")
                }
            }
        }
    }

    fn telegram_probe_details(&self) -> String {
        if !self.plan.enable_telegram {
            return "Telegram channel disabled.".to_string();
        }

        let allow_count = parse_csv_list(&self.plan.telegram_allowed_users).len();
        let allowlist_summary = if allow_count == 0 {
            "allowlist empty (messages blocked until populated)".to_string()
        } else {
            format!("allowlist entries: {allow_count}")
        };

        match &self.telegram_probe {
            CheckStatus::NotRun => format!("Telegram probe not run; {allowlist_summary}."),
            CheckStatus::Passed(details) => format!("{details}; {allowlist_summary}."),
            CheckStatus::Failed(details) => format!("{details}; {allowlist_summary}."),
            CheckStatus::Skipped(details) => format!("{details}; {allowlist_summary}."),
        }
    }

    fn telegram_probe_remediation(&self) -> String {
        if !self.plan.enable_telegram {
            return "Enable Telegram in Channels step to run diagnostics.".to_string();
        }

        let allow_count = parse_csv_list(&self.plan.telegram_allowed_users).len();
        match &self.telegram_probe {
            CheckStatus::NotRun => "Run Telegram test with Enter or r.".to_string(),
            CheckStatus::Passed(_) if allow_count == 0 => {
                "Optional hardening: add Telegram allowlist entries before production use."
                    .to_string()
            }
            CheckStatus::Passed(_) => "No Telegram remediation required.".to_string(),
            CheckStatus::Skipped(details) => format!("Review skip reason and re-run: {details}"),
            CheckStatus::Failed(details) => {
                let lower = details.to_ascii_lowercase();
                if details.contains("bot token is empty") {
                    "Set Telegram bot token from @BotFather in Channels step.".to_string()
                } else if contains_http_status(details, 401)
                    || contains_http_status(details, 403)
                    || lower.contains("unauthorized")
                {
                    "Regenerate Telegram bot token in @BotFather and update Channels step."
                        .to_string()
                } else if looks_like_network_error(details) {
                    "Verify connectivity to api.telegram.org (proxy/firewall/DNS), then re-run."
                        .to_string()
                } else {
                    format!("Resolve Telegram error and re-run: {details}")
                }
            }
        }
    }

    fn discord_probe_details(&self) -> String {
        if !self.plan.enable_discord {
            return "Discord channel disabled.".to_string();
        }

        let guild_id = self.plan.discord_guild_id.trim();
        let guild_scope = if guild_id.is_empty() {
            "guild scope: all guilds visible to bot token".to_string()
        } else {
            format!("guild scope: {guild_id}")
        };
        let allow_count = parse_csv_list(&self.plan.discord_allowed_users).len();
        let allowlist_summary = if allow_count == 0 {
            "allowlist empty (messages blocked until populated)".to_string()
        } else {
            format!("allowlist entries: {allow_count}")
        };

        match &self.discord_probe {
            CheckStatus::NotRun => {
                format!("Discord probe not run; {guild_scope}; {allowlist_summary}.")
            }
            CheckStatus::Passed(details) => {
                format!("{details}; {guild_scope}; {allowlist_summary}.")
            }
            CheckStatus::Failed(details) => {
                format!("{details}; {guild_scope}; {allowlist_summary}.")
            }
            CheckStatus::Skipped(details) => {
                format!("{details}; {guild_scope}; {allowlist_summary}.")
            }
        }
    }

    fn discord_probe_remediation(&self) -> String {
        if !self.plan.enable_discord {
            return "Enable Discord in Channels step to run diagnostics.".to_string();
        }

        let allow_count = parse_csv_list(&self.plan.discord_allowed_users).len();
        match &self.discord_probe {
            CheckStatus::NotRun => "Run Discord test with Enter or r.".to_string(),
            CheckStatus::Passed(_) if allow_count == 0 => {
                "Optional hardening: add Discord allowlist user IDs before production use."
                    .to_string()
            }
            CheckStatus::Passed(_) => "No Discord remediation required.".to_string(),
            CheckStatus::Skipped(details) => format!("Review skip reason and re-run: {details}"),
            CheckStatus::Failed(details) => {
                let lower = details.to_ascii_lowercase();
                if details.contains("bot token is empty") {
                    "Set Discord bot token from Discord Developer Portal in Channels step."
                        .to_string()
                } else if contains_http_status(details, 401)
                    || contains_http_status(details, 403)
                    || lower.contains("unauthorized")
                {
                    "Rotate Discord bot token and verify bot app permissions/intents.".to_string()
                } else if details.contains("not found in bot membership") {
                    "Invite bot to target guild or correct Guild ID, then re-run.".to_string()
                } else if looks_like_network_error(details) {
                    "Verify connectivity to discord.com API (proxy/firewall/DNS), then re-run."
                        .to_string()
                } else {
                    format!("Resolve Discord error and re-run: {details}")
                }
            }
        }
    }

    fn cloudflare_probe_details(&self) -> String {
        match &self.cloudflare_probe {
            CheckStatus::NotRun => {
                "Cloudflare token probe not run; validates local token structure only.".to_string()
            }
            CheckStatus::Passed(details) => {
                format!("Token payload decoded successfully ({details}).")
            }
            CheckStatus::Failed(details) => format!("Token decode failed ({details})."),
            CheckStatus::Skipped(details) => details.clone(),
        }
    }

    fn cloudflare_probe_remediation(&self) -> String {
        match &self.cloudflare_probe {
            CheckStatus::NotRun => "Run Cloudflare token probe with Enter or r.".to_string(),
            CheckStatus::Passed(_) => "No Cloudflare token remediation required.".to_string(),
            CheckStatus::Skipped(details) => format!("Review skip reason and re-run: {details}"),
            CheckStatus::Failed(details) => {
                if details.contains("token is empty") {
                    "Set Cloudflare tunnel token in Tunnel step.".to_string()
                } else if details.contains("JWT-like") {
                    "Use full token from Cloudflare Zero Trust (cloudflared tunnel token output)."
                        .to_string()
                } else if details.contains("payload decode failed") {
                    "Token appears truncated/corrupted; paste a fresh Cloudflare token.".to_string()
                } else if details.contains("payload parse failed") {
                    "Regenerate tunnel token in Cloudflare dashboard and retry.".to_string()
                } else {
                    format!("Resolve Cloudflare token error and re-run: {details}")
                }
            }
        }
    }

    fn ngrok_probe_details(&self) -> String {
        let domain_note = if self.plan.ngrok_domain.trim().is_empty() {
            "custom domain: not set".to_string()
        } else {
            format!(
                "custom domain: {} (domain ownership not validated here)",
                self.plan.ngrok_domain.trim()
            )
        };

        match &self.ngrok_probe {
            CheckStatus::NotRun => format!("ngrok probe not run; {domain_note}."),
            CheckStatus::Passed(details) => format!("{details}; {domain_note}."),
            CheckStatus::Failed(details) => format!("{details}; {domain_note}."),
            CheckStatus::Skipped(details) => format!("{details}; {domain_note}."),
        }
    }

    fn ngrok_probe_remediation(&self) -> String {
        match &self.ngrok_probe {
            CheckStatus::NotRun => "Run ngrok API probe with Enter or r.".to_string(),
            CheckStatus::Passed(_) => "No ngrok remediation required.".to_string(),
            CheckStatus::Skipped(details) => format!("Review skip reason and re-run: {details}"),
            CheckStatus::Failed(details) => {
                if details.contains("auth token is empty") {
                    "Set ngrok auth token in Tunnel step.".to_string()
                } else if contains_http_status(details, 401) || contains_http_status(details, 403) {
                    "Rotate/verify ngrok API token in dashboard.ngrok.com and re-run.".to_string()
                } else if looks_like_network_error(details) {
                    "Verify connectivity to api.ngrok.com (proxy/firewall/DNS), then re-run."
                        .to_string()
                } else {
                    format!("Resolve ngrok error and re-run: {details}")
                }
            }
        }
    }

    fn selected_config_path(&self) -> Result<Option<PathBuf>> {
        let trimmed = self.plan.workspace_path.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let expanded = shellexpand::tilde(trimmed).to_string();
        let path = PathBuf::from(expanded);
        let (config_dir, _) = crate::config::schema::resolve_config_dir_for_workspace(&path);
        Ok(Some(config_dir.join("config.toml")))
    }

    fn start_editing(&mut self) {
        if self.editing.is_some() {
            return;
        }

        let Some(field_key) = self.current_field_key() else {
            return;
        };

        let (value, secret) = match field_key {
            FieldKey::WorkspacePath => (self.plan.workspace_path.clone(), false),
            FieldKey::ApiKey => (self.plan.api_key.clone(), true),
            FieldKey::Model => (self.plan.model.clone(), false),
            FieldKey::TelegramToken => (self.plan.telegram_token.clone(), true),
            FieldKey::TelegramAllowedUsers => (self.plan.telegram_allowed_users.clone(), false),
            FieldKey::DiscordToken => (self.plan.discord_token.clone(), true),
            FieldKey::DiscordGuildId => (self.plan.discord_guild_id.clone(), false),
            FieldKey::DiscordAllowedUsers => (self.plan.discord_allowed_users.clone(), false),
            FieldKey::CloudflareToken => (self.plan.cloudflare_token.clone(), true),
            FieldKey::NgrokAuthToken => (self.plan.ngrok_auth_token.clone(), true),
            FieldKey::NgrokDomain => (self.plan.ngrok_domain.clone(), false),
            _ => return,
        };

        self.editing = Some(EditingState {
            key: field_key,
            value,
            secret,
        });
        self.status =
            "Editing field: Enter or Ctrl+S saves, Esc cancels, Ctrl+U clears.".to_string();
    }

    fn commit_editing(&mut self) {
        let Some(editing) = self.editing.take() else {
            return;
        };

        let value = editing.value.trim().to_string();
        match editing.key {
            FieldKey::WorkspacePath => self.plan.workspace_path = value,
            FieldKey::ApiKey => {
                self.plan.api_key = value;
                self.provider_probe = CheckStatus::NotRun;
            }
            FieldKey::Model => {
                self.plan.model = value;
                self.model_touched = true;
                self.provider_probe = CheckStatus::NotRun;
            }
            FieldKey::TelegramToken => {
                self.plan.telegram_token = value;
                self.telegram_probe = CheckStatus::NotRun;
            }
            FieldKey::TelegramAllowedUsers => self.plan.telegram_allowed_users = value,
            FieldKey::DiscordToken => {
                self.plan.discord_token = value;
                self.discord_probe = CheckStatus::NotRun;
            }
            FieldKey::DiscordGuildId => {
                self.plan.discord_guild_id = value;
                self.discord_probe = CheckStatus::NotRun;
            }
            FieldKey::DiscordAllowedUsers => self.plan.discord_allowed_users = value,
            FieldKey::CloudflareToken => {
                self.plan.cloudflare_token = value;
                self.cloudflare_probe = CheckStatus::NotRun;
            }
            FieldKey::NgrokAuthToken => {
                self.plan.ngrok_auth_token = value;
                self.ngrok_probe = CheckStatus::NotRun;
            }
            FieldKey::NgrokDomain => {
                self.plan.ngrok_domain = value;
                self.ngrok_probe = CheckStatus::NotRun;
            }
            _ => {}
        }
        self.status = "Field updated".to_string();
    }

    fn cancel_editing(&mut self) {
        self.editing = None;
        self.status = "Edit canceled".to_string();
    }

    fn adjust_current_field(&mut self, direction: i8) {
        let Some(field_key) = self.current_field_key() else {
            return;
        };

        match field_key {
            FieldKey::ForceOverwrite => {
                self.plan.force_overwrite = !self.plan.force_overwrite;
            }
            FieldKey::Provider => {
                self.plan.provider_idx =
                    advance_index(self.plan.provider_idx, PROVIDER_OPTIONS.len(), direction);
                if !self.model_touched {
                    self.plan.model = provider_default_model(self.plan.provider());
                }
                self.provider_probe = CheckStatus::NotRun;
            }
            FieldKey::MemoryBackend => {
                self.plan.memory_idx =
                    advance_index(self.plan.memory_idx, MEMORY_OPTIONS.len(), direction);
            }
            FieldKey::DisableTotp => {
                self.plan.disable_totp = !self.plan.disable_totp;
            }
            FieldKey::EnableTelegram => {
                self.plan.enable_telegram = !self.plan.enable_telegram;
                self.telegram_probe = CheckStatus::NotRun;
            }
            FieldKey::EnableDiscord => {
                self.plan.enable_discord = !self.plan.enable_discord;
                self.discord_probe = CheckStatus::NotRun;
            }
            FieldKey::AutostartChannels => {
                self.plan.autostart_channels = !self.plan.autostart_channels;
            }
            FieldKey::TunnelProvider => {
                self.plan.tunnel_idx =
                    advance_index(self.plan.tunnel_idx, TUNNEL_OPTIONS.len(), direction);
                self.cloudflare_probe = CheckStatus::NotRun;
                self.ngrok_probe = CheckStatus::NotRun;
            }
            FieldKey::AllowFailedDiagnostics => {
                self.plan.allow_failed_diagnostics = !self.plan.allow_failed_diagnostics;
            }
            _ => {}
        }

        self.clamp_focus();
    }

    fn next_step(&mut self) -> Result<()> {
        self.validate_step(self.step)?;
        self.step = self.step.next();
        self.focus = 0;
        self.clamp_focus();
        self.status = format!("Moved to {}", self.step.title());
        Ok(())
    }

    fn previous_step(&mut self) {
        self.step = self.step.previous();
        self.focus = 0;
        self.clamp_focus();
        self.status = format!("Moved to {}", self.step.title());
    }

    fn run_probe_for_field(&mut self, field_key: FieldKey) -> bool {
        match field_key {
            FieldKey::RunProviderProbe => {
                self.status = "Running provider probe...".to_string();
                self.provider_probe = run_provider_probe(&self.plan);
                self.status = format!("Provider probe {}", self.provider_probe.badge());
                true
            }
            FieldKey::RunTelegramProbe => {
                self.status = "Running Telegram probe...".to_string();
                self.telegram_probe = run_telegram_probe(&self.plan);
                self.status = format!("Telegram probe {}", self.telegram_probe.badge());
                true
            }
            FieldKey::RunDiscordProbe => {
                self.status = "Running Discord probe...".to_string();
                self.discord_probe = run_discord_probe(&self.plan);
                self.status = format!("Discord probe {}", self.discord_probe.badge());
                true
            }
            FieldKey::RunCloudflareProbe => {
                self.status = "Running Cloudflare token probe...".to_string();
                self.cloudflare_probe = run_cloudflare_probe(&self.plan);
                self.status = format!("Cloudflare probe {}", self.cloudflare_probe.badge());
                true
            }
            FieldKey::RunNgrokProbe => {
                self.status = "Running ngrok API probe...".to_string();
                self.ngrok_probe = run_ngrok_probe(&self.plan);
                self.status = format!("ngrok probe {}", self.ngrok_probe.badge());
                true
            }
            _ => false,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<LoopAction> {
        if let Some(editing) = self.editing.as_mut() {
            match key.code {
                KeyCode::Esc => self.cancel_editing(),
                KeyCode::Enter => self.commit_editing(),
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.commit_editing();
                }
                KeyCode::Backspace => {
                    editing.value.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    editing.value.clear();
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    editing.value.push(ch);
                }
                _ => {}
            }
            return Ok(LoopAction::Continue);
        }

        match key.code {
            KeyCode::Char('q') => return Ok(LoopAction::Cancel),
            KeyCode::PageDown => {
                self.next_step()?;
            }
            KeyCode::PageUp => {
                self.previous_step();
            }
            KeyCode::Char('n')
                if key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.next_step()?;
            }
            KeyCode::Char('p')
                if key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.previous_step();
            }
            KeyCode::Up => self.move_focus(-1),
            KeyCode::Down => self.move_focus(1),
            KeyCode::Char('k') if key.modifiers.is_empty() => self.move_focus(-1),
            KeyCode::Char('j') if key.modifiers.is_empty() => self.move_focus(1),
            KeyCode::Tab => self.move_focus(1),
            KeyCode::BackTab => self.move_focus(-1),
            KeyCode::Left => self.adjust_current_field(-1),
            KeyCode::Right => self.adjust_current_field(1),
            KeyCode::Char('h') if key.modifiers.is_empty() => self.adjust_current_field(-1),
            KeyCode::Char('l') if key.modifiers.is_empty() => self.adjust_current_field(1),
            KeyCode::Char(' ') => self.adjust_current_field(1),
            KeyCode::Char('r') => {
                if let Some(field_key) = self.current_field_key() {
                    let _ = self.run_probe_for_field(field_key);
                }
            }
            KeyCode::Char('e') if key.modifiers.is_empty() => {
                if let Some(field_key) = self.current_field_key() {
                    if is_text_input_field(field_key) {
                        self.start_editing();
                    }
                }
            }
            KeyCode::Char('a') if self.step == Step::Review => {
                self.validate_step(Step::Review)?;
                return Ok(LoopAction::Submit);
            }
            KeyCode::Char('s')
                if self.step == Step::Review
                    && (key.modifiers.is_empty()
                        || key.modifiers.contains(KeyModifiers::CONTROL)) =>
            {
                self.validate_step(Step::Review)?;
                return Ok(LoopAction::Submit);
            }
            KeyCode::Enter => {
                if self.step == Step::Review {
                    self.validate_step(Step::Review)?;
                    return Ok(LoopAction::Submit);
                }

                match self.current_field_key() {
                    Some(FieldKey::Continue) => self.next_step()?,
                    Some(field_key @ FieldKey::RunProviderProbe)
                    | Some(field_key @ FieldKey::RunTelegramProbe)
                    | Some(field_key @ FieldKey::RunDiscordProbe)
                    | Some(field_key @ FieldKey::RunCloudflareProbe)
                    | Some(field_key @ FieldKey::RunNgrokProbe) => {
                        let _ = self.run_probe_for_field(field_key);
                    }
                    Some(field_key) if is_text_input_field(field_key) => self.start_editing(),
                    Some(_) | None => self.adjust_current_field(1),
                }
            }
            _ => {}
        }

        self.clamp_focus();
        Ok(LoopAction::Continue)
    }

    fn review_text(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "Workspace: {}",
            self.plan.workspace_path.trim().if_empty("(empty)")
        ));
        if let Ok(Some(path)) = self.selected_config_path() {
            lines.push(format!("Config path: {}", path.display()));
            if path.exists() {
                lines.push(if self.plan.force_overwrite {
                    "Overwrite existing config: enabled".to_string()
                } else {
                    "Overwrite existing config: disabled (will block apply)".to_string()
                });
            }
        }

        lines.push(format!("Provider: {}", self.plan.provider()));
        lines.push(format!(
            "Model: {}",
            self.plan.model.trim().if_empty("(empty)")
        ));
        lines.push(format!(
            "API key: {}",
            if self.plan.api_key.trim().is_empty() {
                "not set"
            } else {
                "set"
            }
        ));
        lines.push(format!(
            "Provider diagnostics: {}",
            self.provider_probe.as_line()
        ));
        lines.push(format!("Memory backend: {}", self.plan.memory_backend()));
        lines.push(format!(
            "TOTP: {}",
            if self.plan.disable_totp {
                "disabled"
            } else {
                "enabled"
            }
        ));

        let mut channel_notes = vec!["CLI".to_string()];
        if self.plan.enable_telegram {
            channel_notes.push("Telegram".to_string());
        }
        if self.plan.enable_discord {
            channel_notes.push("Discord".to_string());
        }
        lines.push(format!("Channels: {}", channel_notes.join(", ")));
        if self.plan.enable_telegram {
            lines.push(format!(
                "Telegram diagnostics: {}",
                self.telegram_probe.as_line()
            ));
        }
        if self.plan.enable_discord {
            lines.push(format!(
                "Discord diagnostics: {}",
                self.discord_probe.as_line()
            ));
        }
        lines.push(format!("Tunnel: {}", self.plan.tunnel_provider()));
        if self.plan.tunnel_provider() == "cloudflare" {
            lines.push(format!(
                "Cloudflare diagnostics: {}",
                self.cloudflare_probe.as_line()
            ));
        } else if self.plan.tunnel_provider() == "ngrok" {
            lines.push(format!("ngrok diagnostics: {}", self.ngrok_probe.as_line()));
        }
        lines.push(format!(
            "Allow failed diagnostics: {}",
            if self.plan.allow_failed_diagnostics {
                "yes"
            } else {
                "no"
            }
        ));
        let blockers = self.blocking_diagnostic_failures();
        lines.push(if blockers.is_empty() {
            "Blocking diagnostic failures: none".to_string()
        } else {
            format!("Blocking diagnostic failures: {}", blockers.join(", "))
        });
        lines.push(format!(
            "Autostart channels: {}",
            if self.plan.autostart_channels {
                "yes"
            } else {
                "no"
            }
        ));

        lines.join("\n")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopAction {
    Continue,
    Submit,
    Cancel,
}

fn probe_http_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .context("failed to build probe HTTP client")
}

fn run_provider_probe(plan: &TuiOnboardPlan) -> CheckStatus {
    let provider = plan.provider();
    let api_key = plan.api_key.trim();

    match provider {
        "openrouter" => {
            if api_key.is_empty() {
                return CheckStatus::Skipped(
                    "missing API key (required for OpenRouter probe)".to_string(),
                );
            }
            let client = match probe_http_client() {
                Ok(client) => client,
                Err(error) => return CheckStatus::Failed(error.to_string()),
            };
            match client
                .get("https://openrouter.ai/api/v1/models")
                .header("Authorization", format!("Bearer {api_key}"))
                .send()
            {
                Ok(response) if response.status().is_success() => {
                    CheckStatus::Passed("models endpoint reachable".to_string())
                }
                Ok(response) => CheckStatus::Failed(format!("HTTP {}", response.status())),
                Err(error) => CheckStatus::Failed(error.to_string()),
            }
        }
        "openai" => {
            if api_key.is_empty() {
                return CheckStatus::Skipped(
                    "missing API key (required for OpenAI probe)".to_string(),
                );
            }
            let client = match probe_http_client() {
                Ok(client) => client,
                Err(error) => return CheckStatus::Failed(error.to_string()),
            };
            match client
                .get("https://api.openai.com/v1/models")
                .header("Authorization", format!("Bearer {api_key}"))
                .send()
            {
                Ok(response) if response.status().is_success() => {
                    CheckStatus::Passed("models endpoint reachable".to_string())
                }
                Ok(response) => CheckStatus::Failed(format!("HTTP {}", response.status())),
                Err(error) => CheckStatus::Failed(error.to_string()),
            }
        }
        "anthropic" => {
            if api_key.is_empty() {
                return CheckStatus::Skipped(
                    "missing API key (required for Anthropic probe)".to_string(),
                );
            }
            let client = match probe_http_client() {
                Ok(client) => client,
                Err(error) => return CheckStatus::Failed(error.to_string()),
            };
            match client
                .get("https://api.anthropic.com/v1/models")
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .send()
            {
                Ok(response) if response.status().is_success() => {
                    CheckStatus::Passed("models endpoint reachable".to_string())
                }
                Ok(response) => CheckStatus::Failed(format!("HTTP {}", response.status())),
                Err(error) => CheckStatus::Failed(error.to_string()),
            }
        }
        "gemini" => {
            if api_key.is_empty() {
                return CheckStatus::Skipped(
                    "missing API key (required for Gemini probe)".to_string(),
                );
            }
            let client = match probe_http_client() {
                Ok(client) => client,
                Err(error) => return CheckStatus::Failed(error.to_string()),
            };
            let url =
                format!("https://generativelanguage.googleapis.com/v1beta/models?key={api_key}");
            match client.get(url).send() {
                Ok(response) if response.status().is_success() => {
                    CheckStatus::Passed("models endpoint reachable".to_string())
                }
                Ok(response) => CheckStatus::Failed(format!("HTTP {}", response.status())),
                Err(error) => CheckStatus::Failed(error.to_string()),
            }
        }
        "ollama" => {
            let client = match probe_http_client() {
                Ok(client) => client,
                Err(error) => return CheckStatus::Failed(error.to_string()),
            };
            match client.get("http://127.0.0.1:11434/api/tags").send() {
                Ok(response) if response.status().is_success() => {
                    CheckStatus::Passed("local Ollama reachable".to_string())
                }
                Ok(response) => CheckStatus::Failed(format!("HTTP {}", response.status())),
                Err(error) => CheckStatus::Failed(error.to_string()),
            }
        }
        _ => CheckStatus::Skipped(format!("no probe implemented for provider `{provider}`")),
    }
}

fn run_telegram_probe(plan: &TuiOnboardPlan) -> CheckStatus {
    if !plan.enable_telegram {
        return CheckStatus::Skipped("telegram channel is disabled".to_string());
    }

    let token = plan.telegram_token.trim();
    if token.is_empty() {
        return CheckStatus::Failed("telegram bot token is empty".to_string());
    }

    let client = match probe_http_client() {
        Ok(client) => client,
        Err(error) => return CheckStatus::Failed(error.to_string()),
    };

    let url = format!("https://api.telegram.org/bot{token}/getMe");
    let response = match client.get(url).send() {
        Ok(response) => response,
        Err(error) => return CheckStatus::Failed(error.to_string()),
    };
    if !response.status().is_success() {
        return CheckStatus::Failed(format!("HTTP {}", response.status()));
    }

    let json: Value = match response.json() {
        Ok(json) => json,
        Err(error) => return CheckStatus::Failed(error.to_string()),
    };

    if json.get("ok").and_then(Value::as_bool) == Some(true) {
        let username = json
            .pointer("/result/username")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return CheckStatus::Passed(format!("token accepted (bot @{username})"));
    }

    let description = json
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("unknown Telegram error");
    CheckStatus::Failed(description.to_string())
}

fn run_discord_probe(plan: &TuiOnboardPlan) -> CheckStatus {
    if !plan.enable_discord {
        return CheckStatus::Skipped("discord channel is disabled".to_string());
    }

    let token = plan.discord_token.trim();
    if token.is_empty() {
        return CheckStatus::Failed("discord bot token is empty".to_string());
    }

    let client = match probe_http_client() {
        Ok(client) => client,
        Err(error) => return CheckStatus::Failed(error.to_string()),
    };

    let response = match client
        .get("https://discord.com/api/v10/users/@me/guilds")
        .header("Authorization", format!("Bot {token}"))
        .send()
    {
        Ok(response) => response,
        Err(error) => return CheckStatus::Failed(error.to_string()),
    };

    if !response.status().is_success() {
        return CheckStatus::Failed(format!("HTTP {}", response.status()));
    }

    let json: Value = match response.json() {
        Ok(json) => json,
        Err(error) => return CheckStatus::Failed(error.to_string()),
    };

    let guilds = match json.as_array() {
        Some(guilds) => guilds,
        None => return CheckStatus::Failed("unexpected Discord response payload".to_string()),
    };

    let guild_id = plan.discord_guild_id.trim();
    if guild_id.is_empty() {
        return CheckStatus::Passed(format!("token accepted ({} guilds visible)", guilds.len()));
    }

    let matched = guilds.iter().any(|guild| {
        guild
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id == guild_id)
    });
    if matched {
        CheckStatus::Passed(format!("guild {guild_id} visible to bot token"))
    } else {
        CheckStatus::Failed(format!("guild {guild_id} not found in bot membership"))
    }
}

fn run_cloudflare_probe(plan: &TuiOnboardPlan) -> CheckStatus {
    if plan.tunnel_provider() != "cloudflare" {
        return CheckStatus::Skipped("cloudflare tunnel is not selected".to_string());
    }

    let token = plan.cloudflare_token.trim();
    if token.is_empty() {
        return CheckStatus::Failed("cloudflare tunnel token is empty".to_string());
    }

    let mut segments = token.split('.');
    let (Some(_header), Some(payload), Some(_signature), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return CheckStatus::Failed("token is not JWT-like (expected 3 segments)".to_string());
    };

    let decoded = match URL_SAFE_NO_PAD.decode(payload.as_bytes()) {
        Ok(decoded) => decoded,
        Err(error) => return CheckStatus::Failed(format!("payload decode failed: {error}")),
    };
    let payload_json: Value = match serde_json::from_slice(&decoded) {
        Ok(json) => json,
        Err(error) => return CheckStatus::Failed(format!("payload parse failed: {error}")),
    };

    let aud = payload_json
        .get("aud")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let subject = payload_json
        .get("sub")
        .and_then(Value::as_str)
        .unwrap_or("unknown");

    CheckStatus::Passed(format!("jwt parsed (aud={aud}, sub={subject})"))
}

fn run_ngrok_probe(plan: &TuiOnboardPlan) -> CheckStatus {
    if plan.tunnel_provider() != "ngrok" {
        return CheckStatus::Skipped("ngrok tunnel is not selected".to_string());
    }

    let token = plan.ngrok_auth_token.trim();
    if token.is_empty() {
        return CheckStatus::Failed("ngrok auth token is empty".to_string());
    }

    let client = match probe_http_client() {
        Ok(client) => client,
        Err(error) => return CheckStatus::Failed(error.to_string()),
    };

    let response = match client
        .get("https://api.ngrok.com/tunnels")
        .header("Authorization", format!("Bearer {token}"))
        .header("Ngrok-Version", "2")
        .send()
    {
        Ok(response) => response,
        Err(error) => return CheckStatus::Failed(error.to_string()),
    };

    if !response.status().is_success() {
        return CheckStatus::Failed(format!("HTTP {}", response.status()));
    }

    let json: Value = match response.json() {
        Ok(json) => json,
        Err(error) => return CheckStatus::Failed(error.to_string()),
    };
    let count = json
        .get("tunnels")
        .and_then(Value::as_array)
        .map_or(0, std::vec::Vec::len);

    CheckStatus::Passed(format!("token accepted ({} active tunnels)", count))
}

pub async fn run_wizard_tui(force: bool) -> Result<Config> {
    run_wizard_tui_with_migration(force, OpenClawOnboardMigrationOptions::default()).await
}

pub async fn run_wizard_tui_with_migration(
    force: bool,
    migration_options: OpenClawOnboardMigrationOptions,
) -> Result<Config> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("TUI onboarding requires an interactive terminal")
    }

    let (_, default_workspace_dir) =
        crate::config::schema::resolve_runtime_dirs_for_onboarding().await?;

    let plan = tokio::task::spawn_blocking(move || run_tui_session(default_workspace_dir, force))
        .await
        .context("TUI onboarding thread failed")??;

    let workspace_value = plan.workspace_path.trim();
    if workspace_value.is_empty() {
        bail!("Workspace path is required")
    }

    let expanded_workspace = shellexpand::tilde(workspace_value).to_string();
    let selected_workspace = PathBuf::from(expanded_workspace);
    let (config_dir, resolved_workspace_dir) =
        crate::config::schema::resolve_config_dir_for_workspace(&selected_workspace);
    let config_path = config_dir.join("config.toml");

    if config_path.exists() && !plan.force_overwrite {
        bail!(
            "Config already exists at {}. Re-run with --force or enable overwrite inside TUI.",
            config_path.display()
        );
    }

    let _workspace_guard = ScopedEnvVar::set(
        "ZEROCLAW_WORKSPACE",
        resolved_workspace_dir.to_string_lossy().as_ref(),
    );

    let provider = plan.provider().to_string();
    let model = if plan.model.trim().is_empty() {
        provider_default_model(&provider)
    } else {
        plan.model.trim().to_string()
    };
    let memory_backend = plan.memory_backend().to_string();
    let api_key = (!plan.api_key.trim().is_empty()).then_some(plan.api_key.trim());

    let mut config = run_quick_setup_with_migration(
        api_key,
        Some(&provider),
        Some(&model),
        Some(&memory_backend),
        true,
        plan.disable_totp,
        migration_options,
    )
    .await?;

    apply_channel_overrides(&mut config, &plan);
    apply_tunnel_overrides(&mut config, &plan);
    config.save().await?;

    if plan.autostart_channels && has_launchable_channels(&config.channels_config) {
        std::env::set_var("ZEROCLAW_AUTOSTART_CHANNELS", "1");
    }

    println!();
    println!(
        "  {} {}",
        style("✓").green().bold(),
        style("TUI onboarding complete.").white().bold()
    );
    println!(
        "  {} {}",
        style("Provider:").cyan().bold(),
        style(config.default_provider.as_deref().unwrap_or("openrouter")).green()
    );
    println!(
        "  {} {}",
        style("Model:").cyan().bold(),
        style(config.default_model.as_deref().unwrap_or("(default)")).green()
    );
    let tunnel_summary = match plan.tunnel_provider() {
        "none" => "none (local only)".to_string(),
        "cloudflare" => "cloudflare".to_string(),
        "ngrok" => "ngrok".to_string(),
        other => other.to_string(),
    };
    println!(
        "  {} {}",
        style("Tunnel:").cyan().bold(),
        style(tunnel_summary).green()
    );
    println!(
        "  {} {}",
        style("Config:").cyan().bold(),
        style(config.config_path.display()).green()
    );
    println!();

    Ok(config)
}

fn run_tui_session(default_workspace_dir: PathBuf, force: bool) -> Result<TuiOnboardPlan> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("TUI onboarding requires an interactive terminal")
    }

    enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide).context("failed to enter alternate screen")?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to initialize terminal backend")?;
    let mut state = TuiState::new(default_workspace_dir, force);

    let result = run_tui_loop(&mut terminal, &mut state);
    let restore = restore_terminal(&mut terminal);

    match (result, restore) {
        (Ok(plan), Ok(())) => Ok(plan),
        (Err(err), Ok(())) => Err(err),
        (Ok(_), Err(restore_err)) => Err(restore_err),
        (Err(err), Err(_restore_err)) => Err(err),
    }
}

#[allow(clippy::too_many_lines)]
fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut TuiState,
) -> Result<TuiOnboardPlan> {
    loop {
        terminal
            .draw(|frame| draw_ui(frame, state))
            .context("failed to draw onboarding UI")?;

        if !event::poll(Duration::from_millis(120)).context("failed to poll terminal events")? {
            continue;
        }

        let event = event::read().context("failed to read terminal event")?;
        let Event::Key(key) = event else {
            continue;
        };

        match state.handle_key(key) {
            Ok(LoopAction::Continue) => {}
            Ok(LoopAction::Submit) => {
                state.validate_step(Step::Review)?;
                return Ok(state.plan.clone());
            }
            Ok(LoopAction::Cancel) => bail!("Onboarding canceled by user"),
            Err(error) => {
                state.status = error.to_string();
            }
        }
    }
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode().context("failed to disable raw mode")?;
    execute!(terminal.backend_mut(), Show, LeaveAlternateScreen)
        .context("failed to leave alternate screen")?;
    terminal.show_cursor().context("failed to restore cursor")?;
    Ok(())
}

fn draw_ui(frame: &mut Frame<'_>, state: &TuiState) {
    let root = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(7),
        ])
        .split(root);

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "ZeroClaw Onboarding UI",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!(
                "Step {}/{}: {}",
                state.step.index() + 1,
                Step::ORDER.len(),
                state.step.title()
            ),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]))
    .block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(header, chunks[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(26), Constraint::Min(50)])
        .split(chunks[1]);

    render_steps(frame, state, body[0]);

    if state.step == Step::Review {
        render_review(frame, state, body[1]);
    } else {
        render_fields(frame, state, body[1]);
    }

    let footer_text = vec![
        Line::from(vec![
            Span::styled(
                "Help: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(state.step.help()),
        ]),
        Line::from("Nav: ↑/↓ or j/k move | Tab/Shift+Tab cycle fields | n/PgDn next step | p/PgUp back"),
        Line::from("Select: ←/→ or h/l or Space toggles/options | Enter runs checks on probe rows"),
        Line::from("Edit: Enter or e opens text editor | Enter or Ctrl+S saves | Esc cancels | Ctrl+U clears"),
        Line::from("Apply/Quit: Review step -> Enter, a, or s applies config | q quits"),
        Line::from(state.status.as_str()),
    ];

    let footer = Paragraph::new(footer_text)
        .block(Block::default().borders(Borders::TOP))
        .wrap(Wrap { trim: true });
    frame.render_widget(footer, chunks[2]);

    if let Some(editing) = &state.editing {
        render_editor_popup(frame, editing);
    }
}

fn render_steps(frame: &mut Frame<'_>, state: &TuiState, area: Rect) {
    let items: Vec<ListItem<'_>> = Step::ORDER
        .iter()
        .enumerate()
        .map(|(idx, step)| {
            let prefix = if idx < state.step.index() {
                "✓"
            } else {
                "•"
            };
            ListItem::new(Line::from(vec![
                Span::styled(prefix, Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::raw(step.title()),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Flow"))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

    let mut stateful = ListState::default();
    stateful.select(Some(state.step.index()));
    frame.render_stateful_widget(list, area, &mut stateful);
}

fn render_fields(frame: &mut Frame<'_>, state: &TuiState, area: Rect) {
    let fields = state.visible_fields();
    let items: Vec<ListItem<'_>> = fields
        .iter()
        .map(|field| {
            let required = if field.required { "*" } else { " " };
            let label_style = if field.editable {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::DIM)
            };
            let line = Line::from(vec![
                Span::styled(format!("{} {:<24}", required, field.label), label_style),
                Span::styled(field.value.clone(), Style::default().fg(Color::White)),
            ]);
            let hint = Line::from(Span::styled(
                format!("   {}", field.hint),
                Style::default().fg(Color::DarkGray),
            ));
            ListItem::new(vec![line, hint])
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(state.step.title()),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

    let mut stateful = ListState::default();
    if !fields.is_empty() {
        stateful.select(Some(state.focus.min(fields.len() - 1)));
    }
    frame.render_stateful_widget(list, area, &mut stateful);
}

fn render_review(frame: &mut Frame<'_>, state: &TuiState, area: Rect) {
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(5)])
        .split(area);

    let summary = Paragraph::new(state.review_text())
        .block(Block::default().borders(Borders::ALL).title("Summary"))
        .wrap(Wrap { trim: true });
    frame.render_widget(summary, split[0]);

    render_fields(frame, state, split[1]);
}

fn render_editor_popup(frame: &mut Frame<'_>, editing: &EditingState) {
    let area = centered_rect(70, 25, frame.area());
    frame.render_widget(Clear, area);

    let value = if editing.secret {
        if editing.value.is_empty() {
            "<empty>".to_string()
        } else {
            "*".repeat(editing.value.chars().count().min(24))
        }
    } else if editing.value.is_empty() {
        "<empty>".to_string()
    } else {
        editing.value.clone()
    };

    let input = Paragraph::new(vec![
        Line::from("Type your value, then press Enter or Ctrl+S to save."),
        Line::from("Esc cancels, Ctrl+U clears."),
        Line::from(""),
        Line::from(Span::styled(
            value,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Edit Field")
            .border_style(Style::default().fg(Color::Cyan)),
    )
    .wrap(Wrap { trim: false });

    frame.render_widget(input, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn provider_default_model(provider: &str) -> String {
    default_model_fallback_for_provider(Some(provider)).to_string()
}

fn display_value(value: &str, secret: bool) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "<empty>".to_string();
    }
    if !secret {
        return trimmed.to_string();
    }

    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 4 {
        return "*".repeat(chars.len());
    }

    let suffix: String = chars[chars.len() - 4..].iter().collect();
    format!("{}{}", "*".repeat(chars.len().saturating_sub(4)), suffix)
}

fn bool_label(value: bool) -> String {
    if value {
        "yes".to_string()
    } else {
        "no".to_string()
    }
}

fn contains_http_status(message: &str, code: u16) -> bool {
    message.contains(&format!("HTTP {code}"))
}

fn looks_like_network_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "timeout",
        "timed out",
        "connection",
        "dns",
        "resolve",
        "refused",
        "unreachable",
        "network",
        "tls",
        "socket",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn advance_index(current: usize, len: usize, direction: i8) -> usize {
    if len == 0 {
        return 0;
    }
    if direction < 0 {
        if current == 0 {
            len - 1
        } else {
            current - 1
        }
    } else if current + 1 >= len {
        0
    } else {
        current + 1
    }
}

fn apply_channel_overrides(config: &mut Config, plan: &TuiOnboardPlan) {
    let mut channels = ChannelsConfig::default();

    if plan.enable_telegram {
        channels.telegram = Some(TelegramConfig {
            bot_token: plan.telegram_token.trim().to_string(),
            allowed_users: parse_csv_list(&plan.telegram_allowed_users),
            stream_mode: StreamMode::Off,
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            progress_mode: ProgressMode::Compact,
            group_reply: None,
            base_url: None,
            ack_enabled: true,
        });
    }

    if plan.enable_discord {
        let guild_id = plan.discord_guild_id.trim();
        channels.discord = Some(DiscordConfig {
            bot_token: plan.discord_token.trim().to_string(),
            guild_id: if guild_id.is_empty() {
                None
            } else {
                Some(guild_id.to_string())
            },
            allowed_users: parse_csv_list(&plan.discord_allowed_users),
            listen_to_bots: false,
            mention_only: false,
            group_reply: None,
        });
    }

    config.channels_config = channels;
}

fn apply_tunnel_overrides(config: &mut Config, plan: &TuiOnboardPlan) {
    config.tunnel = match plan.tunnel_provider() {
        "cloudflare" => TunnelConfig {
            provider: "cloudflare".to_string(),
            cloudflare: Some(CloudflareTunnelConfig {
                token: plan.cloudflare_token.trim().to_string(),
            }),
            tailscale: None,
            ngrok: None,
            custom: None,
        },
        "ngrok" => {
            let domain = plan.ngrok_domain.trim();
            TunnelConfig {
                provider: "ngrok".to_string(),
                cloudflare: None,
                tailscale: None,
                ngrok: Some(NgrokTunnelConfig {
                    auth_token: plan.ngrok_auth_token.trim().to_string(),
                    domain: if domain.is_empty() {
                        None
                    } else {
                        Some(domain.to_string())
                    },
                }),
                custom: None,
            }
        }
        _ => TunnelConfig::default(),
    };
}

fn has_launchable_channels(channels: &ChannelsConfig) -> bool {
    channels
        .channels_except_webhook()
        .iter()
        .any(|(_, enabled)| *enabled)
}

fn is_text_input_field(field_key: FieldKey) -> bool {
    matches!(
        field_key,
        FieldKey::WorkspacePath
            | FieldKey::ApiKey
            | FieldKey::Model
            | FieldKey::TelegramToken
            | FieldKey::TelegramAllowedUsers
            | FieldKey::DiscordToken
            | FieldKey::DiscordGuildId
            | FieldKey::DiscordAllowedUsers
            | FieldKey::CloudflareToken
            | FieldKey::NgrokAuthToken
            | FieldKey::NgrokDomain
    )
}

fn parse_csv_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

trait EmptyFallback {
    fn if_empty(self, fallback: &str) -> String;
}

impl EmptyFallback for &str {
    fn if_empty(self, fallback: &str) -> String {
        if self.trim().is_empty() {
            fallback.to_string()
        } else {
            self.to_string()
        }
    }
}

struct ScopedEnvVar {
    key: &'static str,
    previous: Option<String>,
}

impl ScopedEnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn sample_plan() -> TuiOnboardPlan {
        TuiOnboardPlan::new(Path::new("/tmp/zeroclaw-tui-tests").to_path_buf(), false)
    }

    #[test]
    fn parse_csv_list_ignores_empty_segments() {
        let parsed = parse_csv_list("alice, bob ,,carol");
        assert_eq!(parsed, vec!["alice", "bob", "carol"]);
    }

    #[test]
    fn provider_default_model_changes_with_provider() {
        let openai = provider_default_model("openai");
        let anthropic = provider_default_model("anthropic");
        assert_ne!(openai, anthropic);
    }

    #[test]
    fn advance_index_wraps_both_directions() {
        assert_eq!(advance_index(0, 3, -1), 2);
        assert_eq!(advance_index(2, 3, 1), 0);
        assert_eq!(advance_index(1, 3, 1), 2);
    }

    #[test]
    fn cloudflare_probe_fails_for_invalid_token_shape() {
        let mut plan = sample_plan();
        plan.tunnel_idx = 1; // cloudflare
        plan.cloudflare_token = "not-a-jwt".to_string();

        let status = run_cloudflare_probe(&plan);
        assert!(matches!(status, CheckStatus::Failed(_)));
    }

    #[test]
    fn cloudflare_probe_parses_minimal_jwt_payload() {
        let mut plan = sample_plan();
        plan.tunnel_idx = 1; // cloudflare

        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#.as_bytes());
        let payload = URL_SAFE_NO_PAD.encode(r#"{"aud":"demo","sub":"tunnel"}"#.as_bytes());
        plan.cloudflare_token = format!("{header}.{payload}.signature");

        let status = run_cloudflare_probe(&plan);
        assert!(matches!(status, CheckStatus::Passed(_)));
    }

    #[test]
    fn provider_probe_skips_without_required_api_key() {
        let mut plan = sample_plan();
        plan.provider_idx = 1; // openai
        plan.api_key.clear();

        let status = run_provider_probe(&plan);
        assert!(matches!(status, CheckStatus::Skipped(_)));
    }

    #[test]
    fn ngrok_probe_skips_when_tunnel_not_selected() {
        let plan = sample_plan();
        let status = run_ngrok_probe(&plan);
        assert!(matches!(status, CheckStatus::Skipped(_)));
    }

    #[test]
    fn review_validation_blocks_failed_diagnostics_by_default() {
        let workspace = Path::new("/tmp/zeroclaw-review-gate").to_path_buf();
        let mut state = TuiState::new(workspace, true);
        state.provider_probe = CheckStatus::Failed("network timeout".to_string());

        let err = state
            .validate_step(Step::Review)
            .expect_err("review should fail when blocking diagnostics fail");
        assert!(err
            .to_string()
            .contains("Blocking diagnostics failed: provider"));
    }

    #[test]
    fn review_validation_allows_failed_diagnostics_with_override() {
        let workspace = Path::new("/tmp/zeroclaw-review-gate-override").to_path_buf();
        let mut state = TuiState::new(workspace, true);
        state.provider_probe = CheckStatus::Failed("network timeout".to_string());
        state.plan.allow_failed_diagnostics = true;

        state
            .validate_step(Step::Review)
            .expect("override should allow review validation to pass");
    }

    #[test]
    fn step_navigation_resets_focus_to_first_field() {
        let workspace = Path::new("/tmp/zeroclaw-focus-reset").to_path_buf();
        let mut state = TuiState::new(workspace, true);

        state.step = Step::Tunnel;
        state.plan.tunnel_idx = 1; // cloudflare
        state.plan.cloudflare_token = "token.payload.signature".to_string();
        state.focus = 1; // cloudflare token row

        state
            .next_step()
            .expect("tunnel step should validate when token is present");
        assert_eq!(state.step, Step::TunnelDiagnostics);
        assert_eq!(state.focus, 0);

        state.focus = 1; // status row
        state
            .next_step()
            .expect("tunnel diagnostics should advance");
        assert_eq!(state.step, Step::Review);
        assert_eq!(state.focus, 0);

        state.focus = 1;
        state.previous_step();
        assert_eq!(state.step, Step::TunnelDiagnostics);
        assert_eq!(state.focus, 0);
    }

    #[test]
    fn provider_remediation_recommends_api_key_for_skipped_cloud_provider() {
        let workspace = Path::new("/tmp/zeroclaw-provider-remediation").to_path_buf();
        let mut state = TuiState::new(workspace, true);
        state.plan.provider_idx = 1; // openai
        state.provider_probe =
            CheckStatus::Skipped("missing API key (required for OpenAI probe)".to_string());

        let remediation = state.provider_probe_remediation();
        assert!(remediation.contains("API key"));
        assert!(remediation.contains("openai"));
    }

    #[test]
    fn discord_remediation_guides_guild_membership_fix() {
        let workspace = Path::new("/tmp/zeroclaw-discord-remediation").to_path_buf();
        let mut state = TuiState::new(workspace, true);
        state.plan.enable_discord = true;
        state.discord_probe =
            CheckStatus::Failed("guild 1234 not found in bot membership".to_string());

        let remediation = state.discord_probe_remediation();
        assert!(remediation.contains("Invite bot"));
        assert!(remediation.contains("Guild ID"));
    }

    #[test]
    fn cloudflare_remediation_explains_jwt_shape_failures() {
        let workspace = Path::new("/tmp/zeroclaw-cloudflare-remediation").to_path_buf();
        let mut state = TuiState::new(workspace, true);
        state.plan.tunnel_idx = 1; // cloudflare
        state.cloudflare_probe =
            CheckStatus::Failed("token is not JWT-like (expected 3 segments)".to_string());

        let remediation = state.cloudflare_probe_remediation();
        assert!(remediation.contains("Cloudflare Zero Trust"));
    }

    #[test]
    fn provider_diagnostics_page_shows_details_and_remediation_rows() {
        let workspace = Path::new("/tmp/zeroclaw-provider-diag-rows").to_path_buf();
        let mut state = TuiState::new(workspace, true);
        state.step = Step::ProviderDiagnostics;

        let labels: Vec<&str> = state.visible_fields().iter().map(|row| row.label).collect();
        assert!(labels.contains(&"Provider check details"));
        assert!(labels.contains(&"Provider remediation"));
    }

    #[test]
    fn channel_diagnostics_page_shows_advanced_rows_for_enabled_channels() {
        let workspace = Path::new("/tmp/zeroclaw-channel-diag-rows").to_path_buf();
        let mut state = TuiState::new(workspace, true);
        state.step = Step::ChannelDiagnostics;
        state.plan.enable_telegram = true;
        state.plan.enable_discord = true;

        let labels: Vec<&str> = state.visible_fields().iter().map(|row| row.label).collect();
        assert!(labels.contains(&"Telegram check details"));
        assert!(labels.contains(&"Telegram remediation"));
        assert!(labels.contains(&"Discord check details"));
        assert!(labels.contains(&"Discord remediation"));
    }

    #[test]
    fn tunnel_diagnostics_page_shows_cloudflare_details_and_remediation_rows() {
        let workspace = Path::new("/tmp/zeroclaw-tunnel-diag-rows").to_path_buf();
        let mut state = TuiState::new(workspace, true);
        state.step = Step::TunnelDiagnostics;
        state.plan.tunnel_idx = 1; // cloudflare

        let labels: Vec<&str> = state.visible_fields().iter().map(|row| row.label).collect();
        assert!(labels.contains(&"Cloudflare check details"));
        assert!(labels.contains(&"Cloudflare remediation"));
    }

    #[test]
    fn key_aliases_allow_step_navigation_and_field_navigation() {
        let workspace = Path::new("/tmp/zeroclaw-key-alias-nav").to_path_buf();
        let mut state = TuiState::new(workspace, true);

        state
            .handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE))
            .expect("n should move to next step");
        assert_eq!(state.step, Step::Workspace);

        state
            .handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE))
            .expect("p should move to previous step");
        assert_eq!(state.step, Step::Welcome);

        state.step = Step::Runtime;
        state.focus = 0;

        state
            .handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .expect("j should move focus down");
        assert_eq!(state.focus, 1);

        state
            .handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE))
            .expect("k should move focus up");
        assert_eq!(state.focus, 0);

        state.focus = 1; // DisableTotp toggle
        state
            .handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE))
            .expect("l should toggle on");
        assert!(state.plan.disable_totp);

        state
            .handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))
            .expect("h should toggle off");
        assert!(!state.plan.disable_totp);
    }

    #[test]
    fn edit_shortcuts_support_e_to_open_and_ctrl_s_to_save() {
        let workspace = Path::new("/tmp/zeroclaw-key-alias-edit").to_path_buf();
        let mut state = TuiState::new(workspace, true);
        state.step = Step::Provider;
        state.focus = 2; // Model
        let original = state.plan.model.clone();

        state
            .handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE))
            .expect("e should open editor for text fields");
        assert!(state.editing.is_some());

        state
            .handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
            .expect("typing while editing should append to value");
        state
            .handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
            .expect("ctrl+s should save editing");

        assert!(state.editing.is_none());
        assert_eq!(state.plan.model, format!("{original}x"));
    }

    #[test]
    fn review_step_accepts_s_as_apply_shortcut() {
        let workspace = Path::new("/tmp/zeroclaw-key-alias-review-apply").to_path_buf();
        let mut state = TuiState::new(workspace, true);
        state.step = Step::Review;

        let action = state
            .handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))
            .expect("s should be accepted on review step");

        assert_eq!(action, LoopAction::Submit);
    }
}
