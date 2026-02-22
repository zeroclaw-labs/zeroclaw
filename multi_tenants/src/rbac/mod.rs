use crate::auth::middleware::AuthUser;
use crate::error::AppError;
use rusqlite::OptionalExtension;

/// Role hierarchy — higher discriminant = more permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer = 0,
    Contributor = 1,
    Manager = 2,
    Owner = 3,
}

impl Role {
    pub fn parse_role(s: &str) -> Option<Self> {
        match s {
            "viewer" => Some(Role::Viewer),
            "contributor" => Some(Role::Contributor),
            "manager" => Some(Role::Manager),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Contributor => "contributor",
            Role::Manager => "manager",
            Role::Owner => "owner",
        }
    }
}

/// Check if `user` has at least `required_role` for `tenant_id`.
/// Super-admins bypass all tenant role checks.
/// Queries the DB on every call so revoked roles take effect immediately.
pub fn require_tenant_role(
    user: &AuthUser,
    tenant_id: &str,
    required_role: Role,
    db: &crate::db::pool::DbPool,
) -> Result<(), AppError> {
    if user.is_super_admin {
        return Ok(());
    }

    // Fresh role lookup — not from JWT cache, so revocations are immediate.
    let role_str: Option<String> = db
        .read(|conn| {
            conn.query_row(
                "SELECT role FROM members WHERE user_id = ?1 AND tenant_id = ?2",
                rusqlite::params![user.claims.sub, tenant_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| anyhow::anyhow!("role lookup failed: {}", e))
        })
        .map_err(|_| AppError::Forbidden("access denied".into()))?;

    let Some(role_str) = role_str else {
        return Err(AppError::Forbidden("not a member of this tenant".into()));
    };

    let user_role =
        Role::parse_role(&role_str).ok_or_else(|| AppError::Forbidden("invalid role".into()))?;

    if user_role < required_role {
        return Err(AppError::Forbidden(format!(
            "requires {} role, you have {}",
            required_role.as_str(),
            user_role.as_str(),
        )));
    }

    Ok(())
}

/// Check if `user` is a super-admin.
pub fn require_super_admin(user: &AuthUser) -> Result<(), AppError> {
    if user.is_super_admin {
        Ok(())
    } else {
        Err(AppError::Forbidden("super_admin required".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::jwt::{Claims, TenantRole};
    use crate::db::pool::DbPool;

    fn make_auth_user(
        sub: &str,
        tenant_roles: Vec<(&str, &str)>,
        is_super_admin: bool,
    ) -> AuthUser {
        AuthUser {
            claims: Claims {
                sub: sub.into(),
                email: "zeroclaw_test@example.com".into(),
                tenant_roles: tenant_roles
                    .into_iter()
                    .map(|(tid, role)| TenantRole {
                        tenant_id: tid.into(),
                        role: role.into(),
                    })
                    .collect(),
                iat: 0,
                exp: 0,
            },
            is_super_admin,
            token: String::new(),
        }
    }

    struct TempDb {
        path: std::path::PathBuf,
    }
    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            let wal = self.path.with_extension("db-wal");
            let shm = self.path.with_extension("db-shm");
            let _ = std::fs::remove_file(wal);
            let _ = std::fs::remove_file(shm);
        }
    }

    /// Open a file-backed temp DB with a minimal members table.
    fn make_test_db(members: &[(&str, &str, &str)]) -> (DbPool, TempDb) {
        let path =
            std::env::temp_dir().join(format!("zcplatform_rbac_test_{}.db", uuid::Uuid::new_v4()));
        let guard = TempDb { path: path.clone() };
        let path_str = path.to_str().unwrap().to_string();
        let db = DbPool::open(&path_str, 1).expect("open db");
        db.write(|conn| {
            conn.execute_batch(
                "CREATE TABLE members (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    tenant_id TEXT NOT NULL,
                    role TEXT NOT NULL
                );",
            )?;
            Ok(())
        })
        .expect("create table");
        for (user_id, tenant_id, role) in members {
            db.write(|conn| {
                conn.execute(
                    "INSERT INTO members (id, user_id, tenant_id, role) VALUES (?, ?, ?, ?)",
                    rusqlite::params![uuid::Uuid::new_v4().to_string(), user_id, tenant_id, role],
                )?;
                Ok(())
            })
            .expect("insert member");
        }
        (db, guard)
    }

    #[test]
    fn test_super_admin_bypasses_tenant_check() {
        // Super-admin bypasses DB lookup entirely — pass an empty DB.
        let (db, _tmp) = make_test_db(&[]);
        let user = make_auth_user("zeroclaw_admin", vec![], true);
        assert!(require_tenant_role(&user, "tenant-abc", Role::Owner, &db).is_ok());
    }

    #[test]
    fn test_owner_can_manage_members() {
        let (db, _tmp) = make_test_db(&[("zeroclaw_user_1", "tenant-abc", "owner")]);
        let user = make_auth_user("zeroclaw_user_1", vec![], false);
        assert!(require_tenant_role(&user, "tenant-abc", Role::Manager, &db).is_ok());
    }

    #[test]
    fn test_viewer_cannot_edit_config() {
        let (db, _tmp) = make_test_db(&[("zeroclaw_user_2", "tenant-abc", "viewer")]);
        let user = make_auth_user("zeroclaw_user_2", vec![], false);
        let result = require_tenant_role(&user, "tenant-abc", Role::Manager, &db);
        assert!(matches!(result, Err(AppError::Forbidden(_))));
    }

    #[test]
    fn test_no_role_returns_forbidden() {
        let (db, _tmp) = make_test_db(&[]);
        let user = make_auth_user("zeroclaw_user_3", vec![], false);
        let result = require_tenant_role(&user, "tenant-abc", Role::Viewer, &db);
        assert!(matches!(result, Err(AppError::Forbidden(_))));
    }

    #[test]
    fn test_role_parse_role_roundtrip() {
        for (s, role) in [
            ("viewer", Role::Viewer),
            ("contributor", Role::Contributor),
            ("manager", Role::Manager),
            ("owner", Role::Owner),
        ] {
            assert_eq!(Role::parse_role(s), Some(role));
            assert_eq!(role.as_str(), s);
        }
        assert_eq!(Role::parse_role("unknown"), None);
    }

    #[test]
    fn test_role_ordering() {
        assert!(Role::Viewer < Role::Contributor);
        assert!(Role::Contributor < Role::Manager);
        assert!(Role::Manager < Role::Owner);
    }
}
