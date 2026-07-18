#[cfg(feature = "provider-gitea")]
pub mod gitea;
#[cfg(feature = "provider-github")]
pub mod github;

/// Map the provider-neutral [`ForgeMethod`](crate::git::types::ForgeMethod)
/// onto a `reqwest::Method`. Lives here so every provider shares one mapping
/// and the contract stays reqwest-free.
#[cfg(any(feature = "provider-github", feature = "provider-gitea"))]
pub(crate) fn forge_method_to_reqwest(method: crate::git::types::ForgeMethod) -> reqwest::Method {
    use crate::git::types::ForgeMethod;
    match method {
        ForgeMethod::Get => reqwest::Method::GET,
        ForgeMethod::Post => reqwest::Method::POST,
        ForgeMethod::Patch => reqwest::Method::PATCH,
        ForgeMethod::Put => reqwest::Method::PUT,
        ForgeMethod::Delete => reqwest::Method::DELETE,
    }
}
