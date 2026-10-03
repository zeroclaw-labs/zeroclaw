//! Architecture ratchet: authority-sensitive effects stay behind the recheck.
//!
//! `security::authority` makes a recheck at the effect a type obligation:
//! an effect runs inside `Effect::commit`, and an `Effect` exists only after
//! `Admitted::recheck` succeeded under the operation's own proof. The types
//! cannot see everything. A raw sink reachable from anywhere else, a proof
//! minted outside the wait it stands for, or a background grant built outside
//! the envelope all compile and pass every existing test. This gate turns
//! those into failures.
//!
//! What it guards is the sinks and the identifiers that stand for authority,
//! not a hand-kept list of effect functions: a list only guards what is on
//! it. New entry points are found from canonical sources that exist whether
//! or not anyone remembers to list them: the `Method` enum and every
//! `impl AuthorizedOp`.
//!
//! Rules (numbers match the ratchet design):
//!
//! - C1  a restricted identifier (proof constructors, `trusted_internal`,
//!   test-only fabricators, raw sink capabilities) appears only in its
//!   allowed modules, or in test code where that is permitted;
//! - C2/C6 in an operation's site module, a raw sink capability appears only
//!   inside a closure passed to `.commit(..)`;
//! - C3  a field whose type names a sink capability is private and lives in
//!   an allowed module;
//! - C4  a function whose signature names a sink capability is not `pub`
//!   and lives in an allowed module;
//! - C5  mutating SQL on an audited table appears only in that table's
//!   storage module (or test code);
//! - C7  `ExecutionGrants` is built only in `principal_envelope`;
//! - C9  every `Method` variant is classified, or on the legacy list, which
//!   can only shrink;
//! - C10 every non-test `impl AuthorizedOp` is registered, and every
//!   registration still has an impl;
//! - C11 the four per-site test names are reserved for the site-test macro;
//!   a hand-written function carrying one fails.
//!
//! What it cannot do: classify arbitrary Rust as free of side effects. A new
//! kind of sink (a new table, a new external call) is guarded only once it is
//! added to `SINKS` or `AUDITED_TABLES`. The scan is identifier-level with no
//! type inference, so it over-approximates and fails safe; capability names
//! must be distinctive for that reason.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};
use syn::visit::{self, Visit};

// ─── Policy ────────────────────────────────────────────────────────────────

/// An identifier that may appear only in the listed modules.
struct Restricted {
    ident: &'static str,
    /// Module-path prefixes (`crate::a::b`) where the identifier may appear.
    allowed: &'static [&'static str],
    /// Whether test code anywhere may name it.
    allow_in_tests: bool,
    why: &'static str,
}

/// A raw sink reached only through a named capability type.
struct Sink {
    capability: &'static str,
    /// Modules that implement or wire the capability (anywhere in them).
    owners: &'static [&'static str],
    /// Site modules, where it may appear only inside a `.commit(..)` closure.
    sites: &'static [&'static str],
}

/// A registered operation: its type name and the module holding its commit.
struct RegisteredOp {
    name: &'static str,
    site: &'static str,
}

struct Policy {
    restricted: Vec<Restricted>,
    sinks: Vec<Sink>,
    audited_tables: Vec<&'static str>,
    /// Repo-relative files that own writes to the audited tables.
    sql_owners: Vec<&'static str>,
    ops: Vec<RegisteredOp>,
}

const ENVELOPE_MODULE: &str = "zeroclaw_runtime::security::principal_envelope";
const AUTHORITY_MODULE: &str = "zeroclaw_runtime::security::authority";

/// The per-site test names. Only the site-test macro may produce them, so a
/// function carrying one cannot be an empty, hand-written stand-in.
const RESERVED_TEST_SUFFIXES: &[&str] = &[
    "_revoked_after_admission_has_no_effect",
    "_narrowed_after_admission_has_no_effect",
    "_widened_after_admission_is_honoured",
    "_resource_reowned_after_admission_is_refused",
];

fn workspace_policy() -> Policy {
    Policy {
        restricted: vec![
            Restricted {
                ident: "after_decision_wait",
                allowed: &[AUTHORITY_MODULE, "zeroclaw_runtime::sop::dispatch"],
                allow_in_tests: false,
                why: "a DecisionSettled proof is minted only where the decision wait returns",
            },
            Restricted {
                ident: "after_claim",
                allowed: &[AUTHORITY_MODULE, "zeroclaw_runtime::cron::scheduler"],
                allow_in_tests: false,
                why: "a SchedulerClaim proof is minted only once the job row is claimed",
            },
            Restricted {
                // The delivery path does not exist yet; its migration adds
                // the module that reserves outbound capacity here.
                ident: "after_reservation",
                allowed: &[AUTHORITY_MODULE],
                allow_in_tests: false,
                why: "a DeliverySlot proof is minted only after the outbound reservation",
            },
            Restricted {
                ident: "trusted_internal",
                allowed: &[ENVELOPE_MODULE],
                allow_in_tests: true,
                why: "an internal-origin envelope is minted only by the daemon's own starters",
            },
            Restricted {
                ident: "fabricate_for_tests",
                allowed: &[],
                allow_in_tests: true,
                why: "a fabricated proof is for exercising the types in tests only",
            },
        ],
        // Populated as sites migrate: each migration splits its raw sink out
        // behind a capability and registers it here.
        sinks: Vec::new(),
        audited_tables: vec!["sessions", "session_metadata", "cron_jobs", "sop_runs"],
        sql_owners: vec![
            "crates/zeroclaw-infra/src/session_sqlite.rs",
            "crates/zeroclaw-runtime/src/cron/store.rs",
            "crates/zeroclaw-runtime/src/sop/store/sqlite.rs",
            // WhatsApp's own protocol-session table, in its own database;
            // it shares the name `sessions` only.
            "crates/zeroclaw-channels/src/whatsapp_storage.rs",
        ],
        ops: Vec::new(),
    }
}

