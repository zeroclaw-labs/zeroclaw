//! Architecture gates for the crates.io publish contract.
//!
//! Publishing is irreversible — a version can be yanked but never replaced — and
//! every invariant here corresponds to a failure that is invisible during normal
//! development and only surfaces mid-publish, after earlier crates in the
//! dependency order have already uploaded.
//!
//! Each gate below encodes a defect that actually occurred:
//!
//! * a `publish = false` crate inside the root's dependency closure, which
//!   blocked every release after the microkernel split
//! * a git dependency with no `version`, which crates.io rejects outright
//! * a compile-time file reference reaching outside its own crate directory,
//!   which packages fine and then fails to compile from the tarball. Feature
//!   gated ones are the dangerous case: `cargo publish --dry-run` verifies
//!   default features only, so they pass preflight and ship broken

use proc_macro2::{TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use syn::visit::{self, Visit};

/// The crate the world installs. Renamed from `zeroclawlabs` so that
/// `cargo install zeroclaw` matches the binary name.
const ROOT_PACKAGE: &str = "zeroclaw";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

struct Crate {
    name: String,
    version: String,
    dir: PathBuf,
    manifest: toml::Table,
    publishable: bool,
}

/// `publish` is absent when unrestricted, `false` when private. crates.io also
/// accepts a registry allow-list, which this workspace does not use.
fn is_publishable(manifest: &toml::Table) -> bool {
    manifest
        .get("package")
        .and_then(|p| p.get("publish"))
        .is_none_or(|v| v.as_bool() != Some(false))
}

fn workspace_crates() -> Vec<Crate> {
    let root = repo_root();
    let root_manifest: toml::Table = fs::read_to_string(root.join("Cargo.toml"))
        .expect("read workspace Cargo.toml")
        .parse()
        .expect("workspace Cargo.toml is valid TOML");

    let members = root_manifest["workspace"]["members"]
        .as_array()
        .expect("[workspace] members must be an array");
    let workspace_version = root_manifest["workspace"]["package"]["version"]
        .as_str()
        .expect("[workspace.package] version must be a string");

    members
        .iter()
        .map(|m| {
            let rel = m.as_str().expect("member entries are strings");
            let dir = if rel == "." {
                root.clone()
            } else {
                root.join(rel)
            };
            let manifest: toml::Table = fs::read_to_string(dir.join("Cargo.toml"))
                .unwrap_or_else(|e| panic!("read {rel}/Cargo.toml: {e}"))
                .parse()
                .unwrap_or_else(|e| panic!("{rel}/Cargo.toml is valid TOML: {e}"));
            let name = manifest["package"]["name"]
                .as_str()
                .expect("package name is a string")
                .to_owned();
            let version = manifest["package"]["version"]
                .as_str()
                .unwrap_or(workspace_version)
                .to_owned();
            let publishable = is_publishable(&manifest);
            Crate {
                name,
                version,
                dir,
                manifest,
                publishable,
            }
        })
        .collect()
}

fn workspace_version() -> String {
    let manifest: toml::Table = fs::read_to_string(repo_root().join("Cargo.toml"))
        .expect("read workspace Cargo.toml")
        .parse()
        .expect("workspace Cargo.toml is valid TOML");
    manifest["workspace"]["package"]["version"]
        .as_str()
        .expect("[workspace.package] version must be a string")
        .to_owned()
}

/// Crates in the coordinated ZeroClaw release.
///
/// Independent-version workspace members (for example a hardware binding that
/// is awaiting removal) are not silently pulled into a v0.8.x release merely
/// because they share the repository.
fn release_crates(crates: &[Crate]) -> BTreeSet<&str> {
    let version = workspace_version();
    crates
        .iter()
        .filter(|krate| krate.publishable && krate.version == version)
        .map(|krate| krate.name.as_str())
        .collect()
}

/// Internal dependency edges that affect publish order. Dev-dependencies are
/// excluded: cargo strips them when packaging, so a dev-only cycle is legal.
fn internal_deps(krate: &Crate, workspace_names: &BTreeSet<String>) -> BTreeSet<String> {
    ["dependencies", "build-dependencies"]
        .iter()
        .filter_map(|section| krate.manifest.get(*section))
        .filter_map(|v| v.as_table())
        .flat_map(|t| t.keys())
        .filter(|k| workspace_names.contains(*k))
        .cloned()
        .collect()
}

/// Resolve `base/relative` without touching the filesystem, so the comparison
/// is about the declared path rather than whatever happens to exist locally.
fn normalize(base: &Path, relative: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Rust sources belonging to the crate rooted at `dir`.
///
/// Skips any subdirectory that has its own `Cargo.toml`, mirroring cargo: a
/// nested package is a separate crate and its files are excluded from the parent's
/// tarball. Without this the root package (whose directory is the repo root) would
/// vacuum up every other member's sources and misattribute their includes.
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default();
            // `target/` is build output; `.`-prefixed dirs are tooling.
            if name == "target" || name.to_string_lossy().starts_with('.') {
                continue;
            }
            if path.join("Cargo.toml").is_file() {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// One compile-time file reference and the directory its path is relative to.
struct Include {
    /// Path as written, with any `CARGO_MANIFEST_DIR` prefix already stripped.
    path: String,
    /// True when the path is anchored at the crate root rather than the source file.
    from_crate_root: bool,
}

/// Every compile-time file reference in `source`.
///
/// Handles the four spellings that appear in this workspace. The naive "first
/// string literal" scan this replaced silently missed the `concat!` form and let
/// seven escaping includes through in `zeroclaw-hardware`; `bindgen!` was missed
/// in turn because it is not an `include_*` macro at all, yet reads a directory
/// from disk at compile time exactly like one:
///
/// ```ignore
/// include_str!("../locales/en/tools.ftl")                     // file-relative
/// include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/x"))   // crate-root-relative
/// include_dir!("$CARGO_MANIFEST_DIR/../../web/dist")          // crate-root-relative
/// bindgen!({ world: "w", path: "wit/v0" })                    // crate-root-relative
/// ```
fn string_literal(token: &proc_macro2::Literal) -> Option<String> {
    syn::parse_str::<syn::LitStr>(&token.to_string())
        .ok()
        .map(|literal| literal.value())
}

fn append_string_literals(tokens: TokenStream, out: &mut String) {
    for token in tokens {
        match token {
            TokenTree::Literal(literal) => {
                if let Some(value) = string_literal(&literal) {
                    out.push_str(&value);
                }
            }
            TokenTree::Group(group) => append_string_literals(group.stream(), out),
            TokenTree::Ident(_) | TokenTree::Punct(_) => {}
        }
    }
}

fn bindgen_path(tokens: TokenStream) -> Option<String> {
    let mut after_path = false;
    for token in tokens {
        match token {
            TokenTree::Ident(ident) if ident == "path" => after_path = true,
            TokenTree::Literal(literal) if after_path => return string_literal(&literal),
            TokenTree::Punct(punct) if after_path && punct.as_char() == ',' => {
                after_path = false;
            }
            TokenTree::Group(group) => {
                if let Some(path) = bindgen_path(group.stream()) {
                    return Some(path);
                }
            }
            TokenTree::Ident(_) | TokenTree::Literal(_) | TokenTree::Punct(_) => {}
        }
    }
    None
}

fn is_test_cfg(attr: &syn::Attribute) -> bool {
    attr.path().is_ident("cfg")
        && attr
            .meta
            .require_list()
            .is_ok_and(|list| list.tokens.to_string() == "test")
}

#[derive(Default)]
struct IncludeVisitor {
    found: Vec<Include>,
}

impl<'ast> Visit<'ast> for IncludeVisitor {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        // Cargo packages test source, but package verification and downstream
        // builds do not compile it. Skip any parsed inline test module while
        // continuing to inspect production items that follow it.
        if node.content.is_some() && node.attrs.iter().any(is_test_cfg) {
            return;
        }
        visit::visit_item_mod(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let Some(name) = node
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
        else {
            return;
        };
        if !matches!(
            name.as_str(),
            "include_str" | "include_bytes" | "include_dir" | "bindgen"
        ) {
            visit::visit_macro(self, node);
            return;
        }

        let anchored = name == "bindgen" || node.tokens.to_string().contains("CARGO_MANIFEST_DIR");
        let mut literals = if name == "bindgen" {
            bindgen_path(node.tokens.clone()).unwrap_or_default()
        } else {
            let mut joined = String::new();
            append_string_literals(node.tokens.clone(), &mut joined);
            joined
        };
        literals = literals
            .replace("$CARGO_MANIFEST_DIR", "")
            .replace("CARGO_MANIFEST_DIR", "");
        let path = literals.trim_start_matches('/').to_owned();
        if !path.is_empty() {
            self.found.push(Include {
                path,
                from_crate_root: anchored,
            });
        }
        visit::visit_macro(self, node);
    }
}

fn compile_time_includes(source: &str) -> Vec<Include> {
    let file = syn::parse_file(source).expect("published Rust source parses for include scan");
    let mut visitor = IncludeVisitor::default();
    visitor.visit_file(&file);
    visitor.found
}

#[test]
fn compile_time_include_scan_visits_only_production_macros() {
    let source = r#"
/// This used to be `include_str!("../../comment-only.txt")`.
const PRODUCTION: &str = include_str!("../production.txt");
const BINARY: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/firmware.bin"));
const DIRECTORY: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets");
wasmtime::component::bindgen!({ world: "demo", path: "wit/v0" });

#[cfg(test)]
mod tests {
    const FIXTURE: &str = include_str!("../../test-fixture.txt");
}

const AFTER_TESTS: &str = include_str!("../after-tests.txt");
"#;
    let includes: Vec<(String, bool)> = compile_time_includes(source)
        .into_iter()
        .map(|include| (include.path, include.from_crate_root))
        .collect();
    assert_eq!(
        includes,
        [
            ("../production.txt".to_owned(), false),
            ("firmware.bin".to_owned(), true),
            ("assets".to_owned(), true),
            ("wit/v0".to_owned(), true),
            ("../after-tests.txt".to_owned(), false),
        ]
    );
}

fn root_closure(crates: &[Crate]) -> BTreeSet<String> {
    let by_name: BTreeMap<&str, &Crate> = crates
        .iter()
        .map(|krate| (krate.name.as_str(), krate))
        .collect();
    let names: BTreeSet<String> = crates.iter().map(|krate| krate.name.clone()).collect();
    let mut reachable = BTreeSet::new();
    let mut stack = vec![ROOT_PACKAGE.to_owned()];

    while let Some(name) = stack.pop() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        let Some(krate) = by_name.get(name.as_str()) else {
            continue;
        };
        stack.extend(internal_deps(krate, &names));
    }
    reachable
}

#[test]
fn root_package_is_the_installable_crate() {
    let crates = workspace_crates();
    let root = crates
        .iter()
        .find(|c| c.dir == repo_root())
        .expect("workspace has a root package");

    assert_eq!(
        root.name, ROOT_PACKAGE,
        "the root package must stay `{ROOT_PACKAGE}` so `cargo install {ROOT_PACKAGE}` \
         matches the binary name"
    );
    assert!(
        root.publishable,
        "the root package must remain publishable; `publish = false` here silently \
         removes ZeroClaw from crates.io"
    );
}

#[test]
fn release_set_is_exactly_the_root_closure_plus_companion_apps() {
    let crates = workspace_crates();
    let by_name: BTreeMap<&str, &Crate> = crates
        .iter()
        .map(|krate| (krate.name.as_str(), krate))
        .collect();
    let version = workspace_version();
    let mut expected: BTreeSet<&str> = root_closure(&crates)
        .iter()
        .filter_map(|name| {
            by_name
                .get(name.as_str())
                .filter(|krate| krate.version == version)
                .map(|krate| krate.name.as_str())
        })
        .collect();
    expected.insert("zerocode");
    expected.insert("zerorelay");

    let actual = release_crates(&crates);
    assert_eq!(
        actual, expected,
        "the coordinated release must contain the root dependency closure plus zerocode and \
         zerorelay, and no test fixture, desktop bundle, or maintainer tool"
    );

    for name in &actual {
        let krate = by_name[name];
        assert_eq!(
            krate.manifest["package"]["publish"].as_bool(),
            Some(true),
            "{name} must declare `publish = true` explicitly"
        );
        assert!(
            krate.manifest["package"]
                .as_table()
                .is_some_and(|package| package.contains_key("repository")),
            "{name} must inherit the workspace repository metadata"
        );
    }
}

#[test]
fn publishable_crates_never_depend_on_private_crates() {
    let crates = workspace_crates();
    let by_name: BTreeMap<&str, &Crate> = crates.iter().map(|c| (c.name.as_str(), c)).collect();
    let reachable = root_closure(&crates);

    let private: Vec<&str> = reachable
        .iter()
        .filter(|n| by_name.get(n.as_str()).is_some_and(|c| !c.publishable))
        .map(String::as_str)
        .collect();

    assert!(
        private.is_empty(),
        "these crates are reachable from `{ROOT_PACKAGE}` but are not publishable: {private:?}.\n\
         cargo strips `path` when publishing and resolves the `version` from crates.io, so the \
         publish fails partway through — after earlier crates have irreversibly uploaded.\n\
         Either publish them or remove the dependency edge."
    );
}

#[test]
fn publishable_crates_pin_git_dependencies_to_a_version() {
    let crates = workspace_crates();
    let release = release_crates(&crates);
    for krate in crates
        .iter()
        .filter(|krate| release.contains(krate.name.as_str()))
    {
        for section in ["dependencies", "build-dependencies", "target"] {
            let Some(table) = krate.manifest.get(section).and_then(|v| v.as_table()) else {
                continue;
            };
            // `[target.<cfg>.dependencies]` nests one level deeper.
            let dep_tables: Vec<&toml::Table> = if section == "target" {
                table
                    .values()
                    .filter_map(|v| v.as_table())
                    .filter_map(|t| t.get("dependencies"))
                    .filter_map(|v| v.as_table())
                    .collect()
            } else {
                vec![table]
            };

            for deps in dep_tables {
                for (dep_name, spec) in deps {
                    let Some(spec) = spec.as_table() else {
                        continue;
                    };
                    if spec.contains_key("git") && !spec.contains_key("version") {
                        panic!(
                            "{}: dependency `{dep_name}` has a `git` source but no `version`.\n\
                             crates.io rejects git sources; cargo drops `git`/`rev` at publish \
                             time and keeps only the version requirement, so one is required.",
                            krate.name
                        );
                    }
                }
            }
        }
    }
}

/// Top-level entries of a manifest's `package.include`, if it has one.
///
/// Returns `None` when the crate ships everything cargo would collect by default.
/// Entries are reduced to their first path segment (`/src/**/*` -> `src`), which is
/// all this test needs to decide which directories to walk.
fn packaged_roots(krate: &Crate) -> Option<Vec<String>> {
    let include = krate.manifest["package"].get("include")?.as_array()?;
    Some(
        include
            .iter()
            .filter_map(|v| v.as_str())
            .filter_map(|entry| {
                entry
                    .trim_start_matches('/')
                    .split('/')
                    .next()
                    .filter(|s| !s.is_empty() && !s.contains('*'))
                    .map(str::to_owned)
            })
            .collect(),
    )
}

fn collect_dangling_symlinks(path: &Path, package_root: &Path, out: &mut BTreeSet<PathBuf>) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        if !path.exists() {
            out.insert(path.to_path_buf());
        }
        return;
    }
    if !metadata.is_dir() {
        return;
    }
    if path != package_root && path.join("Cargo.toml").is_file() {
        return;
    }

    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        let name = child.file_name().unwrap_or_default();
        if name == "target" || name == ".git" {
            continue;
        }
        collect_dangling_symlinks(&child, package_root, out);
    }
}

