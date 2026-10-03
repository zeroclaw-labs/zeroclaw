//! The effect of a config write on each path it persists, shared by the
//! gateway's principal gate and the RPC persistence boundary so both
//! transports authorize the same `(path, verb)` set for the same mutation.

use std::collections::HashSet;

use zeroclaw_api::grants::Verb;
use zeroclaw_config::schema::Config;

/// Classify `paths` by what the write does to each: a path declared only in
/// `after` is a `Create`, one declared only in `before` a `Delete`, anything
/// else an `Update`. Declaration follows the property tree, so a map entry
/// (`agents.<alias>`) exists exactly while it has fields, and creating one
/// implicitly (a write under a new alias) classifies as the creation it is.
pub fn classify_by_effect<'a>(
    before: &Config,
    after: &Config,
    paths: impl IntoIterator<Item = &'a str>,
) -> Vec<(String, Verb)> {
    let before = declared_paths(before);
    let after = declared_paths(after);
    paths
        .into_iter()
        .map(|path| {
            let verb = match (contains_path(&before, path), contains_path(&after, path)) {
                (false, true) => Verb::Create,
                (true, false) => Verb::Delete,
                _ => Verb::Update,
            };
            (path.to_owned(), verb)
        })
        .collect()
}

/// Pin the verb for a path whose route semantics the effect diff does not
/// show, replacing any classification it already has: clearing a scalar
/// prop leaves the field declared but is a removal.
pub fn pin(writes: &mut Vec<(String, Verb)>, path: impl Into<String>, verb: Verb) {
    let path = path.into();
    writes.retain(|(existing, _)| *existing != path);
    writes.push((path, verb));
}

fn declared_paths(config: &Config) -> HashSet<String> {
    config
        .prop_fields()
        .into_iter()
        .map(|info| info.name)
        .collect()
}

fn contains_path(declared: &HashSet<String>, path: &str) -> bool {
    declared.contains(path)
        || declared.iter().any(|name| {
            name.strip_prefix(path)
                .is_some_and(|rest| rest.starts_with('.'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_create_update_and_delete_by_effect() {
        let mut before = Config::default();
        before
            .create_map_key("agents", "gone")
            .expect("create agents.gone");
        let mut after = before.clone();
        after
            .create_map_key("agents", "fresh")
            .expect("create agents.fresh");
        after.agents.remove("gone");
        after.memory.backend = "none".into();

        let writes = classify_by_effect(
            &before,
            &after,
            ["agents.fresh", "agents.gone", "memory.backend"],
        );

        assert!(writes.contains(&("agents.fresh".into(), Verb::Create)));
        assert!(writes.contains(&("agents.gone".into(), Verb::Delete)));
        assert!(writes.contains(&("memory.backend".into(), Verb::Update)));
    }

    #[test]
    fn a_pin_replaces_the_classified_verb() {
        let mut writes = vec![("memory.backend".to_string(), Verb::Update)];
        pin(&mut writes, "memory.backend", Verb::Delete);
        assert_eq!(writes, vec![("memory.backend".to_string(), Verb::Delete)]);
    }
}