/// `Method` variants not yet classified as an operation, read-only, or exempt.
/// The list may only shrink: a new variant must be classified, and removing an
/// entry must lower `LEGACY_METHOD_CEILING` to match.
const LEGACY_UNCLASSIFIED_METHODS: &[&str] = &[
    "Initialize",
    "Status",
    "Health",
    "DoctorRun",
    "SessionNew",
    "SessionClose",
    "SessionPrompt",
    "SessionConfigure",
    "SessionCancel",
    "SessionGitBranch",
    "SessionList",
    "SessionListAcp",
    "SessionMessages",
    "SessionState",
    "SessionDelete",
    "SessionApprove",
    "SessionKill",
    "MemoryList",
    "MemorySearch",
    "MemoryGet",
    "MemoryStore",
    "MemoryDelete",
    "CronList",
    "CronGet",
    "CronAdd",
    "CronPatch",
    "CronDelete",
    "CronRuns",
    "CronTrigger",
    "CronSettings",
    "ConfigGet",
    "ConfigSet",
    "ConfigSetMany",
    "ConfigValidate",
    "ConfigReload",
    "ConfigList",
    "ConfigDelete",
    "ConfigMapKeys",
    "ConfigResolveAliasSource",
    "ConfigMapKeyCreate",
    "ConfigMapKeyDelete",
    "ConfigMapKeyRename",
    "ConfigTemplates",
    "AgentsList",
    "AgentsStatus",
    "CostQuery",
    "CostOrg",
    "SkillsBundles",
    "SkillsList",
    "SkillsRead",
    "SkillsWrite",
    "SkillsDelete",
    "PersonalityList",
    "PersonalityGet",
    "PersonalityPut",
    "PersonalityTemplates",
    "ConfigSections",
    "ConfigStatus",
    "ConfigCatalog",
    "ConfigCatalogModels",
    "LogsSubscribe",
    "LogsQuery",
    "LogsGet",
    "TuiList",
    "FileAttach",
    "FsListDir",
    "LocalesList",
    "LocalesFetch",
    "QuickstartState",
    "QuickstartFields",
    "QuickstartValidate",
    "QuickstartApply",
    "QuickstartDismiss",
    "CertRenew",
    "SopsList",
    "SopsGet",
    "SopsGraph",
    "SopsRun",
    "SopsRuns",
    "SopsRunDetail",
    "SopsRunOverlay",
    "SopsValidate",
    "SopsSave",
    "SopsCreate",
    "SopsDelete",
    "SopsRename",
    "SopsDecide",
    "SopsWireDraft",
    "SopsGraphDraft",
    "SopsTriggerSources",
    "ToolsParamOptions",
];
const LEGACY_METHOD_CEILING: usize = 91;

/// Classified `Method` variants: `(variant, classification)`. Empty until the
/// first site migrates.
const CLASSIFIED_METHODS: &[(&str, &str)] = &[("EventsHistory", "read_only")];

// ─── Source model ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Kind {
    Ident(String),
    FnDef(String),
    ImplOp(String),
    StructLit(String),
    Sql {
        table: String,
    },
    Field {
        is_pub: bool,
        mentions: BTreeSet<String>,
    },
    FnSig {
        name: String,
        is_pub: bool,
        mentions: BTreeSet<String>,
    },
}

#[derive(Debug, Clone)]
struct Hit {
    kind: Kind,
    file: String,
    line: usize,
    module: String,
    in_test: bool,
    in_commit_closure: bool,
}

impl Hit {
    fn at(&self) -> String {
        format!("{}:{} ({})", self.file, self.line, self.module)
    }
}

fn is_test_attr(attr: &syn::Attribute) -> bool {
    let path = attr.path();
    if path.segments.last().is_some_and(|s| s.ident == "test") {
        return true;
    }
    if !path.is_ident("cfg") {
        return false;
    }
    let Ok(list) = attr.meta.require_list() else {
        return false;
    };
    let mut saw_test = false;
    let mut saw_not = false;
    walk_tokens(&list.tokens, &mut |tt| match tt {
        TokenTree::Ident(i) if i == "test" => saw_test = true,
        TokenTree::Ident(i) if i == "not" => saw_not = true,
        TokenTree::Literal(l) if l.to_string() == "\"test-util\"" => saw_test = true,
        _ => {}
    });
    saw_test && !saw_not
}

fn walk_tokens(tokens: &TokenStream, f: &mut impl FnMut(&TokenTree)) {
    for tt in tokens.clone() {
        f(&tt);
        if let TokenTree::Group(g) = &tt {
            walk_tokens(&g.stream(), f);
        }
    }
}

fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    use syn::Item::*;
    match item {
        Const(i) => &i.attrs,
        Enum(i) => &i.attrs,
        ExternCrate(i) => &i.attrs,
        Fn(i) => &i.attrs,
        ForeignMod(i) => &i.attrs,
        Impl(i) => &i.attrs,
        Macro(i) => &i.attrs,
        Mod(i) => &i.attrs,
        Static(i) => &i.attrs,
        Struct(i) => &i.attrs,
        Trait(i) => &i.attrs,
        TraitAlias(i) => &i.attrs,
        Type(i) => &i.attrs,
        Union(i) => &i.attrs,
        Use(i) => &i.attrs,
        _ => &[],
    }
}

/// Mutating SQL in `text` that names one of `tables`, as `(table)` matches.
fn sql_writes(text: &str, tables: &[&str]) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect();
    let mut found = Vec::new();
    for i in 0..words.len() {
        let target = match words[i] {
            "update" => words.get(i + 1),
            "delete" if words.get(i + 1) == Some(&"from") => words.get(i + 2),
            "replace" if words.get(i + 1) == Some(&"into") => words.get(i + 2),
            "insert" if words.get(i + 1) == Some(&"into") => words.get(i + 2),
            "insert" if words.get(i + 1) == Some(&"or") && words.get(i + 3) == Some(&"into") => {
                words.get(i + 4)
            }
            _ => None,
        };
        if let Some(t) = target
            && tables.contains(t)
        {
            found.push((*t).to_string());
        }
    }
    found
}

struct IdentCollector(BTreeSet<String>);

impl<'ast> Visit<'ast> for IdentCollector {
    fn visit_ident(&mut self, i: &'ast proc_macro2::Ident) {
        self.0.insert(i.to_string());
    }
}

fn idents_in_type(ty: &syn::Type) -> BTreeSet<String> {
    let mut c = IdentCollector(BTreeSet::new());
    c.visit_type(ty);
    c.0
}

/// What the scan records. Everything else is skipped, so the workspace scan
/// stays proportional to the rules rather than to the source size.
struct Watch {
    idents: BTreeSet<&'static str>,
    capabilities: BTreeSet<&'static str>,
    tables: Vec<&'static str>,
}

impl Watch {
    fn of(policy: &Policy) -> Self {
        let capabilities: BTreeSet<&'static str> =
            policy.sinks.iter().map(|s| s.capability).collect();
        let mut idents: BTreeSet<&'static str> =
            policy.restricted.iter().map(|r| r.ident).collect();
        idents.extend(capabilities.iter().copied());
        Watch {
            idents,
            capabilities,
            tables: policy.audited_tables.clone(),
        }
    }

    fn names_capability(&self, mentions: &BTreeSet<String>) -> bool {
        mentions
            .iter()
            .any(|m| self.capabilities.contains(m.as_str()))
    }
}

struct Scan<'p> {
    file: String,
    module: Vec<String>,
    base_in_test: bool,
    test_depth: usize,
    commit_depth: usize,
    watch: &'p Watch,
    test_modules: &'p BTreeSet<String>,
    hits: Vec<Hit>,
    /// Module paths of `#[cfg(test)]` modules declared here (pass one).
    declared_test_modules: Vec<String>,
}

impl Scan<'_> {
    fn module_path(&self) -> String {
        self.module.join("::")
    }

    fn in_test(&self) -> bool {
        if self.base_in_test || self.test_depth > 0 {
            return true;
        }
        let here = self.module_path();
        self.test_modules
            .iter()
            .any(|m| here == *m || here.starts_with(&format!("{m}::")))
    }

    fn push(&mut self, kind: Kind, line: usize) {
        let hit = Hit {
            kind,
            file: self.file.clone(),
            line,
            module: self.module_path(),
            in_test: self.in_test(),
            in_commit_closure: self.commit_depth > 0,
        };
        self.hits.push(hit);
    }

    fn sql(&mut self, text: &str, line: usize) {
        for table in sql_writes(text, &self.watch.tables) {
            self.push(Kind::Sql { table }, line);
        }
    }

    fn tokens(&mut self, tokens: &TokenStream) {
        let mut found = Vec::new();
        walk_tokens(tokens, &mut |tt| match tt {
            TokenTree::Ident(i) => found.push((Some(i.to_string()), None, i.span().start().line)),
            TokenTree::Literal(l) => {
                if let Ok(s) = syn::parse_str::<syn::LitStr>(&l.to_string()) {
                    found.push((None, Some(s.value()), l.span().start().line));
                }
            }
            _ => {}
        });
        for (ident, text, line) in found {
            if let Some(ident) = ident
                && self.watch.idents.contains(ident.as_str())
            {
                self.push(Kind::Ident(ident), line);
            }
            if let Some(text) = text {
                self.sql(&text, line);
            }
        }
    }

    fn fn_sig(&mut self, sig: &syn::Signature, is_pub: bool) {
        let mut mentions = BTreeSet::new();
        for input in &sig.inputs {
            if let syn::FnArg::Typed(t) = input {
                mentions.extend(idents_in_type(&t.ty));
            }
        }
        if let syn::ReturnType::Type(_, ty) = &sig.output {
            mentions.extend(idents_in_type(ty));
        }
        let line = sig.ident.span().start().line;
        let name = sig.ident.to_string();
        if RESERVED_TEST_SUFFIXES.iter().any(|s| name.ends_with(s)) {
            self.push(Kind::FnDef(name.clone()), line);
        }
        if self.watch.names_capability(&mentions) {
            self.push(
                Kind::FnSig {
                    name,
                    is_pub,
                    mentions,
                },
                line,
            );
        }
    }
}

