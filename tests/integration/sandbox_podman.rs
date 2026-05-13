//! Integration tests for Podman sandbox configuration.
//!
//! Deeper behavioral tests (wrap_command flags, is_podman_docker detection)
//! live as unit tests inside src/security/podman.rs, since `security` is
//! pub(crate). These tests cover the public config surface: TOML parsing,
//! schema defaults, and round-trips that would catch serde regressions.

use daemonclaw_config::schema::{PodmanSandboxConfig, SandboxBackend, SandboxConfig};

// ─── SandboxBackend::Podman deserialization ─────────────────────────────────

#[test]
fn sandbox_backend_deserializes_podman() {
    let config: SandboxConfig = toml::from_str(
        r#"
        enabled = true
        backend = "podman"
    "#,
    )
    .unwrap();
    assert!(matches!(config.backend, SandboxBackend::Podman));
}

#[test]
fn sandbox_backend_podman_is_distinct_from_docker() {
    let podman: SandboxConfig = toml::from_str("backend = \"podman\"").unwrap();
    let docker: SandboxConfig = toml::from_str("backend = \"docker\"").unwrap();
    assert!(matches!(podman.backend, SandboxBackend::Podman));
    assert!(matches!(docker.backend, SandboxBackend::Docker));
}

// ─── PodmanSandboxConfig defaults ───────────────────────────────────────────

#[test]
fn podman_sandbox_config_defaults() {
    let cfg = PodmanSandboxConfig::default();
    assert_eq!(cfg.image, "ubuntu:24.04");
    assert_eq!(cfg.userns, "keep-id");
    assert_eq!(cfg.network, "none");
    assert_eq!(cfg.memory_limit, "512m");
    assert_eq!(cfg.cpu_limit, "1.0");
    assert!(cfg.selinux_label);
    assert!(cfg.extra_args.is_empty());
}

#[test]
fn sandbox_config_default_has_podman_sub_config() {
    let cfg = SandboxConfig::default();
    assert_eq!(cfg.podman.userns, "keep-id");
}

// ─── TOML round-trip ────────────────────────────────────────────────────────

#[test]
fn podman_sandbox_config_roundtrips_toml() {
    let toml = r#"
        enabled = true
        backend = "podman"

        [podman]
        image = "ubuntu:24.04"
        userns = "auto"
        network = "slirp4netns"
        memory_limit = "256m"
        cpu_limit = "0.5"
        selinux_label = false
        extra_args = ["--read-only", "--cap-drop=ALL"]
    "#;

    let cfg: SandboxConfig = toml::from_str(toml).unwrap();
    assert!(matches!(cfg.backend, SandboxBackend::Podman));
    assert_eq!(cfg.podman.image, "ubuntu:24.04");
    assert_eq!(cfg.podman.userns, "auto");
    assert_eq!(cfg.podman.network, "slirp4netns");
    assert_eq!(cfg.podman.memory_limit, "256m");
    assert_eq!(cfg.podman.cpu_limit, "0.5");
    assert!(!cfg.podman.selinux_label);
    assert_eq!(cfg.podman.extra_args, vec!["--read-only", "--cap-drop=ALL"]);
}

#[test]
fn podman_sandbox_config_missing_sub_table_uses_defaults() {
    let cfg: SandboxConfig = toml::from_str("backend = \"podman\"").unwrap();
    assert_eq!(cfg.podman.image, "ubuntu:24.04");
    assert_eq!(cfg.podman.userns, "keep-id");
    assert!(cfg.podman.selinux_label);
}

#[test]
fn sandbox_config_serializes_and_deserializes_with_podman() {
    let original = SandboxConfig {
        backend: SandboxBackend::Podman,
        podman: PodmanSandboxConfig {
            image: "debian:bookworm-slim".to_string(),
            extra_args: vec!["--cap-drop=ALL".to_string()],
            ..PodmanSandboxConfig::default()
        },
        ..SandboxConfig::default()
    };

    let serialized = toml::to_string(&original).expect("serialization failed");
    let deserialized: SandboxConfig =
        toml::from_str(&serialized).expect("deserialization failed");

    assert!(matches!(deserialized.backend, SandboxBackend::Podman));
    assert_eq!(deserialized.podman.image, "debian:bookworm-slim");
    assert_eq!(deserialized.podman.extra_args, vec!["--cap-drop=ALL"]);
}
