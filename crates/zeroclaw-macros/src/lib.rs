use proc_macro::TokenStream;
use quote::{ToTokens, quote};
use syn::{
    Data, DeriveInput, Fields, GenericArgument, Lit, Meta, PathArguments, parse_macro_input,
};

/// Check if a type is a known compound container (Vec, HashMap, etc.)
/// that should be skipped from property enumeration.
fn is_compound_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(type_path) = ty else {
        return false;
    };
    let Some(ident) = type_path.path.segments.last().map(|s| &s.ident) else {
        return false;
    };
    ident == "Vec" || ident == "HashMap" || ident == "PathBuf"
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
/// ## Secret methods
/// - `secret_fields(&self) -> Vec<SecretFieldInfo>`
/// - `set_secret(&mut self, name: &str, value: String) -> Result<()>`
/// - `encrypt_secrets(&mut self, store: &SecretStore) -> Result<()>`
/// - `decrypt_secrets(&mut self, store: &SecretStore) -> Result<()>`
///
/// ## Property methods
/// - `prop_fields(&self) -> Vec<PropFieldInfo>` — enumerate all fields
/// - `get_prop(&self, name: &str) -> Result<String>` — get current value as string
/// - `set_prop(&mut self, name: &str, value_str: &str) -> Result<()>` — parse string and set
/// - `prop_is_secret(name: &str) -> bool` — static check
/// - `init_defaults(&mut self, prefix: Option<&str>) -> Vec<&'static str>` — instantiate None nested sections
///
/// # Adding a new config struct
///
/// 1. Derive `Configurable` and `Default`, set `#[prefix]`, add `enabled` if the
///    section is opt-in:
///
/// ```ignore
/// use zeroclaw_macros::Configurable;
///
/// #[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
/// #[prefix = "channels.your-channel"]
/// pub struct YourChannelConfig {
///     #[serde(default)]
///     pub enabled: bool,
///     #[secret]
///     pub bot_token: String,
///     #[secret]
///     pub webhook_secret: Option<String>,
///     pub room_id: String,
/// }
/// ```
///
/// 2. If the struct nests inside a parent (e.g. `ChannelsConfig`), add `#[nested]`
///    on the parent's field so the tree traversal finds it.
///
/// 3. Field names convert from `snake_case` to `kebab-case` for CLI use.
///    `bot_token` on a struct with `#[prefix = "channels.your-channel"]`
///    becomes `channels.your-channel.bot-token`.
///
/// ## Enum fields
///
/// Enum types used as fields must implement `HasPropKind`. Add the type to the
/// `impl_enum_prop_kind!` block in `crates/zeroclaw-config/src/schema.rs`, or
/// implement `HasPropKind` at the enum's definition site:
///
/// ```ignore
/// impl crate::config::HasPropKind for YourEnum {
///     const PROP_KIND: crate::config::PropKind = crate::config::PropKind::Enum;
/// }
/// ```
///
/// Live examples: see `ChannelsConfig`, `ProvidersConfig`, and `MemoryConfig`
/// in `crates/zeroclaw-config/src/schema.rs`.
#[proc_macro_derive(
    Configurable,
    attributes(secret, nested, prefix, serde, derived_from_secret)
)]
pub fn derive_configurable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;

    let prefix = extract_prefix(&input);
    let category = derive_category(&prefix);

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => {
                return syn::Error::new_spanned(
                    &input,
                    "Configurable only supports structs with named fields",
                )
                .to_compile_error()
                .into();
            }
        },
        _ => {
            return syn::Error::new_spanned(&input, "Configurable can only be derived for structs")
                .to_compile_error()
                .into();
        }
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

    // ── Property codegen accumulators ──
    let mut prop_field_entries = Vec::new();
    let mut prop_names: Vec<String> = Vec::new();
    let mut prop_kind_tokens = Vec::new();
    let mut prop_is_option_flags = Vec::new();
    let mut prop_is_secret_arms = Vec::new();
    let mut nested_prop_fields = Vec::new();
    let mut nested_get_prop = Vec::new();
    let mut nested_set_prop = Vec::new();
    let mut nested_prop_is_secret = Vec::new();
    let mut init_defaults_ops = Vec::new();

    // ── Map-key (HashMap<String, T>) and List (Vec<T:Configurable>) section
    //    accumulators. Both surface a "+ Add entry" affordance in the
    //    dashboard / CLI; both are auto-discovered from #[nested] fields
    //    whose type is a container. The dispatch table at the gateway
    //    `handle_map_key` walks `Config::map_key_sections()` and matches
    //    on the path string — no hand-maintained list anywhere.
    let mut map_key_section_entries: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut create_map_key_arms: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut map_key_recurse: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut create_map_key_recurse: Vec<proc_macro2::TokenStream> = Vec::new();

    for field in fields {
        let field_ident = field.ident.as_ref().expect("Named field must have ident");
        let is_secret = has_attr(field, "secret");
        let is_nested = has_attr(field, "nested");
        let serde_skip = has_serde_skip(field);
        let derived_from_secret = has_attr(field, "derived_from_secret");

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
                .map(|inner| inner.to_token_stream().to_string() == "String")
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

                // Path routing through HashMap<String, T>: the one parser
                // lives in `crate::config::route_hashmap_path` so get/set
                // don't duplicate it. Paths look like
                // `<my_prefix>.<field>.<key>.<inner_suffix>`; on a hit the
                // dispatch is forwarded to the value type's own get_prop /
                // set_prop via its `configurable_prefix()`.
                let field_name_lit = snake_to_kebab(&field_ident.to_string());
                nested_get_prop.push(quote! {
                    if let Some((hm_key, inner_name)) = crate::config::route_hashmap_path(
                        name,
                        Self::configurable_prefix(),
                        #field_name_lit,
                        <#value_ty>::configurable_prefix(),
                    ) && let Some(inner) = self.#field_ident.get(hm_key)
                        && let Ok(val) = inner.get_prop(&inner_name)
                    {
                        return Ok(val);
                    }
                });
                nested_set_prop.push(quote! {
                    if let Some((hm_key, inner_name)) = crate::config::route_hashmap_path(
                        name,
                        Self::configurable_prefix(),
                        #field_name_lit,
                        <#value_ty>::configurable_prefix(),
                    ) && let Some(inner) = self.#field_ident.get_mut(hm_key)
                        && let Ok(()) = inner.set_prop(&inner_name, value_str)
                    {
                        return Ok(());
                    }
                });

                // Enumerate every HashMap entry and inject its runtime key
                // into the child's static field paths: a child field named
                // `<inner_prefix>.api-key` becomes
                // `<my_prefix>.<field>.<hm_key>.api-key`. Without this, prop_fields()
                // never surfaces e.g. `providers.models.anthropic.api-key`,
                // so onboard has no way to prompt for it.
                nested_prop_fields.push(quote! {
                    {
                        let inner_prefix = <#value_ty>::configurable_prefix();
                        let outer_prefix = if Self::configurable_prefix().is_empty() {
                            #field_name_lit.to_string()
                        } else {
                            format!("{}.{}", Self::configurable_prefix(), #field_name_lit)
                        };
                        for (hm_key, inner) in &self.#field_ident {
                            let base = format!("{outer_prefix}.{hm_key}");
                            for mut field in inner.prop_fields() {
                                let leaf = field
                                    .name
                                    .strip_prefix(inner_prefix)
                                    .and_then(|s| s.strip_prefix('.'))
                                    .unwrap_or(field.name.as_str())
                                    .to_string();
                                field.name = if leaf.is_empty() {
                                    base.clone()
                                } else {
                                    format!("{base}.{leaf}")
                                };
                                fields.push(field);
                            }
                        }
                    }
                });

                // ── Map-key section emission (HashMap<String, T>) ──
                // The dashboard / CLI consume `Self::map_key_sections()` to
                // surface "+ Add" affordances; `create_map_key()` is the
                // typed insertion. Both auto-derived — no hand-table.
                let value_ty_name = value_ty.to_token_stream().to_string();
                let field_doc = extract_doc(&field.attrs);
                map_key_section_entries.push(quote! {
                    out.push(crate::config::MapKeySection {
                        // Path is computed at static-init time via the
                        // configurable_prefix const + field name literal.
                        path: {
                            // SAFETY: leak-once for static lifetime; runs
                            // exactly per (Type, field) pair, bounded by the
                            // schema's field count.
                            let prefix = Self::configurable_prefix();
                            let s = if prefix.is_empty() {
                                #field_name_lit.to_string()
                            } else {
                                format!("{prefix}.{}", #field_name_lit)
                            };
                            Box::leak(s.into_boxed_str())
                        },
                        kind: crate::config::MapKeyKind::Map,
                        value_type: #value_ty_name,
                        description: #field_doc,
                    });
                });
                create_map_key_arms.push(quote! {
                    {
                        let prefix = Self::configurable_prefix();
                        let expected = if prefix.is_empty() {
                            #field_name_lit.to_string()
                        } else {
                            format!("{prefix}.{}", #field_name_lit)
                        };
                        if section_path == expected {
                            if self.#field_ident.contains_key(map_key) {
                                return Ok(false);
                            }
                            let value: #value_ty = serde_json::from_value(
                                serde_json::json!({}),
                            ).map_err(|e| format!(
                                "default-construct {} failed: {e}",
                                stringify!(#value_ty)
                            ))?;
                            self.#field_ident.insert(map_key.to_string(), value);
                            return Ok(true);
                        }
                    }
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

                    // Recurse: pull the inner type's map_key_sections + create_map_key.
                    map_key_recurse.push(quote! {
                        out.extend(<#inner_ty_tokens>::map_key_sections());
                    });
                    create_map_key_recurse.push(quote! {
                        if let Some(inner) = &mut self.#field_ident {
                            match inner.create_map_key(section_path, map_key) {
                                Ok(created) => return Ok(created),
                                Err(_) => {} // not handled by this branch; try next
                            }
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

                // Recurse into the nested type's map_key_sections AND
                // create_map_key for non-Option nested fields. This is how
                // the root Config picks up `providers.models` (declared on
                // ProvidersConfig, not on Config).
                let field_ty = &field.ty;
                map_key_recurse.push(quote! {
                    out.extend(<#field_ty>::map_key_sections());
                });
                create_map_key_recurse.push(quote! {
                    if let Ok(created) = self.#field_ident.create_map_key(section_path, map_key) {
                        return Ok(created);
                    }
                });

                // Vec<T> with #[nested]: T is Configurable; surface as a
                // List section + emit a push-with-default arm.
                if let Some(vec_inner_ty) = extract_vec_inner(&field.ty) {
                    let vec_inner_name = vec_inner_ty.to_token_stream().to_string();
                    let field_doc = extract_doc(&field.attrs);
                    let vec_field_name_lit = snake_to_kebab(&field_ident.to_string());
                    map_key_section_entries.push(quote! {
                        out.push(crate::config::MapKeySection {
                            path: {
                                let prefix = Self::configurable_prefix();
                                let s = if prefix.is_empty() {
                                    #vec_field_name_lit.to_string()
                                } else {
                                    format!("{prefix}.{}", #vec_field_name_lit)
                                };
                                Box::leak(s.into_boxed_str())
                            },
                            kind: crate::config::MapKeyKind::List,
                            value_type: #vec_inner_name,
                            description: #field_doc,
                        });
                    });
                    create_map_key_arms.push(quote! {
                        {
                            let prefix = Self::configurable_prefix();
                            let expected = if prefix.is_empty() {
                                #vec_field_name_lit.to_string()
                            } else {
                                format!("{prefix}.{}", #vec_field_name_lit)
                            };
                            if section_path == expected {
                                let value: #vec_inner_ty = serde_json::from_value(
                                    serde_json::json!({}),
                                ).map_err(|e| format!(
                                    "default-construct {} failed: {e}",
                                    stringify!(#vec_inner_ty)
                                ))?;
                                self.#field_ident.push(value);
                                let new_idx = self.#field_ident.len() - 1;
                                let inner_prefix = <#vec_inner_ty>::configurable_prefix();
                                let _ = self.#field_ident[new_idx].set_prop(
                                    &format!("{inner_prefix}.name"), map_key,
                                );
                                let _ = self.#field_ident[new_idx].set_prop(
                                    &format!("{inner_prefix}.hint"), map_key,
                                );
                                return Ok(true);
                            }
                        }
                    });
                }
            }

            continue; // nested fields handled above
        }

        // ── Property handling for non-nested, non-skip fields ──
        if serde_skip {
            continue;
        }

        // Unwrap Option<T> → T for type inspection
        let is_option = is_option_type(&field.ty);
        let inner_ty = extract_option_inner(&field.ty).unwrap_or(&field.ty);

        // Skip HashMap and PathBuf compound types (handled by other paths
        // or omitted from the prop surface). `Vec<T>` is surfaced as a
        // single prop field — `Vec<String>` becomes PropKind::StringArray
        // (chip editor on the dashboard), any other `Vec<T>` becomes
        // PropKind::ObjectArray (per-row sub-form on the dashboard,
        // JSON-array round-tripped on the wire).
        let vec_inner = extract_vec_inner(inner_ty);
        let is_vec_string = vec_inner
            .map(|t| t.to_token_stream().to_string() == "String")
            .unwrap_or(false);
        let is_vec_object = vec_inner.is_some() && !is_vec_string;
        if is_compound_type(inner_ty) && !is_vec_string && !is_vec_object {
            continue;
        }

        let field_name_kebab = snake_to_kebab(&field_ident.to_string());
        let serde_name = field_ident.to_string();
        let full_name = if prefix.is_empty() {
            field_name_kebab.clone()
        } else {
            format!("{}.{}", prefix, field_name_kebab)
        };
        let full_name_lit = &full_name;
        let serde_name_lit = &serde_name;
        let category_lit = &category;
        let type_str = field.ty.to_token_stream().to_string().replace(' ', "");
        let type_hint_lit = &type_str;
        let description = extract_doc(&field.attrs);
        let description_lit = description.as_str();

        // PropKind resolved at compile time via HasPropKind trait.
        // All field types must implement HasPropKind — scalars in traits.rs,
        // config enums in schema.rs via impl_enum_prop_kind!.
        // Exception: `Vec<T>` of structs (anything that's not `Vec<String>`)
        // is hardcoded to ObjectArray here because a blanket
        // `impl<T> HasPropKind for Vec<T>` would conflict with the existing
        // `Vec<String> -> StringArray` impl, and per-type impls would
        // be tedious to maintain. The macro already knows the field is a
        // Vec via `extract_vec_inner`.
        let kind_token = if is_vec_object {
            quote! { crate::config::PropKind::ObjectArray }
        } else {
            quote! { <#inner_ty as crate::config::HasPropKind>::PROP_KIND }
        };
        // Vec<T> object-array fields are never enums; short-circuit the
        // `HasPropKind::PROP_KIND` probe so the compile doesn't demand
        // a HasPropKind impl on `Vec<T>` for arbitrary T.
        let enum_variants_expr = if is_vec_object {
            quote! { None::<fn() -> Vec<String>> }
        } else {
            quote! {
                {
                    #[cfg(feature = "schema-export")]
                    {
                        if <#inner_ty as crate::config::HasPropKind>::PROP_KIND == crate::config::PropKind::Enum {
                            Some(|| {
                                crate::config::enum_variants::<#inner_ty>()
                                    .split(", ")
                                    .map(str::to_string)
                                    // Defensive: the helper returns a placeholder
                                    // string ("(unknown variants)") when schemars
                                    // can't enumerate variants for the type. Drop
                                    // empties and the placeholder so the dashboard
                                    // form falls back to a text input instead of
                                    // rendering a one-option dropdown of garbage.
                                    .filter(|v| !v.is_empty() && v != "(unknown variants)")
                                    .collect()
                            })
                        } else {
                            None
                        }
                    }
                    #[cfg(not(feature = "schema-export"))]
                    {
                        None::<fn() -> Vec<String>>
                    }
                }
            }
        };

        if is_secret {
            prop_is_secret_arms.push(quote! { #full_name_lit => true, });
        }

        prop_names.push(full_name.clone());
        prop_kind_tokens.push(kind_token.clone());
        prop_is_option_flags.push(is_option);

        if is_vec_object {
            // For `Vec<T>` of structs we bypass make_prop_field's TOML-table
            // lookup and JSON-serialize the field directly, so the dashboard
            // gets `value: [{"name":"fs",...}]` instead of `value: "[{name=...}]"`
            // (TOML inline syntax) that the per-row editor would have to parse.
            prop_field_entries.push(quote! {
                crate::config::PropFieldInfo {
                    name: #full_name_lit.to_string(),
                    category: #category_lit,
                    display_value: serde_json::to_string(&self.#field_ident)
                        .unwrap_or_else(|_| "[]".to_string()),
                    type_hint: #type_hint_lit,
                    kind: #kind_token,
                    is_secret: #is_secret,
                    enum_variants: #enum_variants_expr,
                    description: #description_lit,
                    derived_from_secret: #derived_from_secret,
                }
            });
        } else {
            prop_field_entries.push(quote! {
                crate::config::make_prop_field(
                    __table.as_ref(),
                    #full_name_lit,
                    #serde_name_lit,
                    #category_lit,
                    #type_hint_lit,
                    #kind_token,
                    #is_secret,
                    #enum_variants_expr,
                    #description_lit,
                    #derived_from_secret,
                )
            });
        }
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
                let __table = toml::Value::try_from(self)
                    .ok()
                    .and_then(|v| match v { toml::Value::Table(t) => Some(t), _ => None });
                let mut fields = vec![#(#prop_field_entries),*];
                #(#nested_prop_fields)*
                fields
            }

            /// Get a property value by its full dotted name, returning it as a display string.
            pub fn get_prop(&self, name: &str) -> anyhow::Result<String> {
                #(#nested_get_prop)*
                const KNOWN: &[&str] = &[#(#prop_names),*];
                if !KNOWN.contains(&name) {
                    anyhow::bail!("Unknown property '{}'", name);
                }
                crate::config::serde_get_prop(self, Self::configurable_prefix(), name, Self::prop_is_secret(name))
            }

            /// Set a property value by its full dotted name, parsing from string.
            pub fn set_prop(&mut self, name: &str, value_str: &str) -> anyhow::Result<()> {
                #(#nested_set_prop)*
                const KNOWN: &[&str] = &[#(#prop_names),*];
                const KINDS: &[crate::config::PropKind] = &[#(#prop_kind_tokens),*];
                const IS_OPTION: &[bool] = &[#(#prop_is_option_flags),*];
                let idx = KNOWN.iter().position(|&n| n == name)
                    .ok_or_else(|| anyhow::anyhow!("Unknown property '{}'", name))?;
                crate::config::serde_set_prop(self, Self::configurable_prefix(), name, value_str, KINDS[idx], IS_OPTION[idx])
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

            /// Enumerate every map-keyed (`HashMap<String, T>`) and list-shaped
            /// (`Vec<T>`) section discoverable from this Configurable's tree.
            /// The dashboard / CLI consume this to surface "+ Add" affordances
            /// without hardcoding the section list.
            pub fn map_key_sections() -> Vec<crate::config::MapKeySection> {
                let mut out: Vec<crate::config::MapKeySection> = Vec::new();
                #(#map_key_section_entries)*
                #(#map_key_recurse)*
                out
            }

            /// Insert a default-valued entry under a map-keyed section, or
            /// append to a list-shaped one, with `map_key` as the new entry's
            /// natural identifier (HashMap key for Map sections; identifier
            /// field for List sections).
            ///
            /// Returns `Ok(true)` if a new entry was created, `Ok(false)` if
            /// the entry already existed (idempotent), or `Err(reason)` if
            /// the section path doesn't resolve to a Map/List in this tree.
            pub fn create_map_key(
                &mut self,
                section_path: &str,
                map_key: &str,
            ) -> Result<bool, String> {
                #(#create_map_key_arms)*
                #(#create_map_key_recurse)*
                Err(format!(
                    "no map-keyed/list section at `{}` in `{}`",
                    section_path,
                    Self::configurable_prefix(),
                ))
            }
        }
    };

    TokenStream::from(expanded)
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

/// Flatten a field's `///` doc comment into a single space-separated line.
/// Empty string when the field has no doc comment.
fn extract_doc(attrs: &[syn::Attribute]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
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
        let line = lit_str.value();
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    parts.join(" ")
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
