const FILESYSTEM_ALIASES: &[&str] = &["fs", "filesystem"];
const WEB_ALIASES: &[&str] = &["web", "network"];
const SHELL_ALIASES: &[&str] = &["shell", "terminal"];
const SOP_ALIASES: &[&str] = &["sop", "sop-control", "sop_control"];

const FILESYSTEM_TOOLS: &[&str] = &["file_read", "file_write", "file_edit"];
const WEB_TOOLS: &[&str] = &["http_request", "web_search_tool"];
const SHELL_TOOLS: &[&str] = &["shell"];
const SOP_TOOLS: &[&str] = &["sop_execute", "sop_advance", "sop_approve", "sop_status"];

/// Every scope group as `(aliases, member tools)`. A member is compared with
/// registered tool names, so it must equal the `name()` of the tool it means.
const GROUPS: &[(&[&str], &[&str])] = &[
    (FILESYSTEM_ALIASES, FILESYSTEM_TOOLS),
    (WEB_ALIASES, WEB_TOOLS),
    (SHELL_ALIASES, SHELL_TOOLS),
    (SOP_ALIASES, SOP_TOOLS),
];

pub(crate) fn expand_group(name: &str) -> Option<&'static [&'static str]> {
    let normalized = name.trim().to_ascii_lowercase();
    GROUPS
        .iter()
        .find(|(aliases, _)| aliases.contains(&normalized.as_str()))
        .map(|(_, tools)| *tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use tempfile::TempDir;
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_config::schema::{
        BrowserConfig, Config, HttpRequestConfig, MemoryConfig, RiskProfileConfig, SopConfig,
        WebFetchConfig,
    };
    use zeroclaw_memory::Memory;

    use crate::platform::NativeRuntime;
    use crate::sop::engine::SopEngine;
    use crate::sop::scope::{StepToolScope, resolve_excluded};

    /// The names step scope is resolved against in production: the registry
    /// factory's tools, read through the turn loop's own name collector. The
    /// config gates and the SOP engine that group members depend on are on.
    /// Both config arguments point at the temp dir, so the stores the registry
    /// opens (session backend, plugin state) stay out of the real home directory.
    fn registered_tool_names() -> Vec<String> {
        let tmp = TempDir::new().expect("temp dir is created");
        let security = Arc::new(SecurityPolicy::default());
        let memory_config = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let memory: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_config, tmp.path(), None)
                .expect("markdown memory backend is created"),
        );
        let browser = BrowserConfig {
            enabled: false,
            ..BrowserConfig::default()
        };
        let http = HttpRequestConfig {
            enabled: true,
            ..HttpRequestConfig::default()
        };
        let mut root_config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        root_config.web_search.enabled = true;
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig::default())));

        let tools = crate::tools::all_tools_with_runtime(
            Arc::new(root_config.clone()),
            &security,
            &RiskProfileConfig::default(),
            "test-agent",
            Arc::new(NativeRuntime::new()),
            memory,
            None,
            None,
            &browser,
            &http,
            &WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &root_config,
            None,
            false,
            None,
            Some(engine),
            None,
            None,
        )
        .expect("tool registry builds")
        .tools;

        crate::agent::turn::collect_callable_tool_names(&tools, None)
    }

    /// A member that only exists in builds with the external tools cannot be
    /// registered while that feature is off. The `tools-external` test build
    /// still checks it; a default build only has to leave it out.
    fn compiled_out(tool: &str) -> bool {
        !cfg!(feature = "tools-external")
            && zeroclaw_config::builtin_tools::EXTERNAL_TOOL_NAMES.contains(&tool)
    }

    /// The members of one group that this build can register.
    fn registrable_members(tools: &[&'static str]) -> Vec<&'static str> {
        tools
            .iter()
            .copied()
            .filter(|tool| !compiled_out(tool))
            .collect()
    }

    #[test]
    fn every_group_member_is_a_registered_tool_name() {
        let registered = registered_tool_names();

        for (aliases, tools) in GROUPS {
            assert!(!tools.is_empty(), "scope group {aliases:?} has no members");
            for tool in *tools {
                if compiled_out(tool) {
                    continue;
                }
                assert!(
                    registered.iter().any(|name| name == tool),
                    "scope group {aliases:?} lists `{tool}`, which the test registry did not register: fix the name, or switch on its config gate in registered_tool_names(); a member behind the tools-external feature must be listed in EXTERNAL_TOOL_NAMES"
                );
            }
        }
    }

    #[test]
    fn denying_a_group_excludes_exactly_its_registered_tools() {
        let registered = registered_tool_names();

        for (aliases, tools) in GROUPS {
            assert!(!tools.is_empty(), "scope group {aliases:?} has no members");
            let mut expected: Vec<String> = registrable_members(tools)
                .into_iter()
                .map(str::to_string)
                .collect();
            expected.sort();

            for alias in *aliases {
                let scope = StepToolScope {
                    allow: None,
                    deny: vec![(*alias).to_string()],
                };

                assert_eq!(
                    resolve_excluded(&registered, &scope, None, &[]),
                    expected,
                    "deny scope `{alias}`"
                );
            }
        }
    }

    #[test]
    fn allowing_a_group_keeps_exactly_its_registered_tools() {
        let registered = registered_tool_names();

        for (aliases, tools) in GROUPS {
            assert!(!tools.is_empty(), "scope group {aliases:?} has no members");
            let mut expected = registrable_members(tools);
            expected.sort_unstable();

            for alias in *aliases {
                let scope = StepToolScope {
                    allow: Some(vec![(*alias).to_string()]),
                    deny: Vec::new(),
                };
                let excluded = resolve_excluded(&registered, &scope, None, &[]);
                let kept: Vec<&str> = registered
                    .iter()
                    .map(String::as_str)
                    .filter(|name| !excluded.iter().any(|gone| gone == name))
                    .collect();

                assert_eq!(kept, expected, "allow scope `{alias}`");
            }
        }
    }
}