impl<'ast> Visit<'ast> for Scan<'_> {
    // Attributes carry doc comments and cfg predicates, not effects.
    fn visit_attribute(&mut self, _: &'ast syn::Attribute) {}

    fn visit_item(&mut self, item: &'ast syn::Item) {
        let test = item_attrs(item).iter().any(is_test_attr);
        self.test_depth += usize::from(test);
        visit::visit_item(self, item);
        self.test_depth -= usize::from(test);
    }

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if m.attrs.iter().any(is_test_attr) {
            let mut path = self.module.clone();
            path.push(m.ident.to_string());
            self.declared_test_modules.push(path.join("::"));
        }
        if let Some((_, items)) = &m.content {
            self.module.push(m.ident.to_string());
            for item in items {
                self.visit_item(item);
            }
            self.module.pop();
        }
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        let is_pub = !matches!(f.vis, syn::Visibility::Inherited);
        self.fn_sig(&f.sig, is_pub);
        visit::visit_item_fn(self, f);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        let test = f.attrs.iter().any(is_test_attr);
        self.test_depth += usize::from(test);
        let is_pub = !matches!(f.vis, syn::Visibility::Inherited);
        self.fn_sig(&f.sig, is_pub);
        visit::visit_impl_item_fn(self, f);
        self.test_depth -= usize::from(test);
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        self.fn_sig(&f.sig, true);
        visit::visit_trait_item_fn(self, f);
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        if let Some((_, path, _)) = &i.trait_
            && path
                .segments
                .last()
                .is_some_and(|s| s.ident == "AuthorizedOp")
            && let syn::Type::Path(tp) = &*i.self_ty
            && let Some(seg) = tp.path.segments.last()
        {
            self.push(
                Kind::ImplOp(seg.ident.to_string()),
                seg.ident.span().start().line,
            );
        }
        visit::visit_item_impl(self, i);
    }

    fn visit_field(&mut self, f: &'ast syn::Field) {
        let mentions = idents_in_type(&f.ty);
        if self.watch.names_capability(&mentions) {
            let is_pub = !matches!(f.vis, syn::Visibility::Inherited);
            let line = f.ident.as_ref().map_or(0, |i| i.span().start().line);
            self.push(Kind::Field { is_pub, mentions }, line);
        }
        visit::visit_field(self, f);
    }

    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        self.visit_expr(&m.receiver);
        self.visit_ident(&m.method);
        if let Some(t) = &m.turbofish {
            self.visit_angle_bracketed_generic_arguments(t);
        }
        let commit = m.method == "commit";
        for arg in &m.args {
            let closure = commit && matches!(arg, syn::Expr::Closure(_));
            self.commit_depth += usize::from(closure);
            self.visit_expr(arg);
            self.commit_depth -= usize::from(closure);
        }
    }

    fn visit_expr_struct(&mut self, s: &'ast syn::ExprStruct) {
        if let Some(seg) = s.path.segments.last()
            && seg.ident == "ExecutionGrants"
        {
            self.push(
                Kind::StructLit(seg.ident.to_string()),
                seg.ident.span().start().line,
            );
        }
        visit::visit_expr_struct(self, s);
    }

    fn visit_lit_str(&mut self, s: &'ast syn::LitStr) {
        self.sql(&s.value(), s.span().start().line);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        visit::visit_macro(self, m);
        self.tokens(&m.tokens);
    }

    fn visit_ident(&mut self, i: &'ast proc_macro2::Ident) {
        let name = i.to_string();
        if self.watch.idents.contains(name.as_str()) {
            self.push(Kind::Ident(name), i.span().start().line);
        }
    }
}

fn scan_ast(
    file: &str,
    module: &str,
    base_in_test: bool,
    ast: &syn::File,
    watch: &Watch,
    test_modules: &BTreeSet<String>,
) -> (Vec<Hit>, Vec<String>) {
    let mut scan = Scan {
        file: file.to_string(),
        module: module.split("::").map(str::to_string).collect(),
        base_in_test,
        test_depth: 0,
        commit_depth: 0,
        watch,
        test_modules,
        hits: Vec::new(),
        declared_test_modules: Vec::new(),
    };
    scan.visit_file(ast);
    (scan.hits, scan.declared_test_modules)
}

fn scan_file(
    file: &str,
    module: &str,
    base_in_test: bool,
    src: &str,
    watch: &Watch,
    test_modules: &BTreeSet<String>,
) -> (Vec<Hit>, Vec<String>) {
    let ast = syn::parse_file(src).unwrap_or_else(|e| panic!("{file}: does not parse: {e}"));
    scan_ast(file, module, base_in_test, &ast, watch, test_modules)
}

