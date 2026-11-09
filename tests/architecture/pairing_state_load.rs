//! Architecture gate: production code loads a gateway's persisted pairing
//! state only through `PairingGuard::from_gateway_config`.
//!
//! `gateway.paired_tokens` holds the shared-operator tokens and
//! `gateway.paired_token_users` holds the tokens bound to roster users. A
//! production site that built a guard with
//! `PairingGuard::new(.., &config.gateway.paired_tokens, ..)` would load the
//! first field without the second, so every roster-bound token would stop
//! authenticating after a restart. Any `.paired_tokens` read passed to
//! `PairingGuard::new` is flagged, so an alias such as `gw.paired_tokens` is
//! caught too. Tests may still build guards from literal token lists, so
//! items compiled only for tests (`cfg(test)`, or
//! `cfg(any(test, feature = "test-helpers"))`) are skipped; a
//! `cfg(not(test))` item is production code and is scanned. Calls written
//! inside a macro body are not parsed and so are not checked.

use std::fs;
use std::path::{Path, PathBuf};

use syn::spanned::Spanned;
use syn::visit::{self, Visit};

/// Every crate that builds a production `PairingGuard` today (the channels
/// build their own guards from literal lists), plus the root binary. A new
/// crate that builds one must be added here.
const SCAN_ROOTS: &[&str] = &[
    "src",
    "crates/zeroclaw-channels/src",
    "crates/zeroclaw-config/src",
    "crates/zeroclaw-gateway/src",
    "crates/zeroclaw-runtime/src",
];

#[test]
fn production_code_loads_pairing_state_through_from_gateway_config() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    for scan_root in SCAN_ROOTS {
        let mut files = Vec::new();
        collect_rust_files(&root.join(scan_root), &mut files);
        for file in files {
            let source = fs::read_to_string(&file)
                .unwrap_or_else(|error| panic!("could not read {}: {error}", file.display()));
            let syntax = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("could not parse {}: {error}", file.display()));
            let mut finder = GuardFromPairedTokens::default();
            finder.visit_file(&syntax);
            let relative = file
                .strip_prefix(root)
                .unwrap_or(&file)
                .display()
                .to_string();
            violations.extend(
                finder
                    .lines
                    .into_iter()
                    .map(|line| format!("{relative}:{line}")),
            );
        }
    }
    assert!(
        violations.is_empty(),
        "Production code builds a PairingGuard from gateway.paired_tokens with \
         PairingGuard::new, which skips the roster-bound tokens in \
         gateway.paired_token_users. Use PairingGuard::from_gateway_config.\n{violations:#?}"
    );
}

#[test]
fn detector_flags_a_production_call_and_skips_test_items() {
    let flagged = "fn boot(config: &Config) -> PairingGuard {
        PairingGuard::new(config.gateway.require_pairing, &config.gateway.paired_tokens, policy)
    }";
    let aliased = "fn boot(config: &Config) -> PairingGuard {
        let gw = &config.gateway;
        PairingGuard::new(gw.require_pairing, &gw.paired_tokens, gw.pairing_code)
    }";
    let skipped = "#[cfg(test)]
    mod tests {
        fn guard(config: &Config) -> PairingGuard {
            zeroclaw_config::pairing::PairingGuard::new(true, &config.gateway.paired_tokens, p)
        }
    }
    fn channel() -> PairingGuard { PairingGuard::new(true, &[], p) }
    #[cfg(any(test, feature = \"test-helpers\"))]
    fn helper(config: &Config) -> PairingGuard {
        PairingGuard::new(true, &config.gateway.paired_tokens, p)
    }";
    let production_only = "#[cfg(not(test))]
    fn boot(config: &Config) -> PairingGuard {
        PairingGuard::new(true, &config.gateway.paired_tokens, p)
    }
    #[cfg(all(unix, not(test)))]
    fn boot_unix(config: &Config) -> PairingGuard {
        PairingGuard::new(true, &config.gateway.paired_tokens, p)
    }";
    assert_eq!(hits(flagged), 1);
    assert_eq!(
        hits(aliased),
        1,
        "an alias of the gateway section is caught"
    );
    assert_eq!(hits(skipped), 0);
    assert_eq!(
        hits(production_only),
        2,
        "items compiled outside tests must stay in the scan"
    );
}

fn hits(source: &str) -> usize {
    let mut finder = GuardFromPairedTokens::default();
    finder.visit_file(&syn::parse_file(source).expect("fixture parses"));
    finder.lines.len()
}

#[derive(Default)]
struct GuardFromPairedTokens {
    lines: Vec<usize>,
}

impl<'ast> Visit<'ast> for GuardFromPairedTokens {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !is_test_only(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if !is_test_only(&item.attrs) {
            visit::visit_item_fn(self, item);
        }
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if !is_test_only(&item.attrs) {
            visit::visit_item_impl(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !is_test_only(&item.attrs) {
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if is_pairing_guard_new(&call.func)
            && call.args.iter().any(|arg| {
                let mut reads = ReadsPairedTokens(false);
                reads.visit_expr(arg);
                reads.0
            })
        {
            self.lines.push(call.span().start().line);
        }
        visit::visit_expr_call(self, call);
    }
}

/// Whether an expression reads a `paired_tokens` field. Only the gateway
/// section and its persisted form carry a token list that could be passed to
/// `PairingGuard::new`, and loading either that way would drop the
/// roster-bound tokens.
struct ReadsPairedTokens(bool);

impl<'ast> Visit<'ast> for ReadsPairedTokens {
    fn visit_expr_field(&mut self, field: &'ast syn::ExprField) {
        if member_is(&field.member, "paired_tokens") {
            self.0 = true;
        }
        visit::visit_expr_field(self, field);
    }
}

fn member_is(member: &syn::Member, name: &str) -> bool {
    matches!(member, syn::Member::Named(ident) if ident == name)
}

/// Whether an item is compiled only for tests: a `cfg` whose predicate can
/// hold only under `test` (or the test-support `test-helpers` feature). A
/// `cfg(not(test))` item is production code and stays in the scan.
fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .is_ok_and(|predicate| holds_only_under_test(&predicate))
    })
}

fn holds_only_under_test(predicate: &syn::Meta) -> bool {
    match predicate {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::NameValue(pair) => {
            pair.path.is_ident("feature")
                && matches!(&pair.value, syn::Expr::Lit(lit)
                    if matches!(&lit.lit, syn::Lit::Str(name) if name.value() == "test-helpers"))
        }
        syn::Meta::List(list) => {
            let Ok(children) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            ) else {
                return false;
            };
            if list.path.is_ident("all") {
                children.iter().any(holds_only_under_test)
            } else if list.path.is_ident("any") {
                !children.is_empty() && children.iter().all(holds_only_under_test)
            } else {
                // `not(..)` and anything unrecognized can hold in production.
                false
            }
        }
    }
}

fn is_pairing_guard_new(func: &syn::Expr) -> bool {
    let syn::Expr::Path(path) = func else {
        return false;
    };
    let segments: Vec<String> = path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    segments.ends_with(&["PairingGuard".to_string(), "new".to_string()])
}

fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", dir.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|error| panic!("could not read an entry of {}: {error}", dir.display()))
            .path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}
