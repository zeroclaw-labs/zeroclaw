pub fn t(key: &str) -> &str {
    match key {
        "skills.title" => "Skills",
        "skills.agent" => "Agent",
        "skills.search" => "Search skills…",
        "skills.reload" => "Reload",
        "skills.empty" => "This agent has no skills.",
        "skills.load_error" => "Failed to load skills",
        "skills.skipped_count" => "skill(s) skipped (failed security audit)",
        "skills.skill_md" => "SKILL.md",
        "skills.shadows" => "shadows",
        "common.edit" => "Edit",
        _ => key,
    }
}

pub enum AuthText {
    Title,
    Help,
    Token,
    Connect,
    Disconnect,
    Checking,
}

impl AuthText {
    pub fn text(&self) -> &'static str {
        match self {
            Self::Title => "Connect to ZeroClaw",
            Self::Help => "Paste an existing paired bearer token, not a pairing code. It is stored in this browser origin, like the React dashboard.",
            Self::Token => "Gateway token",
            Self::Connect => "Connect",
            Self::Disconnect => "Disconnect / change token",
            Self::Checking => "Checking connection…",
        }
    }
}

impl crate::api::AuthError {
    pub fn text(&self) -> &'static str {
        match self {
            Self::Empty => "Enter a gateway token.",
            Self::Rejected => "Token rejected. Paste a valid paired bearer token.",
            Self::Unavailable => "Cannot validate the token. Check that the gateway and helper are running, then retry.",
            Self::Storage => "Browser storage is unavailable. Allow site storage, then retry.",
        }
    }
}