// ─── Workspace walk ────────────────────────────────────────────────────────

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const SKIP_DIRS: &[&str] = &[
    "target",
    ".git",
    "node_modules",
    "fixtures",
    "firmware",
    "web",
    "docs",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_ref()) && !name.starts_with('.') {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// The nearest ancestor directory holding a `Cargo.toml` with a `[package]`,
/// and that package's name as a Rust identifier.
fn owning_crate(
    file: &Path,
    cache: &mut BTreeMap<PathBuf, Option<String>>,
) -> Option<(PathBuf, String)> {
    let mut dir = file.parent()?.to_path_buf();
    loop {
        let entry = cache.entry(dir.clone()).or_insert_with(|| {
            let manifest = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
            let table: toml::Table = manifest.parse().ok()?;
            let name = table.get("package")?.get("name")?.as_str()?;
            Some(name.replace('-', "_"))
        });
        if let Some(name) = entry.clone() {
            return Some((dir, name));
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// `(module path, is test code)` for a file, from its place in its crate.
fn file_module(file: &Path, crate_dir: &Path, crate_name: &str) -> (String, bool) {
    let rel = file.strip_prefix(crate_dir).unwrap_or(file);
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let (first, rest) = parts
        .split_first()
        .map_or(("", &parts[..]), |(f, r)| (f.as_str(), r));
    let is_test = matches!(first, "tests" | "benches" | "examples");
    let mut segs = vec![crate_name.to_string()];
    if first != "src" {
        segs.push(first.to_string());
    }
    for (i, part) in rest.iter().enumerate() {
        let last = i + 1 == rest.len();
        let stem = part.strip_suffix(".rs").unwrap_or(part);
        if last && matches!(stem, "lib" | "main" | "mod") {
            continue;
        }
        segs.push(stem.to_string());
    }
    (segs.join("::"), is_test)
}

struct Workspace {
    hits: Vec<Hit>,
}

fn scan_workspace(policy: &Policy) -> Workspace {
    let root = repo_root();
    let mut files = Vec::new();
    for top in ["src", "tests", "crates", "apps", "benches"] {
        rust_files(&root.join(top), &mut files);
    }
    files.sort();

    let mut cache = BTreeMap::new();
    let mut sources = Vec::new();
    for path in files {
        let Some((crate_dir, crate_name)) = owning_crate(&path, &mut cache) else {
            continue;
        };
        let src =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let ast = syn::parse_file(&src)
            .unwrap_or_else(|e| panic!("{}: does not parse: {e}", path.display()));
        let (module, is_test) = file_module(&path, &crate_dir, &crate_name);
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        sources.push((rel, module, is_test, ast));
    }

    // Pass one: every `#[cfg(test)] mod x;`, so out-of-line test files are
    // test code even though nothing in them says so.
    let watch = Watch::of(policy);
    let no_tests = BTreeSet::new();
    let mut test_modules = BTreeSet::new();
    for (rel, module, is_test, ast) in &sources {
        let (_, declared) = scan_ast(rel, module, *is_test, ast, &watch, &no_tests);
        test_modules.extend(declared);
    }

    let mut hits = Vec::new();
    for (rel, module, is_test, ast) in &sources {
        let (found, _) = scan_ast(rel, module, *is_test, ast, &watch, &test_modules);
        hits.extend(found);
    }
    Workspace { hits }
}

// ─── Rules ─────────────────────────────────────────────────────────────────

fn under(module: &str, prefix: &str) -> bool {
    module == prefix || module.starts_with(&format!("{prefix}::"))
}

fn under_any(module: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| under(module, p))
}

fn violations(hits: &[Hit], policy: &Policy) -> Vec<String> {
    let mut out = Vec::new();

    for hit in hits {
        match &hit.kind {
            Kind::Ident(name) => {
                // C1: restricted identifiers.
                for r in &policy.restricted {
                    if name == r.ident
                        && !under_any(&hit.module, r.allowed)
                        && !(r.allow_in_tests && hit.in_test)
                    {
                        out.push(format!("C1 `{name}` at {}: {}", hit.at(), r.why));
                    }
                }
                // C1/C2/C6: sink capabilities.
                for s in &policy.sinks {
                    if name != s.capability || under_any(&hit.module, s.owners) {
                        continue;
                    }
                    if under_any(&hit.module, s.sites) {
                        if !hit.in_commit_closure {
                            out.push(format!(
                                "C6 `{name}` at {} is used outside a `.commit(..)` closure",
                                hit.at()
                            ));
                        }
                    } else {
                        out.push(format!(
                            "C1 raw sink `{name}` at {} is outside its owners and sites",
                            hit.at()
                        ));
                    }
                }
            }
            Kind::Field { is_pub, mentions } => {
                for s in &policy.sinks {
                    if mentions.contains(s.capability)
                        && (*is_pub || !under_any(&hit.module, s.owners))
                    {
                        out.push(format!(
                            "C3 a field holding `{}` at {} must be private to its owner",
                            s.capability,
                            hit.at()
                        ));
                    }
                }
            }
            Kind::FnSig {
                name,
                is_pub,
                mentions,
            } => {
                for s in &policy.sinks {
                    if mentions.contains(s.capability)
                        && (*is_pub || !under_any(&hit.module, s.owners))
                    {
                        out.push(format!(
                            "C4 fn `{name}` at {} passes `{}` across its owner's boundary",
                            hit.at(),
                            s.capability
                        ));
                    }
                }
            }
            Kind::Sql { table } => {
                if !hit.in_test && !policy.sql_owners.contains(&hit.file.as_str()) {
                    out.push(format!(
                        "C5 mutating SQL on `{table}` at {} outside its storage module",
                        hit.at()
                    ));
                }
            }
            Kind::StructLit(name) => {
                if name == "ExecutionGrants" && !under(&hit.module, ENVELOPE_MODULE) {
                    out.push(format!(
                        "C7 `ExecutionGrants` built at {}; only resolve_for_execution may",
                        hit.at()
                    ));
                }
            }
            Kind::FnDef(name) => {
                if RESERVED_TEST_SUFFIXES.iter().any(|s| name.ends_with(s)) {
                    out.push(format!(
                        "C11 `{name}` at {} carries a reserved site-test name; only the site-test macro may",
                        hit.at()
                    ));
                }
            }
            Kind::ImplOp(_) => {}
        }
    }

    // C10: every non-test op is registered and implemented in its site; every
    // registration still has an impl.
    let impls: Vec<(&str, &Hit)> = hits
        .iter()
        .filter(|h| !h.in_test)
        .filter_map(|h| match &h.kind {
            Kind::ImplOp(name) => Some((name.as_str(), h)),
            _ => None,
        })
        .collect();
    for (name, hit) in &impls {
        match policy.ops.iter().find(|op| op.name == *name) {
            None => out.push(format!(
                "C10 `impl AuthorizedOp for {name}` at {} is not registered with its commit site",
                hit.at()
            )),
            Some(op) if !under(&hit.module, op.site) => out.push(format!(
                "C10 op `{name}` is implemented at {}, not in its registered site `{}`",
                hit.at(),
                op.site
            )),
            Some(_) => {}
        }
    }
    for op in &policy.ops {
        if !impls.iter().any(|(name, _)| *name == op.name) {
            out.push(format!("C10 registered op `{}` has no impl", op.name));
        }
    }

    out
}

// ─── Tests: the workspace ──────────────────────────────────────────────────

/// One scan of the workspace, shared by the tests that read it.
fn workspace() -> &'static Workspace {
    static SCAN: std::sync::OnceLock<Workspace> = std::sync::OnceLock::new();
    SCAN.get_or_init(|| scan_workspace(&workspace_policy()))
}

#[test]
fn authority_effects_stay_behind_the_recheck() {
    let policy = workspace_policy();
    let found = violations(&workspace().hits, &policy);
    assert!(
        found.is_empty(),
        "authority ratchet violations:\n  {}",
        found.join("\n  ")
    );
}

/// A green scan proves nothing if the walk missed the code it guards (a skip
/// list that is too broad, a module path computed wrongly). These are facts
/// about today's tree the scan must observe.
#[test]
fn the_scan_sees_the_code_it_guards() {
    let hits = &workspace().hits;
    let seen = |pred: &dyn Fn(&Hit) -> bool| hits.iter().any(pred);

    for ident in ["after_decision_wait", "after_claim", "after_reservation"] {
        assert!(
            seen(&|h| matches!(&h.kind, Kind::Ident(i) if i == ident)
                && h.module == AUTHORITY_MODULE
                && !h.in_test),
            "the scan did not see `{ident}` defined in {AUTHORITY_MODULE}"
        );
    }
    assert!(
        seen(
            &|h| matches!(&h.kind, Kind::Ident(i) if i == "trusted_internal")
                && h.module == ENVELOPE_MODULE
        ),
        "the scan did not see `trusted_internal` in {ENVELOPE_MODULE}"
    );
    assert!(
        seen(&|h| matches!(&h.kind, Kind::Ident(i) if i == "fabricate_for_tests") && h.in_test),
        "the scan did not classify the cfg(test-util) fabricators as test code"
    );
    assert!(
        seen(&|h| matches!(&h.kind, Kind::StructLit(_)) && h.module == ENVELOPE_MODULE),
        "the scan did not see the `ExecutionGrants` literal in {ENVELOPE_MODULE}"
    );
    assert!(
        seen(&|h| matches!(&h.kind, Kind::ImplOp(_)) && h.in_test),
        "the scan did not see the test-module `impl AuthorizedOp` blocks"
    );
    for owner in workspace_policy().sql_owners {
        assert!(
            seen(&|h| matches!(&h.kind, Kind::Sql { .. }) && h.file == owner),
            "the scan saw no audited-table SQL in {owner}; the SQL rule would pass vacuously"
        );
    }
}

#[test]
fn every_rpc_method_is_classified_or_on_the_shrinking_legacy_list() {
    let root = repo_root();
    let path = root.join("crates/zeroclaw-runtime/src/rpc/dispatch.rs");
    let src = fs::read_to_string(&path).expect("read dispatch.rs");
    let ast = syn::parse_file(&src).expect("dispatch.rs parses");
    let variants: BTreeSet<String> = ast
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(e) if e.ident == "Method" => {
                Some(e.variants.iter().map(|v| v.ident.to_string()).collect())
            }
            _ => None,
        })
        .expect("dispatch.rs defines `enum Method`");

    let legacy: BTreeSet<&str> = LEGACY_UNCLASSIFIED_METHODS.iter().copied().collect();
    let classified: BTreeSet<&str> = CLASSIFIED_METHODS.iter().map(|(v, _)| *v).collect();

    assert_eq!(
        legacy.len(),
        LEGACY_UNCLASSIFIED_METHODS.len(),
        "LEGACY_UNCLASSIFIED_METHODS lists a variant twice"
    );
    assert_eq!(
        LEGACY_UNCLASSIFIED_METHODS.len(),
        LEGACY_METHOD_CEILING,
        "the legacy list only shrinks: lower LEGACY_METHOD_CEILING with every removal, never raise it"
    );
    let both: Vec<_> = legacy.intersection(&classified).collect();
    assert!(
        both.is_empty(),
        "variants both legacy and classified: {both:?}"
    );

    let unclassified: Vec<_> = variants
        .iter()
        .filter(|v| !legacy.contains(v.as_str()) && !classified.contains(v.as_str()))
        .collect();
    assert!(
        unclassified.is_empty(),
        "new RPC methods must be classified in CLASSIFIED_METHODS (operation, read_only, or exempt), \
         not added to the legacy list: {unclassified:?}"
    );
    let stale: Vec<_> = legacy
        .union(&classified)
        .filter(|v| !variants.contains(**v))
        .collect();
    assert!(
        stale.is_empty(),
        "entries for variants that no longer exist: {stale:?}"
    );
    for (variant, class) in CLASSIFIED_METHODS {
        assert!(
            class.starts_with("op:") || *class == "read_only" || class.starts_with("exempt:"),
            "{variant}: classification `{class}` must be `op:<Op>`, `read_only`, or `exempt:<reason>`"
        );
    }
}

