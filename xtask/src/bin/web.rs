//! `cargo xtask web` — drive the web dashboard build from cargo.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::Command;
use xtask::util::{repo_root, require_tool, run_cmd};

#[derive(Parser, Debug)]
#[command(name = "web", about = "Build the ZeroClaw web dashboard")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Render the gateway's OpenAPI spec and regenerate the TS client.
    GenApi,
    /// Run `npm install` in web/.
    Install,
    /// Regenerate the TS client and run `npm run build`.
    Build,
    /// Regenerate the TS client and start `npm run dev`.
    Dev,
    /// Regenerate the TS client and typecheck (`tsc -b`) without
    /// producing a bundle.
    Check,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = repo_root();
    let web_dir = root.join("web");
    let spec_path = root.join("target/openapi.json");
    match cli.cmd {
        Cmd::GenApi => gen_api(&web_dir, &spec_path),
        Cmd::Install => npm_install(&web_dir),
        Cmd::Build => {
            gen_api(&web_dir, &spec_path)?;
            npm_run(&web_dir, "build")
        }
        Cmd::Dev => {
            gen_api(&web_dir, &spec_path)?;
            npm_run(&web_dir, "dev")
        }
        Cmd::Check => {
            gen_api(&web_dir, &spec_path)?;
            npx(&web_dir, &["tsc", "-b"])
        }
    }
}

fn npm_install(web_dir: &Path) -> Result<()> {
    require_tool("npm", "https://nodejs.org/ or `nvm install --lts`")?;
    println!("==> npm install ({})", web_dir.display());
    run_cmd(Command::new(bin("npm")).current_dir(web_dir).arg("install"))
}

fn node_modules_needs_install(web_dir: &Path) -> bool {
    let node_modules = web_dir.join("node_modules");
    if !node_modules.exists() {
        return true;
    }
    let sentinel = node_modules.join(".package-lock.json");
    let lock = web_dir.join("package-lock.json");
    let (Ok(sentinel_meta), Ok(lock_meta)) = (sentinel.metadata(), lock.metadata()) else {
        // Sentinel missing → install never completed cleanly. Lock missing → treat
        // node_modules as authoritative (no signal to re-install).
        return !sentinel.exists() && lock.exists();
    };
    match (sentinel_meta.modified(), lock_meta.modified()) {
        (Ok(sentinel_t), Ok(lock_t)) => lock_t > sentinel_t,
        _ => false,
    }
}

fn npm_run(web_dir: &Path, script: &str) -> Result<()> {
    println!("==> npm run {script}");
    run_cmd(
        Command::new(bin("npm"))
            .current_dir(web_dir)
            .args(["run", script]),
    )
}

fn npx(web_dir: &Path, args: &[&str]) -> Result<()> {
    println!("==> npx {}", args.join(" "));
    let mut cmd = Command::new(bin("npx"));
    cmd.current_dir(web_dir).arg("--no-install").args(args);
    run_cmd(&mut cmd)
}

fn gen_api(web_dir: &Path, spec_path: &Path) -> Result<()> {
    require_tool("npm", "https://nodejs.org/ or `nvm install --lts`")?;
    if node_modules_needs_install(web_dir) {
        npm_install(web_dir)?;
    }
    let out_rel = PathBuf::from("src/lib/api-generated.ts");
    let out_abs = web_dir.join(&out_rel);
    if let Some(parent) = out_abs.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent directory {}", parent.display()))?;
    }
    if let Some(parent) = spec_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent directory {}", parent.display()))?;
    }

    let spec_value = zeroclaw_gateway::openapi::build_spec();
    let spec = serde_json::to_string(&spec_value).context("serialize openapi spec to JSON")?;
    std::fs::write(spec_path, &spec)
        .with_context(|| format!("write openapi spec to {}", spec_path.display()))?;
    println!("==> gen-api → {}", out_abs.display());

    let desc_rel = PathBuf::from("src/lib/api-descriptions.ts");
    let desc_abs = web_dir.join(&desc_rel);
    let desc_ts = render_descriptions(&spec_value);
    std::fs::write(&desc_abs, &desc_ts)
        .with_context(|| format!("write field descriptions to {}", desc_abs.display()))?;
    println!("==> gen-api → {}", desc_abs.display());

    let enums_rel = PathBuf::from("src/lib/api-enums.ts");
    let enums_abs = web_dir.join(&enums_rel);
    let enums_ts = render_enum_values(&spec_value);
    std::fs::write(&enums_abs, &enums_ts)
        .with_context(|| format!("write enum values to {}", enums_abs.display()))?;
    println!("==> gen-api → {}", enums_abs.display());

    let spec_arg = spec_path
        .to_str()
        .context("openapi spec path is not valid utf-8")?;
    let out_arg = out_rel
        .to_str()
        .context("api-generated.ts path is not valid utf-8")?;
    run_cmd(Command::new(bin("npx")).current_dir(web_dir).args([
        "--no-install",
        "openapi-typescript",
        spec_arg,
        "-o",
        out_arg,
    ]))
    .context("`npx openapi-typescript` failed (run `cargo web install` first?)")
}

