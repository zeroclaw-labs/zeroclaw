//! macOS permission checks for accessibility and screen capture.

/// Check if the process has accessibility permissions (AXIsProcessTrusted).
#[cfg(target_os = "macos")]
pub fn is_accessibility_trusted() -> bool {
    extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }
    unsafe { AXIsProcessTrusted() != 0 }
}

#[cfg(not(target_os = "macos"))]
pub fn is_accessibility_trusted() -> bool {
    false
}

/// Check if screen capture access is available.
#[cfg(target_os = "macos")]
pub fn has_screen_capture_access() -> bool {
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> u8;
    }
    unsafe { CGPreflightScreenCaptureAccess() != 0 }
}

#[cfg(not(target_os = "macos"))]
pub fn has_screen_capture_access() -> bool {
    false
}

/// Request screen capture access (shows system dialog if not granted).
#[cfg(target_os = "macos")]
pub fn request_screen_capture_access() -> bool {
    extern "C" {
        fn CGRequestScreenCaptureAccess() -> u8;
    }
    unsafe { CGRequestScreenCaptureAccess() != 0 }
}

#[cfg(not(target_os = "macos"))]
pub fn request_screen_capture_access() -> bool {
    false
}

/// Check all required permissions and return a status summary.
pub fn check_permissions() -> PermissionStatus {
    PermissionStatus {
        accessibility: is_accessibility_trusted(),
        screen_capture: has_screen_capture_access(),
    }
}

/// Permission status for macOS automation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PermissionStatus {
    pub accessibility: bool,
    pub screen_capture: bool,
}

impl PermissionStatus {
    pub fn all_granted(&self) -> bool {
        self.accessibility && self.screen_capture
    }

    pub fn missing_permissions(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.accessibility {
            missing.push("Accessibility (System Settings → Privacy & Security → Accessibility)");
        }
        if !self.screen_capture {
            missing
                .push("Screen Recording (System Settings → Privacy & Security → Screen Recording)");
        }
        missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_granted_when_both_true() {
        let status = PermissionStatus {
            accessibility: true,
            screen_capture: true,
        };
        assert!(status.all_granted());
        assert!(status.missing_permissions().is_empty());
    }

    #[test]
    fn not_all_granted_when_accessibility_false() {
        let status = PermissionStatus {
            accessibility: false,
            screen_capture: true,
        };
        assert!(!status.all_granted());
        assert_eq!(status.missing_permissions().len(), 1);
        assert!(status.missing_permissions()[0].contains("Accessibility"));
    }

    #[test]
    fn not_all_granted_when_screen_capture_false() {
        let status = PermissionStatus {
            accessibility: true,
            screen_capture: false,
        };
        assert!(!status.all_granted());
        assert_eq!(status.missing_permissions().len(), 1);
        assert!(status.missing_permissions()[0].contains("Screen Recording"));
    }

    #[test]
    fn missing_both_returns_two() {
        let status = PermissionStatus {
            accessibility: false,
            screen_capture: false,
        };
        assert!(!status.all_granted());
        assert_eq!(status.missing_permissions().len(), 2);
    }

    #[test]
    fn permission_status_serializes() {
        let status = PermissionStatus {
            accessibility: true,
            screen_capture: false,
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["accessibility"], true);
        assert_eq!(json["screen_capture"], false);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn check_permissions_returns_status() {
        let status = check_permissions();
        // Just verify it runs without panicking — actual values depend on system config
        let _ = status.all_granted();
    }
}