// ─── Tests: the ratchet catches each evasion ───────────────────────────────

fn snippet_policy() -> Policy {
    let mut policy = workspace_policy();
    policy.sinks = vec![Sink {
        capability: "RawSessionWrite",
        owners: &["zeroclaw_infra::session_sqlite"],
        sites: &["zeroclaw_runtime::security::commit::session"],
    }];
    policy.ops = vec![RegisteredOp {
        name: "SessionAppend",
        site: "zeroclaw_runtime::security::commit::session",
    }];
    policy
}

/// Violations for one synthetic file placed at `module` (non-test unless the
/// module is under a test path).
fn check(file: &str, module: &str, src: &str) -> Vec<String> {
    let policy = snippet_policy();
    let watch = Watch::of(&policy);
    let (_, declared) = scan_file(file, module, false, src, &watch, &BTreeSet::new());
    let tests: BTreeSet<String> = declared.into_iter().collect();
    let (mut all, _) = scan_file(file, module, false, src, &watch, &tests);
    // The registered op must have an impl, or C10 reports it as stale.
    all.push(Hit {
        kind: Kind::ImplOp("SessionAppend".into()),
        file: "fixture".into(),
        line: 0,
        module: "zeroclaw_runtime::security::commit::session".into(),
        in_test: false,
        in_commit_closure: false,
    });
    violations(&all, &policy)
}

