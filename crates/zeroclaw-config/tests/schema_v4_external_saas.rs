use std::collections::BTreeSet;

use zeroclaw_config::schema::Config;

#[test]
fn linkedin_schema_v4_removal_scope_matches_schema_leaf_paths() {
    let actual: BTreeSet<String> = Config::default()
        .prop_fields()
        .into_iter()
        .filter_map(|field| {
            field
                .name
                .strip_prefix("linkedin.")
                .map(|suffix| format!("linkedin.{suffix}"))
        })
        .collect();

    let expected = BTreeSet::from([
        "linkedin.api_version".to_string(),
        "linkedin.content.github_repos".to_string(),
        "linkedin.content.github_users".to_string(),
        "linkedin.content.instructions".to_string(),
        "linkedin.content.persona".to_string(),
        "linkedin.content.rss_feeds".to_string(),
        "linkedin.content.topics".to_string(),
        "linkedin.enabled".to_string(),
        "linkedin.image.card_accent_color".to_string(),
        "linkedin.image.dalle.api_key_env".to_string(),
        "linkedin.image.dalle.model".to_string(),
        "linkedin.image.dalle.size".to_string(),
        "linkedin.image.enabled".to_string(),
        "linkedin.image.fallback_card".to_string(),
        "linkedin.image.flux.api_key_env".to_string(),
        "linkedin.image.flux.model".to_string(),
        "linkedin.image.imagen.api_key_env".to_string(),
        "linkedin.image.imagen.project_id_env".to_string(),
        "linkedin.image.imagen.region".to_string(),
        "linkedin.image.providers".to_string(),
        "linkedin.image.stability.api_key_env".to_string(),
        "linkedin.image.stability.model".to_string(),
        "linkedin.image.temp_dir".to_string(),
    ]);

    assert_eq!(
        actual, expected,
        "Schema V4's LinkedIn removal scope must track every schema-derived linkedin.* leaf path"
    );
}
