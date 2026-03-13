//! Shared types for macOS desktop automation.

use serde::{Deserialize, Serialize};

/// Information about a window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub owner_name: String,
    pub owner_pid: i32,
    pub bounds: Rect,
    pub on_screen: bool,
    pub layer: i32,
}

/// A rectangle with position and size.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A 2D point.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Information about a running application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    pub bundle_id: Option<String>,
    pub pid: i32,
    pub is_active: bool,
    pub is_hidden: bool,
}

/// Display information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    pub scale_factor: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_info_roundtrip() {
        let win = WindowInfo {
            id: 42,
            title: "Test Window".to_string(),
            owner_name: "TestApp".to_string(),
            owner_pid: 1234,
            bounds: Rect {
                x: 10.0,
                y: 20.0,
                width: 800.0,
                height: 600.0,
            },
            on_screen: true,
            layer: 0,
        };
        let json = serde_json::to_string(&win).unwrap();
        let deserialized: WindowInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, 42);
        assert_eq!(deserialized.title, "Test Window");
        assert_eq!(deserialized.owner_pid, 1234);
        assert_eq!(deserialized.bounds.width, 800.0);
    }

    #[test]
    fn app_info_roundtrip() {
        let app = AppInfo {
            name: "Finder".to_string(),
            bundle_id: Some("com.apple.finder".to_string()),
            pid: 100,
            is_active: true,
            is_hidden: false,
        };
        let json = serde_json::to_string(&app).unwrap();
        let deserialized: AppInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "Finder");
        assert_eq!(deserialized.bundle_id.as_deref(), Some("com.apple.finder"));
    }

    #[test]
    fn app_info_no_bundle_id() {
        let app = AppInfo {
            name: "Custom".to_string(),
            bundle_id: None,
            pid: 200,
            is_active: false,
            is_hidden: true,
        };
        let json = serde_json::to_string(&app).unwrap();
        let deserialized: AppInfo = serde_json::from_str(&json).unwrap();
        assert!(deserialized.bundle_id.is_none());
    }

    #[test]
    fn display_info_roundtrip() {
        let display = DisplayInfo {
            id: 1,
            width: 2560,
            height: 1600,
            is_primary: true,
            scale_factor: 2.0,
        };
        let json = serde_json::to_string(&display).unwrap();
        let deserialized: DisplayInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.width, 2560);
        assert!(deserialized.is_primary);
        assert_eq!(deserialized.scale_factor, 2.0);
    }

    #[test]
    fn point_copy() {
        let p = Point { x: 1.5, y: 2.5 };
        let p2 = p;
        assert_eq!(p.x, p2.x);
        assert_eq!(p.y, p2.y);
    }

    #[test]
    fn rect_copy() {
        let r = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
        };
        let r2 = r;
        assert_eq!(r.width, r2.width);
    }
}
