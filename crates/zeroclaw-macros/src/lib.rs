use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    Data, DeriveInput, Fields, GenericArgument, Lit, Meta, PathArguments, parse_macro_input,
};

// ── Field classification ──────────────────────────────────────────

/// Base type of a config field (Option-ness tracked separately).
#[derive(Debug, Clone)]
enum FieldKind {
    String,
    Bool,
    /// Concrete integer type ident (u8–u64, i8–i64, usize, isize).
    Integer(syn::Ident),
    Float,
    /// An enum (or unknown struct) that implements Serialize + Deserialize.
    Enum(TokenStream2),
    /// Vec, HashMap, PathBuf, or other compound types we skip.
    Skip,
}

/// Classify a `syn::Type` into a base `FieldKind`.
/// Option<T> is unwrapped — use `is_option_type()` to check wrapping.
fn classify_field_type(ty: &syn::Type) -> FieldKind {
    let syn::Type::Path(type_path) = ty else {
        return FieldKind::Skip;
    };
    let Some(segment) = type_path.path.segments.last() else {
        return FieldKind::Skip;
    };
    let ident_str = segment.ident.to_string();
    match ident_str.as_str() {
        "String" => FieldKind::String,
        "bool" => FieldKind::Bool,
        "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize" => {
            FieldKind::Integer(segment.ident.clone())
        }
        "f64" => FieldKind::Float,
        "Vec" | "HashMap" | "PathBuf" => FieldKind::Skip,
        "Option" => {
            let Some(inner) = extract_option_inner(ty) else {
                return FieldKind::Skip;
            };
            classify_field_type(inner)
        }
        _ => {
            let tokens = quote! { #ty };
            FieldKind::Enum(tokens)
        }
    }
}

/// Check if any `#[serde(...)]` attribute on the field contains `skip`.
fn has_serde_skip(field: &syn::Field) -> bool {
    for attr in &field.attrs {
        if attr.path().is_ident("serde") {
            // Parse the token stream inside the parens and look for `skip`
            if let Ok(nested) = attr.parse_args_with(
                syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
            ) {
                for meta in &nested {
                    if meta.path().is_ident("skip") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Return the type hint string for display purposes.
fn type_hint_str(kind: &FieldKind, is_option: bool) -> String {
    let inner = match kind {
        FieldKind::String => "String".to_string(),
        FieldKind::Bool => "bool".to_string(),
        FieldKind::Integer(ident) => ident.to_string(),
        FieldKind::Float => "f64".to_string(),
        FieldKind::Enum(tokens) => tokens.to_string().replace(' ', ""),
        FieldKind::Skip => "skip".to_string(),
    };
    if is_option {
        format!("Option<{inner}>")
    } else {
        inner
    }
}

/// Derive macro that generates secret and property methods for config structs.
///
/// # Attributes
///
/// - `#[secret]` on a `String` or `Option<String>` field marks it as a secret.
/// - `#[nested]` on a nested struct or `Option<StructWithSecrets>` field
///   delegates secret discovery and setting to the child.
/// - `#[prefix = "channels.matrix"]` on the struct sets the dotted path prefix.
///
/// # Generated methods
///
/// ## Secret methods (unchanged)
/// - `secret_fields(&self) -> Vec<SecretFieldInfo>`
/// - `set_secret(&mut self, name: &str, value: String) -> Result<()>`
/// - `encrypt_secrets(&mut self, store: &SecretStore) -> Result<()>`
/// - `decrypt_secrets(&mut self, store: &SecretStore) -> Result<()>`
///
/// ## Property methods (new)
/// - `prop_fields(&self) -> Vec<PropFieldInfo>` — enumerate all fields
/// - `get_prop(&self, name: &str) -> Result<String>` — get current value as string
/// - `set_prop(&mut self, name: &str, value_str: &str) -> Result<()>` — parse string and set
/// - `prop_is_secret(name: &str) -> bool` — static check
/// - `init_defaults(&mut self, prefix: Option<&str>) -> Vec<&'static str>` — instantiate None nested sections
#[proc_macro_derive(Configurable, attributes(secret, nested, prefix, serde))]
pub fn derive_configurable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;

    let prefix = extract_prefix(&input);
    let category = derive_category(&prefix);

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => panic!("Configurable only supports structs with named fields"),
        },
        _ => panic!("Configurable can only be derived for structs"),
    };

    // ── Secret codegen accumulators (unchanged) ──
    let mut secret_field_entries = Vec::new();
    let mut set_arms = Vec::new();
    let mut encrypt_ops = Vec::new();
    let mut decrypt_ops = Vec::new();
    let mut nested_collect = Vec::new();
    let mut nested_set = Vec::new();
    let mut nested_encrypt = Vec::new();
    let mut nested_decrypt = Vec::new();

    // ── Property codegen accumulators (new) ──
    let mut prop_field_entries = Vec::new();
    let mut get_prop_arms = Vec::new();
    let mut set_prop_arms = Vec::new();
    let mut prop_is_secret_arms = Vec::new();
    let mut nested_prop_fields = Vec::new();
    let mut nested_get_prop = Vec::new();
    let mut nested_set_prop = Vec::new();
    let mut nested_prop_is_secret = Vec::new();
    let mut init_defaults_ops = Vec::new();

    for field in fields {
        let field_ident = field.ident.as_ref().expect("Named field must have ident");
        let is_secret = has_attr(field, "secret");
        let is_nested = has_attr(field, "nested");
        let serde_skip = has_serde_skip(field);

        // ── Secret handling ──
        if is_secret {
            let field_name_kebab = snake_to_kebab(&field_ident.to_string());
            let full_name = if prefix.is_empty() {
                field_name_kebab.clone()
            } else {
                format!("{}.{}", prefix, field_name_kebab)
            };

            let is_option = is_option_type(&field.ty);
            let is_vec_string = extract_vec_inner(&field.ty)
                .map(|inner| matches!(classify_field_type(inner), FieldKind::String))
                .unwrap_or(false);
            let full_name_lit = &full_name;
            let category_lit = &category;

            if is_vec_string {
                // Vec<String> with #[secret]: iterate elements for encrypt/decrypt
                secret_field_entries.push(quote! {
                    crate::config::SecretFieldInfo {
                        name: #full_name_lit,
                        category: #category_lit,
                        is_set: !self.#field_ident.is_empty(),
                    }
                });
                encrypt_ops.push(quote! {
                    for element in &mut self.#field_ident {
                        if !element.is_empty() && !crate::security::SecretStore::is_encrypted(element) {
                            *element = store.encrypt(element)
                                .with_context(|| format!("Failed to encrypt {}[]", #full_name_lit))?;
                        }
                    }
                });
                decrypt_ops.push(quote! {
                    for element in &mut self.#field_ident {
                        if crate::security::SecretStore::is_encrypted(element) {
                            *element = store.decrypt(element)
                                .with_context(|| format!("Failed to decrypt {}[]", #full_name_lit))?;
                        }
                    }
                });
            } else if is_option {
                secret_field_entries.push(quote! {
                    crate::config::SecretFieldInfo {
                        name: #full_name_lit,
                        category: #category_lit,
                        is_set: self.#field_ident.as_ref().is_some_and(|v| !v.is_empty()),
                    }
                });
                set_arms.push(quote! {
                    #full_name_lit => { self.#field_ident = Some(value); Ok(()) }
                });
                encrypt_ops.push(quote! {
                    if let Some(raw) = &self.#field_ident {
                        if !crate::security::SecretStore::is_encrypted(raw) {
                            self.#field_ident = Some(
                                store.encrypt(raw)
                                    .with_context(|| format!("Failed to encrypt {}", #full_name_lit))?
                            );
                        }
                    }
                });
                decrypt_ops.push(quote! {
                    if let Some(raw) = &self.#field_ident {
                        if crate::security::SecretStore::is_encrypted(raw) {
                            self.#field_ident = Some(
                                store.decrypt(raw)
                                    .with_context(|| format!("Failed to decrypt {}", #full_name_lit))?
                            );
                        }
                    }
                });
            } else {
                secret_field_entries.push(quote! {
                    crate::config::SecretFieldInfo {
                        name: #full_name_lit,
                        category: #category_lit,
                        is_set: !self.#field_ident.is_empty(),
                    }
                });
                set_arms.push(quote! {
                    #full_name_lit => { self.#field_ident = value; Ok(()) }
                });
                encrypt_ops.push(quote! {
                    if !self.#field_ident.is_empty() && !crate::security::SecretStore::is_encrypted(&self.#field_ident) {
                        self.#field_ident = store.encrypt(&self.#field_ident)
                            .with_context(|| format!("Failed to encrypt {}", #full_name_lit))?;
                    }
                });
                decrypt_ops.push(quote! {
                    if crate::security::SecretStore::is_encrypted(&self.#field_ident) {
                        self.#field_ident = store.decrypt(&self.#field_ident)
                            .with_context(|| format!("Failed to decrypt {}", #full_name_lit))?;
                    }
                });
            }
        }

        if is_nested {
            // ── Nested delegation ──
            let is_option = is_option_type(&field.ty);
            let hashmap_value_ty = extract_hashmap_value_type(&field.ty);

            if let Some(value_ty) = hashmap_value_ty {
                // HashMap<String, T> with #[nested]: iterate values for secret ops
                nested_collect.push(quote! {
                    for inner in self.#field_ident.values() {
                        fields.extend(inner.secret_fields());
                    }
                });
                nested_set.push(quote! {
                    for inner in self.#field_ident.values_mut() {
                        if let Ok(()) = inner.set_secret(name, value.clone()) {
                            return Ok(());
                        }
                    }
                });
                nested_encrypt.push(quote! {
                    for inner in self.#field_ident.values_mut() {
                        inner.encrypt_secrets(store)?;
                    }
                });
                nested_decrypt.push(quote! {
                    for inner in self.#field_ident.values_mut() {
                        inner.decrypt_secrets(store)?;
                    }
                });
                nested_prop_is_secret.push(quote! {
                    if <#value_ty>::prop_is_secret(name) { return true; }
                });

                continue;
            } else if is_option {
                nested_collect.push(quote! {
                    if let Some(inner) = &self.#field_ident {
                        fields.extend(inner.secret_fields());
                    }
                });
                nested_set.push(quote! {
                    if let Some(inner) = &mut self.#field_ident {
                        if let Ok(()) = inner.set_secret(name, value.clone()) {
                            return Ok(());
                        }
                    }
                });
                nested_encrypt.push(quote! {
                    if let Some(inner) = &mut self.#field_ident {
                        inner.encrypt_secrets(store)?;
                    }
                });
                nested_decrypt.push(quote! {
                    if let Some(inner) = &mut self.#field_ident {
                        inner.decrypt_secrets(store)?;
                    }
                });

                // ── Nested property delegation (Option<T>) ──
                nested_prop_fields.push(quote! {
                    if let Some(inner) = &self.#field_ident {
                        fields.extend(inner.prop_fields());
                    }
                });
                nested_get_prop.push(quote! {
                    if let Some(inner) = &self.#field_ident {
                        if let Ok(val) = inner.get_prop(name) {
                            return Ok(val);
                        }
                    }
                });
                nested_set_prop.push(quote! {
                    if let Some(inner) = &mut self.#field_ident {
                        if let Ok(()) = inner.set_prop(name, value_str) {
                            return Ok(());
                        }
                    }
                });
                nested_prop_is_secret.push(quote! {
                    // Extract inner type from Option for static dispatch
                    // We need to know the inner type at compile time
                });

                // For Option<T> nested, extract inner type for Default::default()
                if let Some(inner_ty) = extract_option_inner(&field.ty) {
                    let inner_ty_tokens = quote! { #inner_ty };
                    init_defaults_ops.push(quote! {
                        if self.#field_ident.is_none() {
                            let child_prefix = <#inner_ty_tokens>::configurable_prefix();
                            let dominated = prefix.map_or(true, |p| {
                                child_prefix.starts_with(p) || p.starts_with(child_prefix)
                            });
                            if dominated {
                                let mut probe = <#inner_ty_tokens as Default>::default();
                                let child_results = probe.init_defaults(prefix);
                                initialized.push(child_prefix);
                                initialized.extend(child_results);
                                self.#field_ident = Some(probe);
                            }
                        } else if let Some(inner) = &mut self.#field_ident {
                            initialized.extend(inner.init_defaults(prefix));
                        }
                    });

                    // For prop_is_secret delegation on Option<T> nested, we need the inner type
                    nested_prop_is_secret.pop(); // Remove the placeholder
                    nested_prop_is_secret.push(quote! {
                        if <#inner_ty_tokens>::prop_is_secret(name) {
                            return true;
                        }
                    });
                }
            } else {
                nested_collect.push(quote! {
                    fields.extend(self.#field_ident.secret_fields());
                });
                nested_set.push(quote! {
                    if let Ok(()) = self.#field_ident.set_secret(name, value.clone()) {
                        return Ok(());
                    }
                });
                nested_encrypt.push(quote! {
                    self.#field_ident.encrypt_secrets(store)?;
                });
                nested_decrypt.push(quote! {
                    self.#field_ident.decrypt_secrets(store)?;
                });

                // ── Nested property delegation (non-Option) ──
                nested_prop_fields.push(quote! {
                    fields.extend(self.#field_ident.prop_fields());
                });
                nested_get_prop.push(quote! {
                    if let Ok(val) = self.#field_ident.get_prop(name) {
                        return Ok(val);
                    }
                });
                nested_set_prop.push(quote! {
                    if let Ok(()) = self.#field_ident.set_prop(name, value_str) {
                        return Ok(());
                    }
                });

                // Get the field type for static method dispatch
                let field_ty = &field.ty;
                nested_prop_is_secret.push(quote! {
                    if <#field_ty>::prop_is_secret(name) {
                        return true;
                    }
                });

                // init_defaults for non-Option nested: delegate
                init_defaults_ops.push(quote! {
                    initialized.extend(self.#field_ident.init_defaults(prefix));
                });
            }

            continue; // nested fields handled above
        }

        // ── Property handling for non-nested, non-skip fields ──
        if serde_skip {
            continue;
        }

        let kind = classify_field_type(&field.ty);
        if matches!(kind, FieldKind::Skip) {
            continue;
        }

        let field_name_kebab = snake_to_kebab(&field_ident.to_string());
        let full_name = if prefix.is_empty() {
            field_name_kebab.clone()
        } else {
            format!("{}.{}", prefix, field_name_kebab)
        };
        let full_name_lit = &full_name;
        let category_lit = &category;
        let is_option = is_option_type(&field.ty);
        let type_hint = type_hint_str(&kind, is_option);
        let type_hint_lit = &type_hint;
        let is_enum = matches!(kind, FieldKind::Enum(_));

        // prop_is_secret arm
        if is_secret {
            prop_is_secret_arms.push(quote! {
                #full_name_lit => true,
            });
        }

        // prop_fields entry
        let display_value_expr = if is_secret {
            if is_option_type(&field.ty) {
                quote! {
                    if self.#field_ident.as_ref().is_some_and(|v| !v.is_empty()) {
                        "****".to_string()
                    } else {
                        "<unset>".to_string()
                    }
                }
            } else {
                quote! {
                    if self.#field_ident.is_empty() {
                        "<unset>".to_string()
                    } else {
                        "****".to_string()
                    }
                }
            }
        } else {
            gen_value_to_string(field_ident, &kind, is_option)
        };

        let enum_variants_expr = if let FieldKind::Enum(ref ty_tokens) = kind {
            quote! {
                Some(|| {
                    crate::config::enum_variants::<#ty_tokens>()
                        .split(", ")
                        .map(|s| s.to_string())
                        .collect()
                })
            }
        } else {
            quote! { None }
        };

        prop_field_entries.push(quote! {
            {
                let display_value = { #display_value_expr };
                crate::config::PropFieldInfo {
                    name: #full_name_lit,
                    category: #category_lit,
                    display_value,
                    type_hint: #type_hint_lit,
                    is_secret: #is_secret,
                    is_enum: #is_enum,
                    enum_variants: #enum_variants_expr,
                }
            }
        });

        // get_prop arm
        let get_expr = if is_secret {
            quote! { Ok("**** (encrypted)".to_string()) }
        } else {
            let val = gen_value_to_string(field_ident, &kind, is_option);
            quote! { Ok(#val) }
        };
        get_prop_arms.push(quote! {
            #full_name_lit => { #get_expr }
        });

        // set_prop arm
        let set_expr = gen_set_prop_expr(field_ident, &kind, is_option);
        set_prop_arms.push(quote! {
            #full_name_lit => { #set_expr }
        });
    }

    let prefix_lit = &prefix;

    let expanded = quote! {
        impl #struct_name {
            /// Returns the `#[prefix]` value for this Configurable struct.
            pub fn configurable_prefix() -> &'static str {
                #prefix_lit
            }

            /// Returns metadata about all `#[secret]` fields on this struct and nested children.
            pub fn secret_fields(&self) -> Vec<crate::config::SecretFieldInfo> {
                let mut fields = vec![#(#secret_field_entries),*];
                #(#nested_collect)*
                fields
            }

            /// Encrypt all secret fields in place using the provided store.
            pub fn encrypt_secrets(&mut self, store: &crate::security::SecretStore) -> anyhow::Result<()> {
                use anyhow::Context;
                #(#encrypt_ops)*
                #(#nested_encrypt)*
                Ok(())
            }

            /// Decrypt all secret fields in place using the provided store.
            pub fn decrypt_secrets(&mut self, store: &crate::security::SecretStore) -> anyhow::Result<()> {
                use anyhow::Context;
                #(#decrypt_ops)*
                #(#nested_decrypt)*
                Ok(())
            }

            /// Set a secret field by its full dotted name, dispatching to nested children.
            pub fn set_secret(&mut self, name: &str, value: String) -> anyhow::Result<()> {
                // Try direct secret fields first
                match name {
                    #(#set_arms,)*
                    _ => {
                        // Try nested children
                        #(#nested_set)*
                        anyhow::bail!("Unknown secret '{}'", name)
                    }
                }
            }

            /// Returns metadata about all property fields on this struct and nested children.
            pub fn prop_fields(&self) -> Vec<crate::config::PropFieldInfo> {
                let mut fields = vec![#(#prop_field_entries),*];
                #(#nested_prop_fields)*
                fields
            }

            /// Get a property value by its full dotted name, returning it as a display string.
            pub fn get_prop(&self, name: &str) -> anyhow::Result<String> {
                match name {
                    #(#get_prop_arms,)*
                    _ => {
                        #(#nested_get_prop)*
                        anyhow::bail!("Unknown property '{}'", name)
                    }
                }
            }

            /// Set a property value by its full dotted name, parsing from string.
            pub fn set_prop(&mut self, name: &str, value_str: &str) -> anyhow::Result<()> {
                match name {
                    #(#set_prop_arms,)*
                    _ => {
                        #(#nested_set_prop)*
                        anyhow::bail!("Unknown property '{}'", name)
                    }
                }
            }

            /// Check if a property name refers to a secret field (static, no instance needed).
            pub fn prop_is_secret(name: &str) -> bool {
                match name {
                    #(#prop_is_secret_arms)*
                    _ => {
                        #(#nested_prop_is_secret)*
                        false
                    }
                }
            }

            /// Instantiate `None` nested sections whose prefix matches.
            /// Returns the prefixes that were initialized.
            pub fn init_defaults(&mut self, prefix: Option<&str>) -> Vec<&'static str> {
                let mut initialized: Vec<&'static str> = Vec::new();
                #(#init_defaults_ops)*
                initialized
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generate expression to convert a field value to a display string.
/// Used by both `prop_fields` (display_value) and `get_prop`.
fn gen_value_to_string(
    field_ident: &syn::Ident,
    kind: &FieldKind,
    is_option: bool,
) -> TokenStream2 {
    // Inner conversion: T → String
    let inner_expr = match kind {
        FieldKind::String => quote! { self.#field_ident.clone() },
        FieldKind::Bool | FieldKind::Integer(_) | FieldKind::Float => {
            quote! { self.#field_ident.to_string() }
        }
        FieldKind::Enum(_) => quote! {
            serde_json::to_string(&self.#field_ident).unwrap_or_default().trim_matches('"').to_string()
        },
        FieldKind::Skip => unreachable!(),
    };

    if !is_option {
        return inner_expr;
    }

    // Option<T> → unwrap or "<unset>"
    match kind {
        FieldKind::String => quote! {
            self.#field_ident.clone().unwrap_or_else(|| "<unset>".to_string())
        },
        FieldKind::Enum(_) => quote! {
            match &self.#field_ident {
                Some(v) => serde_json::to_string(v).unwrap_or_default().trim_matches('"').to_string(),
                None => "<unset>".to_string(),
            }
        },
        _ => quote! {
            self.#field_ident.map_or_else(|| "<unset>".to_string(), |v| v.to_string())
        },
    }
}

/// Generate the set_prop expression for a field.
fn gen_set_prop_expr(field_ident: &syn::Ident, kind: &FieldKind, is_option: bool) -> TokenStream2 {
    // Inner parse: &str → T (or error)
    let parse_expr = match kind {
        FieldKind::String => quote! { value_str.to_string() },
        FieldKind::Bool => quote! {
            value_str.parse::<bool>()
                .map_err(|_| anyhow::anyhow!("Invalid bool value '{}' — expected 'true' or 'false'", value_str))?
        },
        FieldKind::Integer(ident) => quote! {
            value_str.parse::<#ident>()
                .map_err(|_| anyhow::anyhow!("Invalid {} value '{}'", stringify!(#ident), value_str))?
        },
        FieldKind::Float => quote! {
            value_str.parse::<f64>()
                .map_err(|_| anyhow::anyhow!("Invalid f64 value '{}'", value_str))?
        },
        FieldKind::Enum(ty_tokens) => quote! {
            serde_json::from_value::<#ty_tokens>(serde_json::Value::String(value_str.to_string()))
                .or_else(|_| serde_json::from_str::<#ty_tokens>(value_str))
                .map_err(|_| anyhow::anyhow!(
                    "Invalid value '{}' — expected one of: {}",
                    value_str,
                    crate::config::enum_variants::<#ty_tokens>()
                ))?
        },
        FieldKind::Skip => unreachable!(),
    };

    if !is_option {
        return quote! {
            self.#field_ident = #parse_expr;
            Ok(())
        };
    }

    // Option<T>: empty → None, otherwise Some(parse)
    quote! {
        if value_str.is_empty() {
            self.#field_ident = None;
        } else {
            self.#field_ident = Some(#parse_expr);
        }
        Ok(())
    }
}

fn derive_category(prefix: &str) -> String {
    if prefix.is_empty() {
        return "Core".to_string();
    }
    let first = prefix.split('.').next().unwrap_or("");
    match first {
        "channels" => "Channels".to_string(),
        "tts" => "TTS".to_string(),
        "transcription" => "Transcription".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
                None => "Core".to_string(),
            }
        }
    }
}

fn extract_prefix(input: &DeriveInput) -> String {
    for attr in &input.attrs {
        if !attr.path().is_ident("prefix") {
            continue;
        }
        let Meta::NameValue(nv) = &attr.meta else {
            continue;
        };
        let syn::Expr::Lit(expr_lit) = &nv.value else {
            continue;
        };
        let Lit::Str(lit_str) = &expr_lit.lit else {
            continue;
        };
        return lit_str.value();
    }
    String::new()
}

fn has_attr(field: &syn::Field, name: &str) -> bool {
    field.attrs.iter().any(|attr| attr.path().is_ident(name))
}

fn snake_to_kebab(s: &str) -> String {
    s.replace('_', "-")
}

fn is_option_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(type_path) = ty else {
        return false;
    };
    type_path
        .path
        .segments
        .last()
        .is_some_and(|s| s.ident == "Option")
}

/// Extract the Nth type argument from a generic type matching `expected_ident`.
/// e.g. `extract_type_arg("Option", 0, ty)` returns `T` from `Option<T>`.
fn extract_type_arg<'a>(
    expected_ident: &str,
    index: usize,
    ty: &'a syn::Type,
) -> Option<&'a syn::Type> {
    let syn::Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != expected_ident {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args
        .iter()
        .filter_map(|a| {
            if let GenericArgument::Type(t) = a {
                Some(t)
            } else {
                None
            }
        })
        .nth(index)
}

fn extract_option_inner(ty: &syn::Type) -> Option<&syn::Type> {
    extract_type_arg("Option", 0, ty)
}
fn extract_vec_inner(ty: &syn::Type) -> Option<&syn::Type> {
    extract_type_arg("Vec", 0, ty)
}
fn extract_hashmap_value_type(ty: &syn::Type) -> Option<&syn::Type> {
    extract_type_arg("HashMap", 1, ty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn snake_to_kebab_converts_underscores() {
        assert_eq!(snake_to_kebab("access_token"), "access-token");
        assert_eq!(snake_to_kebab("api_key"), "api-key");
        assert_eq!(snake_to_kebab("bot_token"), "bot-token");
        assert_eq!(snake_to_kebab("simple"), "simple");
    }

    #[test]
    fn derive_category_from_prefix() {
        assert_eq!(derive_category("channels.matrix"), "Channels");
        assert_eq!(derive_category("channels.discord"), "Channels");
        assert_eq!(derive_category("tts.openai"), "TTS");
        assert_eq!(derive_category("tts.elevenlabs"), "TTS");
        assert_eq!(derive_category("transcription"), "Transcription");
        assert_eq!(derive_category("transcription.openai"), "Transcription");
        assert_eq!(derive_category(""), "Core");
    }

    #[test]
    fn classify_string_types() {
        let ty: syn::Type = parse_quote! { String };
        assert!(matches!(classify_field_type(&ty), FieldKind::String));

        let ty: syn::Type = parse_quote! { Option<String> };
        assert!(matches!(classify_field_type(&ty), FieldKind::String));
        assert!(is_option_type(&ty));
    }

    #[test]
    fn classify_bool_types() {
        let ty: syn::Type = parse_quote! { bool };
        assert!(matches!(classify_field_type(&ty), FieldKind::Bool));

        let ty: syn::Type = parse_quote! { Option<bool> };
        assert!(matches!(classify_field_type(&ty), FieldKind::Bool));
        assert!(is_option_type(&ty));
    }

    #[test]
    fn classify_integer_types() {
        for type_name in &[
            "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize",
        ] {
            let ty: syn::Type = syn::parse_str(type_name).unwrap();
            match classify_field_type(&ty) {
                FieldKind::Integer(ident) => assert_eq!(ident.to_string(), *type_name),
                other => panic!("Expected Integer for {}, got {:?}", type_name, other),
            }
        }

        let ty: syn::Type = parse_quote! { Option<u64> };
        assert!(matches!(classify_field_type(&ty), FieldKind::Integer(_)));
        assert!(is_option_type(&ty));
    }

    #[test]
    fn classify_float_types() {
        let ty: syn::Type = parse_quote! { f64 };
        assert!(matches!(classify_field_type(&ty), FieldKind::Float));

        let ty: syn::Type = parse_quote! { Option<f64> };
        assert!(matches!(classify_field_type(&ty), FieldKind::Float));
        assert!(is_option_type(&ty));
    }

    #[test]
    fn classify_skip_types() {
        let ty: syn::Type = parse_quote! { Vec<String> };
        assert!(matches!(classify_field_type(&ty), FieldKind::Skip));

        let ty: syn::Type = parse_quote! { HashMap<String, String> };
        assert!(matches!(classify_field_type(&ty), FieldKind::Skip));

        let ty: syn::Type = parse_quote! { PathBuf };
        assert!(matches!(classify_field_type(&ty), FieldKind::Skip));
    }

    #[test]
    fn classify_enum_types() {
        let ty: syn::Type = parse_quote! { StreamMode };
        assert!(matches!(classify_field_type(&ty), FieldKind::Enum(_)));

        // Unknown struct/enum types go to Enum
        let ty: syn::Type = parse_quote! { SomeCustomType };
        assert!(matches!(classify_field_type(&ty), FieldKind::Enum(_)));
    }

    #[test]
    fn has_serde_skip_detects_skip() {
        let field: syn::Field = parse_quote! {
            #[serde(skip)]
            pub workspace_dir: String
        };
        assert!(has_serde_skip(&field));
    }

    #[test]
    fn has_serde_skip_ignores_other_serde_attrs() {
        let field: syn::Field = parse_quote! {
            #[serde(default)]
            pub enabled: bool
        };
        assert!(!has_serde_skip(&field));

        let field: syn::Field = parse_quote! {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub value: Option<String>
        };
        assert!(!has_serde_skip(&field));
    }

    #[test]
    fn has_serde_skip_no_serde_attr() {
        let field: syn::Field = parse_quote! {
            pub name: String
        };
        assert!(!has_serde_skip(&field));
    }

    #[test]
    fn has_serde_skip_with_other_attrs() {
        let field: syn::Field = parse_quote! {
            #[secret]
            #[serde(skip)]
            pub token: String
        };
        assert!(has_serde_skip(&field));
    }
}
