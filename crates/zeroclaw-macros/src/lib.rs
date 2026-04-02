use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields, Lit, Meta};

/// Derive macro that generates `secret_fields()` and `set_secret()` methods
/// for config structs with `#[secret]` and `#[has_secrets]` annotated fields.
///
/// # Attributes
///
/// - `#[secret]` on a `String` or `Option<String>` field marks it as a secret.
/// - `#[has_secrets]` on a nested struct or `Option<StructWithSecrets>` field
///   delegates secret discovery and setting to the child.
/// - `#[secret_prefix = "channels.matrix"]` on the struct sets the dotted path prefix.
///
/// # Example
///
/// ```ignore
/// #[derive(HasSecrets)]
/// #[secret_prefix = "channels.matrix"]
/// pub struct MatrixConfig {
///     #[secret]
///     pub access_token: String,
///     pub room_id: String,  // not a secret
/// }
///
/// #[derive(HasSecrets)]
/// pub struct ChannelsConfig {
///     #[has_secrets]
///     pub matrix: Option<MatrixConfig>,
/// }
///
/// #[derive(HasSecrets)]
/// pub struct Config {
///     #[secret]
///     pub api_key: Option<String>,
///     #[has_secrets]
///     pub channels_config: ChannelsConfig,
/// }
/// ```
///
/// `config.secret_fields()` traverses the entire tree automatically.
/// `config.set_secret("channels.matrix.access-token", value)` dispatches to the right child.
#[proc_macro_derive(HasSecrets, attributes(secret, has_secrets, secret_prefix))]
pub fn derive_has_secrets(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;

    let prefix = extract_prefix(&input);
    let category = derive_category(&prefix);

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => panic!("HasSecrets only supports structs with named fields"),
        },
        _ => panic!("HasSecrets can only be derived for structs"),
    };

    let mut secret_field_entries = Vec::new();
    let mut set_arms = Vec::new();
    let mut encrypt_ops = Vec::new();
    let mut decrypt_ops = Vec::new();
    let mut nested_collect = Vec::new();
    let mut nested_set = Vec::new();
    let mut nested_encrypt = Vec::new();
    let mut nested_decrypt = Vec::new();

    for field in fields {
        let field_ident = field.ident.as_ref().expect("Named field must have ident");

        if has_attr(field, "secret") {
            // Direct secret field
            let field_name_kebab = snake_to_kebab(&field_ident.to_string());
            let full_name = if prefix.is_empty() {
                field_name_kebab.clone()
            } else {
                format!("{}.{}", prefix, field_name_kebab)
            };

            let is_option = is_option_type(&field.ty);
            let full_name_lit = &full_name;
            let category_lit = &category;

            if is_option {
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
                    if let Some(ref raw) = self.#field_ident {
                        if !crate::security::SecretStore::is_encrypted(raw) {
                            self.#field_ident = Some(
                                store.encrypt(raw)
                                    .with_context(|| format!("Failed to encrypt {}", #full_name_lit))?
                            );
                        }
                    }
                });
                decrypt_ops.push(quote! {
                    if let Some(ref raw) = self.#field_ident {
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
        } else if has_attr(field, "has_secrets") {
            // Nested struct that also derives HasSecrets
            let is_option = is_option_type(&field.ty);

            if is_option {
                nested_collect.push(quote! {
                    if let Some(ref inner) = self.#field_ident {
                        fields.extend(inner.secret_fields());
                    }
                });
                nested_set.push(quote! {
                    if let Some(ref mut inner) = self.#field_ident {
                        if let Ok(()) = inner.set_secret(name, value.clone()) {
                            return Ok(());
                        }
                    }
                });
                nested_encrypt.push(quote! {
                    if let Some(ref mut inner) = self.#field_ident {
                        inner.encrypt_secrets(store)?;
                    }
                });
                nested_decrypt.push(quote! {
                    if let Some(ref mut inner) = self.#field_ident {
                        inner.decrypt_secrets(store)?;
                    }
                });
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
            }
        }
    }

    let expanded = quote! {
        impl #struct_name {
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
        if attr.path().is_ident("secret_prefix") {
            if let Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(expr_lit) = &nv.value {
                    if let Lit::Str(lit_str) = &expr_lit.lit {
                        return lit_str.value();
                    }
                }
            }
        }
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
    if let syn::Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            if segment.ident == "Option" {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
