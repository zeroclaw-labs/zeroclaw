//! System information via sysctl.

use anyhow::Result;
use serde::Serialize;

/// System information summary.
#[derive(Debug, Clone, Serialize)]
pub struct SystemInfo {
    pub hostname: String,
    pub os_version: String,
    pub cpu_model: String,
    pub cpu_cores: u32,
    pub memory_gb: f64,
    pub uptime_secs: u64,
}

/// Get system information.
#[cfg(target_os = "macos")]
pub fn system_info() -> Result<SystemInfo> {
    use std::process::Command;

    fn sysctl_str(key: &str) -> String {
        Command::new("sysctl")
            .args(["-n", key])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }

    fn sysctl_u64(key: &str) -> u64 {
        sysctl_str(key).parse().unwrap_or(0)
    }

    let hostname = sysctl_str("kern.hostname");
    let os_version = sysctl_str("kern.osproductversion");
    let cpu_model = sysctl_str("machdep.cpu.brand_string");
    let cpu_cores = sysctl_str("hw.ncpu").parse().unwrap_or(0);
    let memory_bytes = sysctl_u64("hw.memsize");
    let memory_gb = memory_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let uptime_secs = {
        let boottime = sysctl_str("kern.boottime");
        // Parse "{ sec = 1234, usec = 5678 }"
        boottime
            .split("sec = ")
            .nth(1)
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|boot| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs().saturating_sub(boot))
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    };

    Ok(SystemInfo {
        hostname,
        os_version,
        cpu_model,
        cpu_cores,
        memory_gb,
        uptime_secs,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn system_info() -> Result<SystemInfo> {
    Ok(SystemInfo {
        hostname: String::new(),
        os_version: String::new(),
        cpu_model: String::new(),
        cpu_cores: 0,
        memory_gb: 0.0,
        uptime_secs: 0,
    })
}

/// Battery status.
#[derive(Debug, Clone, Serialize)]
pub struct BatteryInfo {
    pub level: Option<f64>,
    pub charging: bool,
    pub on_battery: bool,
}

/// Get battery status.
#[cfg(target_os = "macos")]
pub fn battery_info() -> Result<BatteryInfo> {
    use std::process::Command;

    let output = Command::new("pmset")
        .args(["-g", "batt"])
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to get battery info: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    let level = stdout.lines().find(|l| l.contains('%')).and_then(|l| {
        l.split('\t')
            .nth(1)
            .and_then(|s| s.split('%').next())
            .and_then(|s| s.trim().parse::<f64>().ok())
    });

    let charging = stdout.contains("charging") && !stdout.contains("discharging");
    let on_battery = stdout.contains("Battery Power");

    Ok(BatteryInfo {
        level,
        charging,
        on_battery,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn battery_info() -> Result<BatteryInfo> {
    Ok(BatteryInfo {
        level: None,
        charging: false,
        on_battery: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_info_serializes() {
        let info = SystemInfo {
            hostname: "test-host".to_string(),
            os_version: "15.0".to_string(),
            cpu_model: "Apple M2".to_string(),
            cpu_cores: 8,
            memory_gb: 16.0,
            uptime_secs: 3600,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["hostname"], "test-host");
        assert_eq!(json["cpu_cores"], 8);
        assert_eq!(json["memory_gb"], 16.0);
    }

    #[test]
    fn battery_info_serializes() {
        let info = BatteryInfo {
            level: Some(85.0),
            charging: true,
            on_battery: false,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["level"], 85.0);
        assert_eq!(json["charging"], true);
        assert_eq!(json["on_battery"], false);
    }

    #[test]
    fn battery_info_null_level() {
        let info = BatteryInfo {
            level: None,
            charging: false,
            on_battery: false,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert!(json["level"].is_null());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn system_info_returns_real_data() {
        let info = system_info().unwrap();
        assert!(!info.hostname.is_empty());
        assert!(info.cpu_cores > 0);
        assert!(info.memory_gb > 0.0);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn battery_info_succeeds() {
        let info = battery_info().unwrap();
        // On desktops level may be None, just verify no panic
        let _ = info.level;
    }
}
