//! Single source of truth for the section under test across the onboarding
//! integration binaries: its walkable spec and the completed outcome shape.

use zeroclaw_config::schema::{Config, MatrixConfig};
use zeroclaw_onboarding::build_spec;
use zeroclaw_runtime::flow::{ConfiguredItem, Outcome, Spec};

pub const SECTION: &str = "channels.matrix.home";
pub const LAYER: &str = "channel";
pub const INSTANCE: &str = "home";

pub fn matrix_config() -> Config {
    let mut config = Config::default();
    config
        .channels
        .matrix
        .insert(INSTANCE.to_string(), MatrixConfig::default());
    config
}

pub fn completed_outcome() -> Outcome {
    Outcome::Completed {
        configured: vec![ConfiguredItem {
            layer: LAYER.to_string(),
            instance: INSTANCE.to_string(),
        }],
    }
}

pub fn matrix_spec() -> Spec {
    build_spec(
        matrix_config().prop_fields(),
        SECTION,
        LAYER,
        INSTANCE,
        completed_outcome(),
    )
    .expect("matrix section yields a spec")
}