#[test]
fn published_crates_never_ship_dangling_symlinks() {
    let crates = workspace_crates();
    let release = release_crates(&crates);
    let mut dangling = BTreeSet::new();

    for krate in crates
        .iter()
        .filter(|krate| release.contains(krate.name.as_str()))
    {
        match packaged_roots(krate) {
            Some(roots) => {
                for root in roots {
                    collect_dangling_symlinks(&krate.dir.join(root), &krate.dir, &mut dangling);
                }
            }
            None => collect_dangling_symlinks(&krate.dir, &krate.dir, &mut dangling),
        }
    }

    let paths: Vec<String> = dangling
        .iter()
        .map(|path| {
            path.strip_prefix(repo_root())
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect();
    assert!(
        paths.is_empty(),
        "published crates contain dangling symlinks that `cargo package` cannot archive: {paths:?}"
    );
}

/// Relative path literals consumed directly by a package build script.
///
/// This complements the compile-time macro scanner: the ZeroCode packaging
/// failure that motivated the gate used `Path::join("../../web/...")` followed
/// by `read_to_string`, so it contained no `include_*` macro for that scanner to
/// see. Restricting this to top-level build scripts avoids treating ordinary
/// runtime paths and test fixtures as package inputs.
fn build_script_relative_paths(source: &str) -> Vec<String> {
    const CALLS: [&str; 4] = [
        ".join(\"",
        "read_to_string(\"",
        "std::fs::read(\"",
        "std::fs::read_to_string(\"",
    ];
    let mut paths = Vec::new();
    for call in CALLS {
        let mut rest = source;
        while let Some(start) = rest.find(call) {
            let value = &rest[start + call.len()..];
            let Some(end) = value.find('"') else {
                break;
            };
            let literal = &value[..end];
            if Path::new(literal)
                .components()
                .any(|component| component == Component::ParentDir)
            {
                paths.push(literal.to_owned());
            }
            rest = &value[end + 1..];
        }
    }
    paths
}

#[test]
fn published_build_scripts_never_read_outside_their_package() {
    let crates = workspace_crates();
    let release = release_crates(&crates);
    let mut violations = Vec::new();

    for krate in crates
        .iter()
        .filter(|krate| release.contains(krate.name.as_str()))
    {
        let script = krate.dir.join("build.rs");
        let Ok(source) = fs::read_to_string(&script) else {
            continue;
        };
        for relative in build_script_relative_paths(&source) {
            let resolved = normalize(&krate.dir, &relative);
            if !resolved.starts_with(&krate.dir) {
                violations.push(format!(
                    "  {} ({}) reads `{relative}` -> {}",
                    script
                        .strip_prefix(repo_root())
                        .unwrap_or(&script)
                        .display(),
                    krate.name,
                    resolved.display()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "these build scripts read inputs outside the package directory:\n{}\n\n\
         The source checkout contains those files, but `cargo package` does not. Materialize the \
         input inside the package and add a drift check instead of weakening this gate.",
        violations.join("\n")
    );
}

/// The one compile-time include allowed to escape its crate, with the reason.
///
/// `embedded-web` bakes the built dashboard into the gateway. `web/dist` is
/// gitignored build output, so it cannot be in a registry tarball at all unless
/// the crate grows a full `include` allowlist — no path fix helps. The feature is
/// declared a non-user-selectable meta toggle in the root manifest's
/// `[package.metadata.zeroclaw] non_row_features`, is off by default in both the
/// gateway and the root crate, and its `build.rs` fails loudly with an actionable
/// message ("run: cargo web build") rather than producing a broken binary. It is
/// therefore source-checkout-only by design.
///
/// Do not add entries here to silence a new violation — fix the include instead.
const ESCAPE_EXCEPTIONS: &[(&str, &str)] = &[(
    "crates/zeroclaw-gateway/src/static_files.rs",
    "../../web/dist",
)];

#[test]
fn published_crates_never_include_files_outside_their_own_directory() {
    let mut violations = Vec::new();
    let crates = workspace_crates();
    let release = release_crates(&crates);

    for krate in crates
        .iter()
        .filter(|krate| release.contains(krate.name.as_str()))
    {
        let mut sources = Vec::new();
        // Scan what the crate actually ships. When the manifest carries an
        // `include` allowlist (the root crate does, and it deliberately omits
        // `tests/`), honour it — scanning unpackaged files produces findings about
        // code no consumer ever compiles. Otherwise scan the whole crate, since
        // build.rs and any other in-crate Rust file is packaged and compiled too.
        match packaged_roots(krate) {
            Some(roots) => {
                for root in roots {
                    let path = krate.dir.join(&root);
                    if path.is_dir() {
                        rust_sources(&path, &mut sources);
                    } else if path.extension().is_some_and(|e| e == "rs") && path.is_file() {
                        sources.push(path);
                    }
                }
            }
            None => rust_sources(&krate.dir, &mut sources),
        }

        for source_path in sources {
            let text = fs::read_to_string(&source_path).expect("read source file");
            for include in compile_time_includes(&text) {
                let base = if include.from_crate_root {
                    krate.dir.clone()
                } else {
                    source_path
                        .parent()
                        .expect("source file has a parent")
                        .to_path_buf()
                };
                let resolved = normalize(&base, &include.path);
                let rel = source_path
                    .strip_prefix(repo_root())
                    .unwrap_or(&source_path)
                    .to_string_lossy()
                    .into_owned();
                let excepted = ESCAPE_EXCEPTIONS
                    .iter()
                    .any(|(f, p)| *f == rel && *p == include.path);

                // Inside the crate directory is necessary but not sufficient. When
                // the manifest has an `include` allowlist, the file must also fall
                // under one of those entries — otherwise it is inside the crate and
                // still absent from the tarball. This matters most for the root
                // package, whose directory is the whole repository: without it,
                // an include reaching into `crates/` would look contained.
                let inside_crate = resolved.starts_with(&krate.dir);
                let shipped = match packaged_roots(krate) {
                    Some(roots) => roots
                        .iter()
                        .any(|r| resolved.starts_with(krate.dir.join(r))),
                    None => true,
                };
                if (!inside_crate || !shipped) && !excepted {
                    violations.push(format!(
                        "  {} ({})\n      includes `{}`\n      -> {}",
                        source_path
                            .strip_prefix(repo_root())
                            .unwrap_or(&source_path)
                            .display(),
                        krate.name,
                        include.path,
                        resolved.display(),
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "these compile-time includes reference files the published crate will not contain:\n{}\n\n\
         `cargo package` archives only files under the crate root, and only those matching the \
         manifest's `include` list when it has one — so each of these compiles in the workspace \
         and then fails from the published tarball. Feature-gated ones are especially dangerous: \
         `cargo publish --dry-run` verifies default features only, so they pass preflight and \
         ship broken.\n\
         Fix by including through an in-crate symlink (see crates/zeroclaw-hardware/firmware \
         and crates/zeroclaw-tools/locales) rather than reaching up out of the crate.",
        violations.join("\n"),
    );
}
