use zeroclaw_api::attribution::{Attributable, Role};

pub struct FixtureAttributable;

impl Attributable for FixtureAttributable {
    fn role(&self) -> Role {
        Role::Skill
    }

    fn alias(&self) -> &str {
        "fixture"
    }
}
