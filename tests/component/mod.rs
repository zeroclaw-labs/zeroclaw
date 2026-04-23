mod config_persistence;
mod config_schema;
mod fdx_parser;
// Dockerfile was deleted in Phase 1.2 (Augusta is native macOS, no Docker)
// mod dockerignore_test;
#[cfg(feature = "gateway")]
mod gateway;
// OpenTelemetry OTLP feature was stripped
// mod otel_dependency_feature_regression;
mod provider_resolution;
mod provider_schema;
mod reply_target_field_regression;
mod security;
#[cfg(feature = "gateway")]
mod whatsapp_webhook_security;