fn assert_caught(found: &[String], rule: &str) {
    assert!(
        found.iter().any(|v| v.starts_with(rule)),
        "expected a {rule} violation, got: {found:#?}"
    );
}

#[test]
fn a_handler_naming_a_raw_sink_is_caught() {
    let found = check(
        "crates/zeroclaw-runtime/src/rpc/handler.rs",
        "zeroclaw_runtime::rpc::handler",
        "use zeroclaw_infra::RawSessionWrite;\nfn h(s: &dyn RawSessionWrite) { s.append_raw(1); }",
    );
    assert_caught(&found, "C1");
}

#[test]
fn a_raw_sink_used_before_or_outside_commit_is_caught() {
    let outside = check(
        "x.rs",
        "zeroclaw_runtime::security::commit::session",
        "fn c(effect: E, w: &W) { let raw: &dyn RawSessionWrite = w.raw(); effect.commit(inb, |op, _, _| ()); }",
    );
    assert_caught(&outside, "C6");

    let inside = check(
        "x.rs",
        "zeroclaw_runtime::security::commit::session",
        "fn c(effect: E, w: &W) { effect.commit(inb, |op, _, _| { let r: &dyn RawSessionWrite = w.raw(); r.put(op) }); }",
    );
    assert!(
        !inside
            .iter()
            .any(|v| v.starts_with("C6") || v.starts_with("C1")),
        "a sink inside the commit closure is the permitted shape: {inside:#?}"
    );
}

#[test]
fn a_raw_sink_leaked_through_a_field_or_accessor_is_caught() {
    let field = check(
        "crates/zeroclaw-infra/src/session_sqlite.rs",
        "zeroclaw_infra::session_sqlite",
        "pub struct Store { pub backend: std::sync::Arc<dyn RawSessionWrite> }",
    );
    assert_caught(&field, "C3");

    let accessor = check(
        "crates/zeroclaw-infra/src/session_sqlite.rs",
        "zeroclaw_infra::session_sqlite",
        "impl Store { pub fn raw(&self) -> &dyn RawSessionWrite { &self.backend } }",
    );
    assert_caught(&accessor, "C4");
}

#[test]
fn a_new_handler_writing_an_audited_table_is_caught() {
    let found = check(
        "crates/zeroclaw-runtime/src/rpc/handler.rs",
        "zeroclaw_runtime::rpc::handler",
        "fn h(c: &Conn) { c.execute(\"DELETE\n FROM sessions WHERE agent = ?1\", [a]); }",
    );
    assert_caught(&found, "C5");

    let in_macro = check(
        "crates/zeroclaw-runtime/src/rpc/handler.rs",
        "zeroclaw_runtime::rpc::handler",
        "fn h() { sqlx::query!(\"INSERT OR REPLACE INTO cron_jobs (id) VALUES (?)\"); }",
    );
    assert_caught(&in_macro, "C5");

    let in_doc = check(
        "crates/zeroclaw-runtime/src/rpc/handler.rs",
        "zeroclaw_runtime::rpc::handler",
        "/// Unlike `DELETE FROM sessions`, this only reads.\nfn h() {}",
    );
    assert!(
        !in_doc.iter().any(|v| v.starts_with("C5")),
        "doc text is not SQL: {in_doc:#?}"
    );

    let in_test = check(
        "crates/zeroclaw-runtime/src/rpc/handler.rs",
        "zeroclaw_runtime::rpc::handler",
        "#[cfg(test)] mod tests { fn t(c: &Conn) { c.execute(\"UPDATE sessions SET x = 1\", []); } }",
    );
    assert!(
        !in_test.iter().any(|v| v.starts_with("C5")),
        "test fixtures may write: {in_test:#?}"
    );
}

