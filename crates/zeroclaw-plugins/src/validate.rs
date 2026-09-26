//! Load verification: does this component instantiate against this host?
//!
//! A plugin built against a drifted or wrong WIT ABI (for example an old
//! `configure(config: string)` where the host expects a no-arg `configure()`,
//! or a `channel-capabilities` flag set the host does not share) will
//! *instantiate* incompatibly. Today that only surfaces at daemon startup, as a
//! `WARN` log, after which the plugin is silently skipped — so `plugin install`
//! reports success for a plugin that will never load.
//!
//! [`verify_component_loads`] closes that gap by running the same type-check
//! and resolved limits the daemon uses at startup, so the failure reaches the
//! operator at the CLI instead of vanishing into a log.
//!
//! It is the single implementation of that check. `plugin install` runs it as a
//! gate; `plugin info` and `plugin list --verify` run it as a report, for
//! plugins that were installed before the gate existed, installed through
//! `--no-verify`, or outlived a host upgrade.

use crate::PluginManifest;
use crate::host::AdmittedComponent;
use anyhow::Result;

/// Verify the admitted `component` instantiates against this host's WIT
/// world for every capability the manifest declares. These are the exact bytes
/// admission read, so the check covers what will be installed. Returns the full
/// instantiation diagnostic (wasmtime cause-chain plus the WIT-drift rebuild
/// hint) on failure.
///
/// When the host is built without a WASM execution backend there is nothing to
/// instantiate against, so this is a no-op that returns `Ok(())`.
#[cfg(feature = "plugins-wasmtime")]
pub async fn verify_component_loads(
    component: &AdmittedComponent,
    manifest: &PluginManifest,
    limits: crate::component::PluginLimits,
) -> Result<()> {
    use crate::PluginCapability;
    use crate::instance::PluginInstanceScope;

    // Compile once up front. This alone catches a wrong build target (a core
    // module or a `wasm32-wasip1` build), a truncated or corrupt artifact, and
    // a non-component wasm — uniformly, including for capabilities that have no
    // dedicated instantiate world below.
    let _component = crate::component::load_component(component)?;

    let services = validation_services();
    for capability in &manifest.capabilities {
        match capability {
            PluginCapability::Tool => {
                let scope = PluginInstanceScope::for_package_binding(
                    manifest,
                    PluginCapability::Tool,
                    manifest.permissions.iter().copied(),
                )?;
                // `create_plugin` compiles and instantiates, then returns —
                // it never calls a guest export, so it is exactly the load-check.
                crate::runtime::create_plugin(component, &scope, &services, limits)
                    .await
                    .map(|_plugin| ())?;
            }
            PluginCapability::Channel => {
                let scope = PluginInstanceScope::for_package_binding(
                    manifest,
                    PluginCapability::Channel,
                    manifest.permissions.iter().copied(),
                )?;
                crate::wasm_channel::verify_channel_loads(component, &scope, &services, limits)
                    .await?;
            }
            // Memory and other capabilities are covered by the compile check
            // above; a dedicated instantiate world for them is a follow-up.
            _ => {}
        }
    }

    Ok(())
}

/// No-backend build: nothing to instantiate against, so verification passes.
///
/// There are no limits to take here: `PluginLimits` lives in the backend-only
/// `component` module, and every caller that has a materialized limit set is
/// itself compiled with a backend, so this shape is reached only by builds that
/// cannot hand one over.
#[cfg(not(feature = "plugins-wasmtime"))]
pub async fn verify_component_loads(
    _component: &AdmittedComponent,
    _manifest: &PluginManifest,
) -> Result<()> {
    Ok(())
}

/// A config resolver that is never consulted: verification stops at
/// instantiation, before any guest `config.get`. If a future change makes
/// instantiation itself read config, this surfaces a clear message instead of a
/// panic rather than silently validating against empty config.
#[cfg(feature = "plugins-wasmtime")]
fn validation_services() -> crate::services::PluginHostServices {
    use crate::config::PluginConfigResolver;
    use crate::error::PluginError;

    crate::services::PluginHostServices::new(
        PluginConfigResolver::new(|_scope| {
            Err(PluginError::InvalidConfig(
                "config resolution is unavailable during install-time load verification".into(),
            ))
        }),
        // Durable state is equally unavailable: verification never runs a
        // service frame, so nothing may read or write an instance's state.
        crate::services::PluginStateService::unavailable(),
    )
}