fn bin(tool: &str) -> String {
    if cfg!(windows) {
        format!("{tool}.cmd")
    } else {
        tool.to_string()
    }
}

/// Collect the string variants of a schema whether it uses top-level `enum`,
/// `oneOf`/`anyOf` `const` strings, or `oneOf`/`anyOf` `enum` string arrays.
/// Object variants (e.g. `StepFailure::Goto`) contribute nothing. Preserves
/// spec order and drops duplicates so pickers render one row per variant.
fn collect_string_enum_members(schema: &serde_json::Value) -> Vec<String> {
    let mut members: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        let owned = value.to_string();
        if !members.contains(&owned) {
            members.push(owned);
        }
    };

    if schema.get("type").and_then(|t| t.as_str()) == Some("string")
        && let Some(variants) = schema.get("enum").and_then(|e| e.as_array())
    {
        for value in variants.iter().filter_map(|v| v.as_str()) {
            push(value);
        }
    }

    for key in ["oneOf", "anyOf"] {
        let Some(variants) = schema.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for variant in variants {
            if variant.get("type").and_then(|t| t.as_str()) != Some("string") {
                continue;
            }
            if let Some(value) = variant.get("const").and_then(|c| c.as_str()) {
                push(value);
            }
            if let Some(inner) = variant.get("enum").and_then(|e| e.as_array()) {
                for value in inner.iter().filter_map(|v| v.as_str()) {
                    push(value);
                }
            }
        }
    }

    members
}

/// Extract `{ SchemaName: [variant, ...] }` for every schema whose top-level
/// shape is a string `enum`, so option pickers walk the spec instead of
/// retyping variant lists. The Rust enums remain the single source of truth.
fn render_enum_values(spec: &serde_json::Value) -> String {
    use std::collections::BTreeMap;

    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();

    let schemas = spec
        .get("components")
        .and_then(|c| c.get("schemas"))
        .and_then(|s| s.as_object());

    if let Some(schemas) = schemas {
        for (name, schema) in schemas {
            let members = collect_string_enum_members(schema);
            if !members.is_empty() {
                out.insert(name.clone(), members);
            }
        }
    }

    let mut body = String::new();
    body.push_str(
        "// GENERATED by `cargo web gen-api` from the gateway OpenAPI spec.\n\
         // Enum variant lists are sourced from Rust enums; do not edit by hand\n\
         // and do not retype variant lists in components.\n\n\
         export type EnumValues = Record<string, readonly string[]>;\n\n\
         export const enumValues: EnumValues = ",
    );
    let json = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    body.push_str(&json);
    body.push_str(
        ";\n\n\
         /** Variant list for a generated string enum, or an empty array. */\n\
         export function enumMembers(schema: string): readonly string[] {\n\
         \x20 return enumValues[schema] ?? [];\n\
         }\n",
    );
    body
}

/// Extract `{ SchemaName: { field: description } }` from the OpenAPI spec so the
/// frontend renders tooltips from the Rust `///` docs at runtime. TypeScript
/// `@description` JSDoc is erased at build time and unreadable at runtime; this
/// projects the same spec into a real data module. The Rust doc comments remain
/// the single source of truth, nothing is retyped on the frontend.
fn render_descriptions(spec: &serde_json::Value) -> String {
    use std::collections::BTreeMap;

    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    let schemas = spec
        .get("components")
        .and_then(|c| c.get("schemas"))
        .and_then(|s| s.as_object());

    if let Some(schemas) = schemas {
        for (name, schema) in schemas {
            let mut fields: BTreeMap<String, String> = BTreeMap::new();
            collect_property_descriptions(schema, &mut fields);
            if !fields.is_empty() {
                out.insert(name.clone(), fields);
            }
        }
    }

    let mut body = String::new();
    body.push_str(
        "// GENERATED by `cargo web gen-api` from the gateway OpenAPI spec.\n\
         // Field help text is sourced from Rust `///` doc comments; do not edit\n\
         // by hand and do not duplicate help text in components.\n\n\
         export type FieldDescriptions = Record<string, Record<string, string>>;\n\n\
         export const fieldDescriptions: FieldDescriptions = ",
    );
    let json = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string());
    body.push_str(&json);
    body.push_str(
        ";\n\n\
         /** Help text for one field of a generated schema, or undefined. */\n\
         export function fieldHelp(schema: string, field: string): string | undefined {\n\
         \x20 return fieldDescriptions[schema]?.[field];\n\
         }\n",
    );
    body
}

