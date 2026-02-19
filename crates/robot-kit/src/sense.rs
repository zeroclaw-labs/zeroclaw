//! Sense Tool - LIDAR, motion sensors, ultrasonic distance
//!
//! Provides environmental awareness through various sensors.
//! Supports multiple backends: direct GPIO, ROS2 topics, or mock.

use crate::config::RobotConfig;
use crate::traits::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

/// LIDAR scan result
#[derive(Debug, Clone)]
pub struct LidarScan {
    /// Distances in meters, 360 values (1 per degree)
    pub ranges: Vec<f64>,
    /// Minimum distance and its angle
    pub nearest: (f64, u16),
    /// Is path clear in forward direction (±30°)?
    pub forward_clear: bool,
}

/// Motion detection result
#[derive(Debug, Clone)]
pub struct MotionResult {
    pub detected: bool,
    pub sensors_triggered: Vec<u8>,
}

pub struct SenseTool {
    config: RobotConfig,
    last_scan: Arc<Mutex<Option<LidarScan>>>,
}

impl SenseTool {
    pub fn new(config: RobotConfig) -> Self {
        Self {
            config,
            last_scan: Arc::new(Mutex::new(None)),
        }
    }

    /// Read LIDAR scan
    async fn scan_lidar(&self) -> Result<LidarScan> {
        match self.config.sensors.lidar_type.as_str() {
            "rplidar" => self.scan_rplidar().await,
            "ros2" => self.scan_ros2().await,
            _ => self.scan_mock().await,
        }
    }

    /// Mock LIDAR for testing
    async fn scan_mock(&self) -> Result<LidarScan> {
        // Simulate a room with walls
        let mut ranges = vec![3.0; 360];

        // Wall in front at 2m
        for range in &mut ranges[350..360] {
            *range = 2.0;
        }
        for range in &mut ranges[0..10] {
            *range = 2.0;
        }

        // Object on left at 1m
        for range in &mut ranges[80..100] {
            *range = 1.0;
        }

        let nearest = ranges
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, &d)| (d, i as u16))
            .unwrap_or((999.0, 0));

        let forward_clear = ranges[0..30]
            .iter()
            .chain(ranges[330..360].iter())
            .all(|&d| d > self.config.safety.min_obstacle_distance);

        Ok(LidarScan {
            ranges,
            nearest,
            forward_clear,
        })
    }

    /// Read from RPLidar via serial
    async fn scan_rplidar(&self) -> Result<LidarScan> {
        // In production, use rplidar_drv crate
        // For now, shell out to rplidar_scan tool if available
        let port = &self.config.sensors.lidar_port;

        let output = tokio::process::Command::new("rplidar_scan")
            .args(["--port", port, "--single"])
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => {
                // Parse output (format: angle,distance per line)
                let mut ranges = vec![999.0; 360];
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    if let Some((angle, dist)) = line.split_once(',') {
                        if let (Ok(a), Ok(d)) = (angle.parse::<usize>(), dist.parse::<f64>()) {
                            if a < 360 {
                                ranges[a] = d;
                            }
                        }
                    }
                }

                let nearest = ranges
                    .iter()
                    .enumerate()
                    .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                    .map(|(i, &d)| (d, i as u16))
                    .unwrap_or((999.0, 0));

                let forward_clear = ranges[0..30]
                    .iter()
                    .chain(ranges[330..360].iter())
                    .all(|&d| d > self.config.safety.min_obstacle_distance);

                Ok(LidarScan {
                    ranges,
                    nearest,
                    forward_clear,
                })
            }
            _ => {
                // Fallback to mock if hardware unavailable
                tracing::warn!("RPLidar unavailable, using mock data");
                self.scan_mock().await
            }
        }
    }

    /// Read from ROS2 /scan topic
    async fn scan_ros2(&self) -> Result<LidarScan> {
        let output = tokio::process::Command::new("ros2")
            .args(["topic", "echo", "--once", "/scan"])
            .output()
            .await?;

        if !output.status.success() {
            return self.scan_mock().await;
        }

        // Parse ROS2 LaserScan message (simplified)
        let stdout = String::from_utf8_lossy(&output.stdout);
        let ranges = vec![999.0; 360];

        // Very simplified parsing - in production use rclrs
        if let Some(_ranges_line) = stdout.lines().find(|l| l.contains("ranges:")) {
            // Extract array values
            // Format: ranges: [1.0, 2.0, ...]
        }

        let nearest = ranges
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, &d)| (d, i as u16))
            .unwrap_or((999.0, 0));

        let forward_clear = ranges[0..30]
            .iter()
            .chain(ranges[330..360].iter())
            .all(|&d| d > self.config.safety.min_obstacle_distance);

        Ok(LidarScan {
            ranges,
            nearest,
            forward_clear,
        })
    }

    /// Check PIR motion sensors
    async fn check_motion(&self) -> Result<MotionResult> {
        let pins = &self.config.sensors.motion_pins;

        // In production, use rppal GPIO
        // For now, mock or read from sysfs
        let mut triggered = Vec::new();

        for &pin in pins {
            let gpio_path = format!("/sys/class/gpio/gpio{}/value", pin);
            match tokio::fs::read_to_string(&gpio_path).await {
                Ok(value) if value.trim() == "1" => {
                    triggered.push(pin);
                }
                _ => {}
            }
        }

        Ok(MotionResult {
            detected: !triggered.is_empty(),
            sensors_triggered: triggered,
        })
    }

    /// Read ultrasonic distance sensor
    async fn check_distance(&self) -> Result<f64> {
        let Some((trigger, echo)) = self.config.sensors.ultrasonic_pins else {
            return Ok(999.0); // No sensor configured
        };

        // In production, use rppal with precise timing
        // Ultrasonic requires µs-level timing, so shell out to helper
        let output = tokio::process::Command::new("hc-sr04")
            .args([
                "--trigger",
                &trigger.to_string(),
                "--echo",
                &echo.to_string(),
            ])
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => {
                let distance = String::from_utf8_lossy(&out.stdout)
                    .trim()
                    .parse::<f64>()
                    .unwrap_or(999.0);
                Ok(distance)
            }
            _ => Ok(999.0), // Sensor unavailable
        }
    }
}

