//! Provider/model construction from the canonical configuration selection.
use crate::ModelProvider;
use std::sync::Arc;
use zeroclaw_config::schema::Config;

/// A provider and its configuration-derived selection, built together.
/// Billing uses `provider_name`; selection-sensitive consumers use `identity`.
pub struct BuiltModel {
    pub provider: Box<dyn ModelProvider>,
    pub provider_name: String,
    pub model_name: String,
    pub identity: zeroclaw_config::schema::ModelIdentity,
    pub model_route_resolver: Arc<crate::router::ModelRouteResolver>,
}

/// The single model constructor: initial construction, runtime switches, live
/// refresh, and session overrides all rebuild through here.
///
/// Callers supply only the ref (two- or three-segment, or a bare family name
/// for the legacy CLI `--provider <family>` form), an optional explicit
/// model; everything else (agent config, entry
/// overlay, temperature, context window, identity) is derived
/// from the full config. The raw ref is passed through to the providers
/// construction chain: that chain re-resolves the model entry from the ref it
/// is given, and feeding it a normalised two-segment ref would select
/// `models.default` and clobber the selected entry's tuning overlay.
pub fn build_model(
    config: &Config,
    agent_alias: &str,
    model_provider_ref: &str,
    model_override: Option<&str>,
) -> anyhow::Result<BuiltModel> {
    crate::ensure_known_model_entry_ref(config, model_provider_ref)?;
    // A two- or three-segment ref names a profile; a third segment selects a
    // model entry under the profile's `models` map. A bare family inherits
    // the agent selection's model and connection, with runtime options
    // inherited from the agent's own profile with the provider-specific bits
    // cleared (exactly what `options_for_provider_ref` gives a bare name).
    // The entry itself may be absent (e.g. auth via env) — resolution stays
    // lenient so an override can still build a provider.
    let (model_provider_name, model_provider_alias, bare_family) =
        match config.providers.models.profile_ref(model_provider_ref) {
            Some((family, alias)) => (family.to_string(), alias.to_string(), false),
            None => (model_provider_ref.to_string(), String::new(), true),
        };

    let agent_selection = config
        .agent(agent_alias)
        .and_then(|agent| config.resolve_model_selection(&agent.model_provider));
    let entry = if bare_family {
        agent_selection.as_ref().map(|selection| selection.entry)
    } else {
        config
            .providers
            .models
            .find(&model_provider_name, &model_provider_alias)
    };
    // Named refs select their own profile; a bare family inherits the
    // configured agent selection, including default/sole nested entries.
    let selection = if bare_family {
        agent_selection
    } else {
        config.resolve_model_selection(model_provider_ref)
    };
    let model_entry = selection.as_ref().and_then(|s| s.model_entry);

    let model_name = model_override
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .or_else(|| selection.as_ref().and_then(|s| s.model_id.clone()))
        .or_else(|| {
            entry
                .and_then(|e| e.model.as_deref())
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(str::to_string)
        })
        .ok_or_else(|| {
            anyhow::Error::msg(format!(
                "model_provider `{model_provider_ref}` has no `model` configured and no model \
                 override was supplied"
            ))
        })?;

    let identity = selection
        .as_ref()
        .filter(|_| !bare_family)
        .map(|s| s.identity(model_override))
        .unwrap_or_else(|| zeroclaw_config::schema::ModelIdentity {
            family: model_provider_name.clone(),
            alias: model_provider_alias.clone(),
            entry_alias: None,
            model_id: model_name.clone(),
        });

    let mut runtime_options = if bare_family {
        // Bare family: inherit the agent's own profile options with the
        // provider-specific bits cleared — the legacy `--provider <family>`
        // semantics preserved by `options_for_provider_ref`'s bare arm.
        let agent_options = crate::provider_runtime_options_for_agent(config, agent_alias);
        crate::options_for_provider_ref(config, model_provider_ref, &agent_options)
    } else {
        crate::provider_runtime_options_for_alias(
            config,
            &model_provider_name,
            &model_provider_alias,
        )
    };
    crate::apply_model_entry_options(&mut runtime_options, model_entry);

    let (provider, model_route_resolver) =
        crate::create_routed_model_provider_with_options_and_resolver(
            config,
            // The raw ref, deliberately: see the function-level comment.
            model_provider_ref,
            entry.and_then(|e| e.api_key.as_deref()),
            entry.and_then(|e| e.uri.as_deref()),
            &config.reliability,
            &config.model_routes,
            &model_name,
            &runtime_options,
        )?;

    Ok(BuiltModel {
        provider,
        // A bare family name stays as-is (billing falls back through
        // `provider_pricing`'s unique-alias arm); a dotted ref commits the
        // normalised two-segment profile name.
        provider_name: if bare_family {
            model_provider_ref.to_string()
        } else {
            format!("{model_provider_name}.{model_provider_alias}")
        },
        model_name,
        identity,
        model_route_resolver,
    })
}
