//! Architecture gate: every URL-valued field a config type declares is
//! classified for config reads. It is masked whole as a secret
//! (`#[secret]`), masked in the components that can carry a credential
//! (`#[credential_url]`, or a hand-written `CredentialUrlField` impl for a
//! value the derive cannot reach), or named below as carrying no credential.
//! A new URL field fails this gate until someone decides which, so config
//! reads cannot echo a credential nobody looked at.

use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};

/// Where the config types live.
const CONFIG_SRC: &str = "crates/zeroclaw-config/src";

/// A field named with one of these words holds a URL, or a list of them.
const URL_WORDS: &[&str] = &[
    "url",
    "urls",
    "uri",
    "uris",
    "endpoint",
    "endpoints",
    "proxy",
    "homeserver",
    "issuer",
    "origin",
    "origins",
    "relays",
];

/// URL fields masked by a hand-written `CredentialUrlField` impl rather than
/// the attribute: (struct, field, the impl's header).
const MASKED_BY_IMPL: &[(&str, &str, &str)] = &[
    (
        "EmailOAuth2Config",
        "token_url",
        "impl CredentialUrlField for EmailOAuth2Config",
    ),
    (
        "EmailOAuth2Config",
        "device_code_url",
        "impl CredentialUrlField for EmailOAuth2Config",
    ),
    (
        "ExternalRegistry",
        "url",
        "impl crate::traits::CredentialUrlField for Vec<ExternalRegistry>",
    ),
];

/// URL-named fields that carry no credential: (struct, field, why).
const NOT_CREDENTIAL_BEARING: &[(&str, &str, &str)] = &[
    (
        "ProxyConfig",
        "no_proxy",
        "host patterns that bypass the proxy, not URLs",
    ),
    (
        "CustomTunnelConfig",
        "url_pattern",
        "a regex that finds the public URL in the tunnel command's output",
    ),
    (
        "WebAuthnConfig",
        "rp_origin",
        "the relying party's public origin, which WebAuthn compares exactly",
    ),
];

/// A URL-named, string-typed field of a struct in the config sources.
#[derive(Debug)]
struct UrlField {
    path: String,
    owner: String,
    field: String,
    /// `#[credential_url]` or `#[secret]`.
    attributed: bool,
}

#[test]
fn every_config_url_field_is_classified() {
    let fields = scan();
    let unclassified: Vec<String> = fields
        .iter()
        .filter(|found| {
            !found.attributed
                && !listed(MASKED_BY_IMPL, found)
                && !listed(NOT_CREDENTIAL_BEARING, found)
        })
        .map(|found| format!("{}: {}.{}", found.path, found.owner, found.field))
        .collect();
    assert!(
        unclassified.is_empty(),
        "URL fields with no read classification. Mark each `#[credential_url]` (or \
         `#[secret]`), or list it in this gate with the reason it carries no \
         credential:\n{}",
        unclassified.join("\n")
    );
}

#[test]
fn every_listed_url_field_is_live_and_its_impl_exists() {
    let fields = scan();
    // The scan reads what it is meant to: the attributed provider URI is one.
    assert!(
        fields
            .iter()
            .any(|found| found.owner == "ModelProviderConfig" && found.field == "uri"),
        "the scan found {} fields and missed a known one",
        fields.len()
    );
    let sources = config_sources()
        .into_iter()
        .map(|path| fs::read_to_string(path).expect("config source reads"))
        .collect::<Vec<_>>()
        .join("\n");
    for (owner, field, impl_header) in MASKED_BY_IMPL {
        assert!(
            fields
                .iter()
                .any(|found| found.owner == *owner && found.field == *field && !found.attributed),
            "{owner}.{field} is listed as masked by an impl but is not an unattributed URL field"
        );
        assert!(
            sources.contains(impl_header),
            "{owner}.{field}: no `{impl_header}` in the config sources"
        );
    }
    for (owner, field, _) in NOT_CREDENTIAL_BEARING {
        assert!(
            fields
                .iter()
                .any(|found| found.owner == *owner && found.field == *field && !found.attributed),
            "{owner}.{field} is listed as carrying no credential but is not an unattributed URL field"
        );
    }
}

fn listed(table: &[(&str, &str, &str)], found: &UrlField) -> bool {
    table
        .iter()
        .any(|(owner, field, _)| found.owner == *owner && found.field == *field)
}

fn scan() -> Vec<UrlField> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    for path in config_sources() {
        let source = fs::read_to_string(&path).expect("config source reads");
        let file = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{} parses: {error}", path.display()));
        let mut visitor = Visitor {
            path: path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/"),
            found: &mut found,
        };
        visitor.visit_file(&file);
    }
    found
}

fn config_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let mut entries: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("{} lists: {error}", dir.display()))
            .map(|entry| entry.expect("directory entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(CONFIG_SRC),
        &mut out,
    );
    out
}

struct Visitor<'a> {
    path: String,
    found: &'a mut Vec<UrlField>,
}

impl<'ast> Visit<'ast> for Visitor<'_> {
    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        if let syn::Fields::Named(named) = &item.fields {
            for field in &named.named {
                let Some(ident) = &field.ident else {
                    continue;
                };
                let name = ident.to_string();
                if !name.split('_').any(|word| URL_WORDS.contains(&word)) || !is_text(&field.ty) {
                    continue;
                }
                let attributed = field.attrs.iter().any(|attr| {
                    attr.path().is_ident("credential_url") || attr.path().is_ident("secret")
                });
                self.found.push(UrlField {
                    path: self.path.clone(),
                    owner: item.ident.to_string(),
                    field: name,
                    attributed,
                });
            }
        }
        visit::visit_item_struct(self, item);
    }
}

/// `String`, or an `Option` or `Vec` of text.
fn is_text(ty: &syn::Type) -> bool {
    let syn::Type::Path(path) = ty else {
        return false;
    };
    let Some(last) = path.path.segments.last() else {
        return false;
    };
    match last.ident.to_string().as_str() {
        "String" => true,
        "Option" | "Vec" => match &last.arguments {
            syn::PathArguments::AngleBracketed(args) => args
                .args
                .iter()
                .any(|arg| matches!(arg, syn::GenericArgument::Type(inner) if is_text(inner))),
            _ => false,
        },
        _ => false,
    }
}