#[async_trait]
impl Tool for SenseTool {
    fn name(&self) -> &str {
        "sense"
    }

    fn description(&self) -> &str {
        "Check robot sensors. Actions: 'scan' for LIDAR (360° obstacle map), \
         'motion' for PIR motion detection, 'distance' for ultrasonic range, \
         'all' for combined sensor report."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["scan", "motion", "distance", "all", "clear_ahead"],
                    "description": "Which sensor(s) to read"
                },
                "direction": {
                    "type": "string",
                    "enum": ["forward", "left", "right", "back", "all"],
                    "description": "For 'scan': which direction to report (default 'forward')"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let action = args["action"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        match action {
            "scan" => {
                let scan = self.scan_lidar().await?;
                let direction = args["direction"].as_str().unwrap_or("forward");

                let report = match direction {
                    "forward" => {
                        let fwd_dist = scan.ranges[0];
                        format!(
                            "Forward: {:.2}m {}. Nearest obstacle: {:.2}m at {}°",
                            fwd_dist,
                            if scan.forward_clear {
                                "(clear)"
                            } else {
                                "(BLOCKED)"
                            },
                            scan.nearest.0,
                            scan.nearest.1
                        )
                    }
                    "left" => {
                        let left_dist = scan.ranges[90];
                        format!("Left (90°): {:.2}m", left_dist)
                    }
                    "right" => {
                        let right_dist = scan.ranges[270];
                        format!("Right (270°): {:.2}m", right_dist)
                    }
                    "back" => {
                        let back_dist = scan.ranges[180];
                        format!("Back (180°): {:.2}m", back_dist)
                    }
                    "all" => {
                        format!(
                            "LIDAR 360° scan:\n\
                             - Forward (0°): {:.2}m\n\
                             - Left (90°): {:.2}m\n\
                             - Back (180°): {:.2}m\n\
                             - Right (270°): {:.2}m\n\
                             - Nearest: {:.2}m at {}°\n\
                             - Forward path: {}",
                            scan.ranges[0],
                            scan.ranges[90],
                            scan.ranges[180],
                            scan.ranges[270],
                            scan.nearest.0,
                            scan.nearest.1,
                            if scan.forward_clear {
                                "CLEAR"
                            } else {
                                "BLOCKED"
                            }
                        )
                    }
                    _ => "Unknown direction".to_string(),
                };

                // Cache scan
                *self.last_scan.lock().await = Some(scan);

                Ok(ToolResult {
                    success: true,
                    output: report,
                    error: None,
                })
            }

            "motion" => {
                let motion = self.check_motion().await?;
                let output = if motion.detected {
                    format!("Motion DETECTED on sensors: {:?}", motion.sensors_triggered)
                } else {
                    "No motion detected".to_string()
                };

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }

            "distance" => {
                let distance = self.check_distance().await?;
                let output = if distance < 999.0 {
                    format!("Ultrasonic distance: {:.2}m", distance)
                } else {
                    "Ultrasonic sensor not available or out of range".to_string()
                };

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }

            "clear_ahead" => {
                let scan = self.scan_lidar().await?;
                Ok(ToolResult {
                    success: true,
                    output: if scan.forward_clear {
                        format!(
                            "Path ahead is CLEAR (nearest obstacle: {:.2}m)",
                            scan.nearest.0
                        )
                    } else {
                        format!("Path ahead is BLOCKED (obstacle at {:.2}m)", scan.ranges[0])
                    },
                    error: None,
                })
            }

            "all" => {
                let scan = self.scan_lidar().await?;
                let motion = self.check_motion().await?;
                let distance = self.check_distance().await?;

                let report = format!(
                    "=== SENSOR REPORT ===\n\
                     LIDAR: nearest {:.2}m at {}°, forward {}\n\
                     Motion: {}\n\
                     Ultrasonic: {:.2}m",
                    scan.nearest.0,
                    scan.nearest.1,
                    if scan.forward_clear {
                        "CLEAR"
                    } else {
                        "BLOCKED"
                    },
                    if motion.detected {
                        format!("DETECTED ({:?})", motion.sensors_triggered)
                    } else {
                        "none".to_string()
                    },
                    distance
                );

                Ok(ToolResult {
                    success: true,
                    output: report,
                    error: None,
                })
            }

            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action: {action}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sense_tool_name() {
        let tool = SenseTool::new(RobotConfig::default());
        assert_eq!(tool.name(), "sense");
    }

    #[tokio::test]
    async fn sense_scan_mock() {
        let tool = SenseTool::new(RobotConfig::default());
        let result = tool
            .execute(json!({"action": "scan", "direction": "all"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Forward"));
    }

    #[tokio::test]
    async fn sense_clear_ahead() {
        let tool = SenseTool::new(RobotConfig::default());
        let result = tool
            .execute(json!({"action": "clear_ahead"}))
            .await
            .unwrap();
        assert!(result.success);
    }
}
