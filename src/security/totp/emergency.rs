// Emergency operations: break-glass and E-Stop (D13, D14, D27, F21).
//
// Break-glass: maintainer key stored separately from .secret_key.
// Allows: reset any TOTP, disable TOTP globally, unlock users.
// E-Stop: TOTP-exempt kill switch for immediate agent shutdown.

use std::fs;
use std::path::Path;

/// Break-glass operation types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakGlassOp {
    ResetUser { user_id: String },
    DisableGlobal,
    ReEnable,
    UnlockUser { user_id: String },
    ConfigReload,
}

/// Verify a maintainer key against the stored key file.
pub fn verify_maintainer_key(
    provided_key: &[u8],
    key_path: &Path,
) -> anyhow::Result<bool> {
    if !key_path.exists() {
        anyhow::bail!("maintainer key file not found: {}", key_path.display());
    }

    let stored_key = fs::read(key_path)?;
    Ok(constant_time_eq(provided_key, &stored_key))
}

/// Generate a new maintainer key file.
pub fn generate_maintainer_key(key_path: &Path) -> anyhow::Result<Vec<u8>> {
    use rand::RngCore;
    let mut key = vec![0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);

    fs::write(key_path, &key)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(key_path, fs::Permissions::from_mode(0o600))?;
    }

    Ok(key)
}

/// Validate that a break-glass operation is allowed for the maintainer.
pub fn validate_break_glass(
    op: &BreakGlassOp,
    allowed_operations: &[String],
) -> bool {
    let op_str = match op {
        BreakGlassOp::ResetUser { .. } => "totp.reset_any",
        BreakGlassOp::DisableGlobal => "totp.disable_global",
        BreakGlassOp::ReEnable => "totp.re_enable",
        BreakGlassOp::UnlockUser { .. } => "user.unlock",
        BreakGlassOp::ConfigReload => "config.reload",
    };
    allowed_operations.iter().any(|a| a == op_str)
}

/// Check if a command is an E-Stop (TOTP-exempt, F21).
pub fn is_estop(command: &str) -> bool {
    let normalized = command.trim().to_lowercase();
    normalized == "e_stop"
        || normalized == "estop"
        || normalized == "emergency_stop"
        || normalized.starts_with("e_stop ")
}

/// Constant-time comparison for key verification.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generate_and_verify_key() {
        let dir = TempDir::new().unwrap();
        let key_path = dir.path().join("maintainer.key");
        let key = generate_maintainer_key(&key_path).unwrap();
        assert_eq!(key.len(), 32);
        assert!(key_path.exists());
        assert!(verify_maintainer_key(&key, &key_path).unwrap());
    }

    #[test]
    fn wrong_key_rejected() {
        let dir = TempDir::new().unwrap();
        let key_path = dir.path().join("maintainer.key");
        generate_maintainer_key(&key_path).unwrap();
        let wrong_key = vec![0u8; 32];
        assert!(!verify_maintainer_key(&wrong_key, &key_path).unwrap());
    }

    #[test]
    fn missing_key_file_errors() {
        let result = verify_maintainer_key(b"key", Path::new("/nonexistent/path"));
        assert!(result.is_err());
    }

    #[test]
    fn estop_detection() {
        assert!(is_estop("e_stop"));
        assert!(is_estop("E_STOP"));
        assert!(is_estop("estop"));
        assert!(is_estop("emergency_stop"));
        assert!(is_estop("  e_stop  "));
        assert!(!is_estop("stop"));
        assert!(!is_estop("e_stop_not_really"));
    }

    #[test]
    fn break_glass_op_validation() {
        let allowed = vec![
            "totp.reset_any".to_string(),
            "totp.disable_global".to_string(),
            "config.reload".to_string(),
        ];

        assert!(validate_break_glass(&BreakGlassOp::ResetUser { user_id: "x".into() }, &allowed));
        assert!(validate_break_glass(&BreakGlassOp::DisableGlobal, &allowed));
        assert!(validate_break_glass(&BreakGlassOp::ConfigReload, &allowed));
        assert!(!validate_break_glass(&BreakGlassOp::ReEnable, &allowed));
        assert!(!validate_break_glass(&BreakGlassOp::UnlockUser { user_id: "x".into() }, &allowed));
    }
}