#[test]
fn a_proof_minted_outside_its_wait_is_caught() {
    let found = check(
        "crates/zeroclaw-runtime/src/rpc/handler.rs",
        "zeroclaw_runtime::rpc::handler",
        "fn h() { let p = SchedulerClaim::after_claim(); }",
    );
    assert_caught(&found, "C1");

    let at_wait = check(
        "crates/zeroclaw-runtime/src/cron/scheduler.rs",
        "zeroclaw_runtime::cron::scheduler",
        "fn tick() { let p = SchedulerClaim::after_claim(); }",
    );
    assert!(!at_wait.iter().any(|v| v.starts_with("C1")), "{at_wait:#?}");

    let via_macro = check(
        "crates/zeroclaw-runtime/src/rpc/handler.rs",
        "zeroclaw_runtime::rpc::handler",
        "fn h() { let v = vec![DecisionSettled::after_decision_wait()]; }",
    );
    assert_caught(&via_macro, "C1");

    let fabricated = check(
        "crates/zeroclaw-runtime/src/rpc/handler.rs",
        "zeroclaw_runtime::rpc::handler",
        "fn h() { let p = DeliverySlot::fabricate_for_tests(); }",
    );
    assert_caught(&fabricated, "C1");

    let fabricated_in_test = check(
        "crates/zeroclaw-runtime/src/rpc/handler.rs",
        "zeroclaw_runtime::rpc::handler",
        "#[cfg(test)] mod tests { fn t() { let p = DeliverySlot::fabricate_for_tests(); } }",
    );
    assert!(
        !fabricated_in_test.iter().any(|v| v.starts_with("C1")),
        "{fabricated_in_test:#?}"
    );

    let not_test = check(
        "crates/zeroclaw-runtime/src/rpc/handler.rs",
        "zeroclaw_runtime::rpc::handler",
        "#[cfg(not(test))] mod live { fn t() { let p = DeliverySlot::fabricate_for_tests(); } }",
    );
    assert_caught(&not_test, "C1");
}

#[test]
fn grants_built_outside_the_envelope_are_caught() {
    let found = check(
        "crates/zeroclaw-runtime/src/cron/scheduler.rs",
        "zeroclaw_runtime::cron::scheduler",
        "fn run() { let g = ExecutionGrants { grants: resolved, delegation: d }; }",
    );
    assert_caught(&found, "C7");

    let internal = check(
        "crates/zeroclaw-runtime/src/cron/scheduler.rs",
        "zeroclaw_runtime::cron::scheduler",
        "fn run() { let e = PrincipalEnvelope::trusted_internal(\"cron\", ceiling); }",
    );
    assert_caught(&internal, "C1");
}

#[test]
fn a_hand_written_site_test_name_is_caught() {
    let found = check(
        "crates/zeroclaw-runtime/src/rpc/tests.rs",
        "zeroclaw_runtime::rpc::tests",
        "#[test] fn session_append_revoked_after_admission_has_no_effect() {}",
    );
    assert_caught(&found, "C11");
}

#[test]
fn an_unregistered_operation_is_caught() {
    let found = check(
        "crates/zeroclaw-runtime/src/rpc/handler.rs",
        "zeroclaw_runtime::rpc::handler",
        "impl AuthorizedOp for ChannelBind { const NAME: &'static str = \"channel_bind\"; }",
    );
    assert_caught(&found, "C10");

    let misplaced = check(
        "crates/zeroclaw-runtime/src/rpc/handler.rs",
        "zeroclaw_runtime::rpc::handler",
        "impl AuthorizedOp for SessionAppend {}",
    );
    assert_caught(&misplaced, "C10");

    let test_op = check(
        "crates/zeroclaw-runtime/src/rpc/handler.rs",
        "zeroclaw_runtime::rpc::handler",
        "#[cfg(test)] mod tests { impl AuthorizedOp for Probe {} }",
    );
    assert!(
        !test_op.iter().any(|v| v.contains("Probe")),
        "test ops are exempt: {test_op:#?}"
    );
}

#[test]
fn sql_matching_is_statement_shaped() {
    let tables = ["sessions", "cron_jobs"];
    assert_eq!(
        sql_writes("update sessions set a = 1", &tables),
        ["sessions"]
    );
    assert_eq!(
        sql_writes("INSERT OR IGNORE INTO cron_jobs(id)", &tables),
        ["cron_jobs"]
    );
    assert!(sql_writes("SELECT * FROM sessions", &tables).is_empty());
    assert!(sql_writes("update sessions_archive set a = 1", &tables).is_empty());
    assert!(sql_writes("DELETE FROM session_metadata", &tables).is_empty());
}
