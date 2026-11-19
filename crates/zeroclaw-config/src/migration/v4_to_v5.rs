//! The V4 → V5 boundary. V4 was released with ZeroClaw 0.9.0.
//!
//! This first step only introduces the version. Provider registry changes
//! extend this module in the next stack layer, leaving V3 → V4 unchanged.

use anyhow::Result;

pub(super) fn migrate(value: toml::Value) -> Result<toml::Value> {
    super::stamp_schema_version(value, 5)
}

#[cfg(test)]
mod tests {
    use super::super::{GenerateOptions, generate, migrate_file};

    #[test]
    fn released_v4_generation_stays_frozen() {
        assert_eq!(
            generate(4, &GenerateOptions::default()).unwrap(),
            include_str!("../../fixtures/v4.toml"),
        );
    }

    #[test]
    fn v4_to_v5_preserves_values_comments_and_secret_ciphertext() {
        let input = "schema_version = 4 # released version\n\n[providers.models.openai.main]\napi_key = 'enc2:TEST_CIPHERTEXT'\nmodel = 'gpt-4o'\n\n[tools]\noptional = ['*']\n";
        let output = migrate_file(input).unwrap().unwrap();
        assert_eq!(
            output,
            input.replacen("schema_version = 4", "schema_version = 5", 1)
        );
        assert!(migrate_file(&output).unwrap().is_none());
    }

    #[test]
    fn older_versions_reach_v5_through_the_chain() {
        for version in 1..=4 {
            let source = generate(version, &GenerateOptions::default()).unwrap();
            let output = migrate_file(&source).unwrap().unwrap();
            let value: toml::Value = toml::from_str(&output).unwrap();
            assert_eq!(value["schema_version"].as_integer(), Some(5));
            assert!(migrate_file(&output).unwrap().is_none());
        }
    }
}