/// Merge every `properties.<field>.description` found in a schema (including its
/// `oneOf` / `anyOf` variants, so serde-tagged enums like `SopTrigger` surface
/// each variant's field docs) into `fields`.
fn collect_property_descriptions(
    schema: &serde_json::Value,
    fields: &mut std::collections::BTreeMap<String, String>,
) {
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        for (field, prop) in props {
            if let Some(desc) = prop.get("description").and_then(|d| d.as_str()) {
                fields
                    .entry(field.clone())
                    .or_insert_with(|| desc.to_string());
            }
        }
    }
    for key in ["oneOf", "anyOf", "allOf"] {
        if let Some(variants) = schema.get(key).and_then(|v| v.as_array()) {
            for variant in variants {
                collect_property_descriptions(variant, fields);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::node_modules_needs_install;
    use std::fs;
    use std::thread::sleep;
    use std::time::Duration;
    use tempfile::TempDir;

    fn fresh_web_dir() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        fs::write(dir.path().join("package-lock.json"), "{}").unwrap();
        dir
    }

    #[test]
    fn needs_install_when_node_modules_missing() {
        let dir = fresh_web_dir();
        // No node_modules at all → must trigger install.
        assert!(node_modules_needs_install(dir.path()));
    }

    #[test]
    fn needs_install_when_sentinel_missing() {
        // node_modules exists but `.package-lock.json` sentinel was never written
        // (e.g. partial / failed previous install). Must trigger install.
        let dir = fresh_web_dir();
        fs::create_dir(dir.path().join("node_modules")).unwrap();
        assert!(node_modules_needs_install(dir.path()));
    }

    #[test]
    fn needs_install_when_lock_is_newer_than_sentinel() {
        // The actual staleness fix: a pull or merge updated package-lock.json
        // after the last install. Sentinel is OLDER than lock → reinstall.
        let dir = fresh_web_dir();
        let nm = dir.path().join("node_modules");
        fs::create_dir(&nm).unwrap();
        fs::write(nm.join(".package-lock.json"), "{}").unwrap();
        // Wait a measurable tick, then bump lock mtime.
        sleep(Duration::from_millis(20));
        fs::write(
            dir.path().join("package-lock.json"),
            "{ \"updated\": true }",
        )
        .unwrap();
        assert!(
            node_modules_needs_install(dir.path()),
            "stale node_modules (sentinel older than lock) must reinstall"
        );
    }

    #[test]
    fn skips_when_sentinel_is_at_least_as_new_as_lock() {
        // Clean install just finished. Sentinel mtime ≥ lock mtime → no
        // spurious reinstall on every build invocation.
        let dir = fresh_web_dir();
        let nm = dir.path().join("node_modules");
        fs::create_dir(&nm).unwrap();
        // Write sentinel AFTER lock so its mtime is newer.
        sleep(Duration::from_millis(20));
        fs::write(nm.join(".package-lock.json"), "{}").unwrap();
        assert!(
            !node_modules_needs_install(dir.path()),
            "fresh node_modules must not trigger spurious reinstall"
        );
    }

    #[test]
    fn skips_when_lock_missing() {
        // Defensive case: no package-lock.json (unusual — repo always has one).
        // Without a signal we cannot tell if node_modules is stale; treat as
        // valid to avoid a reinstall loop.
        let dir = TempDir::new().expect("tempdir");
        let nm = dir.path().join("node_modules");
        fs::create_dir(&nm).unwrap();
        fs::write(nm.join(".package-lock.json"), "{}").unwrap();
        assert!(!node_modules_needs_install(dir.path()));
    }
}
