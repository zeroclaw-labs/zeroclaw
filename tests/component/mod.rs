#[cfg(feature = "channel-acp-server")]
mod acp_session_cwd_stdio;
#[cfg(feature = "agent-runtime")]
mod config_dir_locale_regression;
mod config_patch_cli;
mod config_persistence;
mod config_schema;
#[cfg(feature = "agent-runtime")]
mod cron_delivery_cli;
mod cron_help_examples;
mod daemon_startup_feedback;
#[cfg(all(feature = "agent-runtime", target_os = "linux"))]
mod desktop_cli_linux;
mod direct_cli_terminal_completion;
mod dockerignore_test;
mod gateway;
mod gemini_capabilities;
mod otel_dependency_feature_regression;
mod plugin_feature_graph;
mod provider_resolution;
mod provider_schema;
mod reply_target_field_regression;
mod security;
mod skills_bundle_cli;
mod whatsapp_webhook_security;
