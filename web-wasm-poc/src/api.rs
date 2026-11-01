use serde::Deserialize;

pub const TOKEN_KEY: &str = "zeroclaw_token";
pub const ORIGIN_KEY: &str = "__ZC_API_ORIGIN__";

#[derive(Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShadowedSkillEntry {
    pub name: String,
    pub origin: String,
}

#[derive(Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DroppedSkillEntry {
    pub name: String,
    pub origin: String,
    #[serde(rename = "reason_kind")]
    pub reason_kind: String,
    pub reason: String,
    #[serde(default)]
    pub directory: Option<String>,
}

#[derive(Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkillEntry {
    pub name: String,
    pub description: String,
    pub origin: String,
    #[serde(default)]
    pub plugin: Option<String>,
    #[serde(default)]
    pub bundle: Option<String>,
    #[serde(default)]
    pub directory: Option<String>,
    pub editable: bool,
    #[serde(default)]
    pub shadowed: Option<Vec<ShadowedSkillEntry>>,
}

#[derive(Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SkillDocument {
    pub bundle: String,
    pub name: String,
    pub frontmatter: SkillFrontmatter,
    pub body: String,
}

#[derive(Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentOptionsResponse {
    pub agents: Vec<String>,
}

#[derive(Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkillsResponse {
    pub agent: String,
    pub skills: Vec<AgentSkillEntry>,
    #[serde(default)]
    pub dropped: Vec<DroppedSkillEntry>,
}

pub fn api_origin() -> String {
    origin_from_window(web_sys::window())
}

fn origin_from_window(window: Option<web_sys::Window>) -> String {
    let Some(window) = window else {
        return String::new();
    };
    let value = js_sys::Reflect::get(&window, &ORIGIN_KEY.into())
        .ok()
        .unwrap_or(wasm_bindgen::JsValue::UNDEFINED);
    value.as_string().unwrap_or_default()
}

pub fn auth_token() -> Option<String> {
    let window = web_sys::window()?;
    let storage = window.local_storage().ok().flatten()?;
    storage.get_item(TOKEN_KEY).ok().flatten()
}

#[derive(Clone, Copy, PartialEq)]
pub enum AuthError {
    Empty,
    Rejected,
    Unavailable,
    Storage,
}

fn token_storage() -> Result<web_sys::Storage, AuthError> {
    web_sys::window()
        .ok_or(AuthError::Storage)?
        .local_storage()
        .map_err(|_| AuthError::Storage)?
        .ok_or(AuthError::Storage)
}

pub fn store_token(token: &str) -> Result<(), AuthError> {
    token_storage()?
        .set_item(TOKEN_KEY, token)
        .map_err(|_| AuthError::Storage)
}

pub fn clear_token() -> Result<(), AuthError> {
    token_storage()?
        .remove_item(TOKEN_KEY)
        .map_err(|_| AuthError::Storage)
}

pub async fn validate_token(token: &str) -> Result<(), AuthError> {
    if token.trim().is_empty() {
        return Err(AuthError::Empty);
    }
    let response =
        gloo_net::http::Request::get(&format!("{}/api/config/agent-options", api_origin()))
            .header("Authorization", &format!("Bearer {token}"))
            .send()
            .await
            .map_err(|_| AuthError::Unavailable)?;
    if response.status() == 401 || response.status() == 403 {
        return Err(AuthError::Rejected);
    }
    if !response.ok() {
        return Err(AuthError::Unavailable);
    }
    response
        .json::<AgentOptionsResponse>()
        .await
        .map(|_| ())
        .map_err(|_| AuthError::Unavailable)
}

pub async fn get<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let url = format!("{}{}", api_origin(), path);
    let mut request = gloo_net::http::Request::get(&url);
    if let Some(token) = auth_token() {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    if !response.ok() {
        return Err(format!("HTTP {}", response.status()));
    }
    response.json::<T>().await.map_err(|e| e.to_string())
}

pub async fn agent_options() -> Result<AgentOptionsResponse, String> {
    get("/api/config/agent-options").await
}

pub async fn agent_skills(alias: &str) -> Result<AgentSkillsResponse, String> {
    let path = format!("/api/agents/{}/skills", urlencode(alias));
    get(&path).await
}

pub async fn read_skill(bundle: &str, name: &str) -> Result<SkillDocument, String> {
    let path = format!(
        "/api/skills/bundles/{}/skills/{}",
        urlencode(bundle),
        urlencode(name)
    );
    get(&path).await
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn missing_window_uses_same_origin_without_panicking() {
        assert_eq!(super::origin_from_window(None), "");
    }
}
