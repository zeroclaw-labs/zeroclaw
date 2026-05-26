use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Role {
    Viewer,
    Operator,
    Admin,
}

impl Role {
    pub fn from_str_or_default(s: &str) -> Self {
        match s {
            "admin" => Self::Admin,
            "operator" => Self::Operator,
            _ => Self::Viewer,
        }
    }

    pub fn can_read(self) -> bool {
        true
    }

    pub fn can_execute(self) -> bool {
        matches!(self, Self::Operator | Self::Admin)
    }

    pub fn can_approve(self) -> bool {
        matches!(self, Self::Admin)
    }

    pub fn can_delete(self) -> bool {
        matches!(self, Self::Admin)
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Viewer => write!(f, "viewer"),
            Self::Operator => write!(f, "operator"),
            Self::Admin => write!(f, "admin"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_permissions() {
        assert!(Role::Viewer.can_read());
        assert!(!Role::Viewer.can_execute());
        assert!(!Role::Viewer.can_approve());

        assert!(Role::Operator.can_read());
        assert!(Role::Operator.can_execute());
        assert!(!Role::Operator.can_approve());

        assert!(Role::Admin.can_read());
        assert!(Role::Admin.can_execute());
        assert!(Role::Admin.can_approve());
        assert!(Role::Admin.can_delete());
    }

    #[test]
    fn role_ordering() {
        assert!(Role::Admin > Role::Operator);
        assert!(Role::Operator > Role::Viewer);
    }
}
