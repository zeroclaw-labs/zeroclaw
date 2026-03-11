//! Drive Tool - Motor control for omni-directional movement
//!
//! Supports multiple backends:
//! - ROS2: Publishes geometry_msgs/Twist to cmd_vel topic
//! - GPIO: Direct PWM control via rppal
//! - Serial: Arduino/motor controller via serial commands
//! - Mock: Logs commands for testing

use crate::config::RobotConfig;
use crate::traits::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Drive backend abstraction
#[async_trait]
trait DriveBackend: Send + Sync {
    async fn move_robot(
        &self,
        linear_x: f64,
        linear_y: f64,
        angular_z: f64,
        duration_ms: u64,
    ) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    #[allow(dead_code)]
    async fn get_odometry(&self) -> Result<(f64, f64, f64)>; // x, y, theta - reserved for future odometry integration
}

/// Mock backend for testing
struct MockDrive;

#[async_trait]
impl DriveBackend for MockDrive {
    async fn move_robot(
        &self,
        linear_x: f64,
        linear_y: f64,
        angular_z: f64,
        duration_ms: u64,
    ) -> Result<()> {
        tracing::info!(
            "MOCK DRIVE: linear=({:.2}, {:.2}), angular={:.2}, duration={}ms",
            linear_x,
            linear_y,
            angular_z,
            duration_ms
        );
        tokio::time::sleep(Duration::from_millis(duration_ms.min(100))).await;
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        tracing::info!("MOCK DRIVE: STOP");
        Ok(())
    }

    async fn get_odometry(&self) -> Result<(f64, f64, f64)> {
        Ok((0.0, 0.0, 0.0))
    }
}

/// ROS2 backend - shells out to ros2 topic pub
struct Ros2Drive {
    topic: String,
}

#[async_trait]
impl DriveBackend for Ros2Drive {
    async fn move_robot(
        &self,
        linear_x: f64,
        linear_y: f64,
        angular_z: f64,
        duration_ms: u64,
    ) -> Result<()> {
        // Publish Twist message via ros2 CLI
        // In production, use rclrs (Rust ROS2 bindings) instead
        let msg = format!(
            "{{linear: {{x: {:.2}, y: {:.2}, z: 0.0}}, angular: {{x: 0.0, y: 0.0, z: {:.2}}}}}",
            linear_x, linear_y, angular_z
        );

        let output = tokio::process::Command::new("ros2")
            .args([
                "topic",
                "pub",
                "--once",
                &self.topic,
                "geometry_msgs/msg/Twist",
                &msg,
            ])
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!(
                "ROS2 publish failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Hold for duration then stop
        tokio::time::sleep(Duration::from_millis(duration_ms)).await;
        self.stop().await?;

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let msg = "{linear: {x: 0.0, y: 0.0, z: 0.0}, angular: {x: 0.0, y: 0.0, z: 0.0}}";
        tokio::process::Command::new("ros2")
            .args([
                "topic",
                "pub",
                "--once",
                &self.topic,
                "geometry_msgs/msg/Twist",
                msg,
            ])
            .output()
            .await?;
        Ok(())
    }

    async fn get_odometry(&self) -> Result<(f64, f64, f64)> {
        // Would subscribe to /odom topic in production
        Ok((0.0, 0.0, 0.0))
    }
}

/// Serial backend - sends commands to Arduino/motor controller
struct SerialDrive {
    port: String,
}

#[async_trait]
impl DriveBackend for SerialDrive {
    async fn move_robot(
        &self,
        linear_x: f64,
        linear_y: f64,
        angular_z: f64,
        duration_ms: u64,
    ) -> Result<()> {
        // Protocol: "M <lx> <ly> <az> <ms>\n"
        // The motor controller interprets this and drives motors
        let cmd = format!(
            "M {:.2} {:.2} {:.2} {}\n",
            linear_x, linear_y, angular_z, duration_ms
        );

        // Use blocking serial in spawn_blocking
        let port = self.port.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut serial = std::fs::OpenOptions::new().write(true).open(&port)?;
            serial.write_all(cmd.as_bytes())?;
            serial.flush()?;
            Ok::<_, anyhow::Error>(())
        })
        .await??;

        tokio::time::sleep(Duration::from_millis(duration_ms)).await;
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.move_robot(0.0, 0.0, 0.0, 0).await
    }

    async fn get_odometry(&self) -> Result<(f64, f64, f64)> {
        Ok((0.0, 0.0, 0.0))
    }
}

/// Main Drive Tool
pub struct DriveTool {
    config: RobotConfig,
    backend: Arc<dyn DriveBackend>,
    last_command: Arc<Mutex<Option<std::time::Instant>>>,
}

impl DriveTool {
    pub fn new(config: RobotConfig) -> Self {
        let backend: Arc<dyn DriveBackend> = match config.drive.backend.as_str() {
            "ros2" => Arc::new(Ros2Drive {
                topic: config.drive.ros2_topic.clone(),
            }),
            "serial" => Arc::new(SerialDrive {
                port: config.drive.serial_port.clone(),
            }),
            // "gpio" => Arc::new(GpioDrive::new(&config)), // Would use rppal
            _ => Arc::new(MockDrive),
        };

        Self {
            config,
            backend,
            last_command: Arc::new(Mutex::new(None)),
        }
    }
}

#[async_trait]
impl Tool for DriveTool {
    fn name(&self) -> &str {
        "drive"
    }

    fn description(&self) -> &str {
        "Move the robot. Supports omni-directional movement (forward, backward, strafe left/right, rotate). \
         Use 'stop' action to halt immediately. Distance is in meters, rotation in degrees."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["forward", "backward", "left", "right", "rotate_left", "rotate_right", "stop", "custom"],
                    "description": "Movement action. 'left'/'right' are strafe (omni wheels). 'rotate_*' spins in place."
                },
                "distance": {
                    "type": "number",
                    "description": "Distance in meters (for linear moves) or degrees (for rotation). Default 0.5m or 90deg."
                },
                "speed": {
                    "type": "number",
                    "description": "Speed multiplier 0.0-1.0. Default 0.5 (half speed for safety)."
                },
                "linear_x": {
                    "type": "number",
                    "description": "Custom: forward/backward velocity (-1.0 to 1.0)"
                },
                "linear_y": {
                    "type": "number",
                    "description": "Custom: left/right strafe velocity (-1.0 to 1.0)"
                },
                "angular_z": {
                    "type": "number",
                    "description": "Custom: rotation velocity (-1.0 to 1.0)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let action = args["action"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        // Safety: check max drive duration
        {
            let mut last = self.last_command.lock().await;
            if let Some(instant) = *last {
                if instant.elapsed() < Duration::from_secs(1) {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(
                            "Rate limited: wait 1 second between drive commands".to_string(),
                        ),
                    });
                }
            }
            *last = Some(std::time::Instant::now());
        }

        let speed = args["speed"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0);
        let max_speed = self.config.drive.max_speed * speed;
        let max_rotation = self.config.drive.max_rotation * speed;

        let (linear_x, linear_y, angular_z, duration_ms) = match action {
            "stop" => {
                self.backend.stop().await?;
                return Ok(ToolResult {
                    success: true,
                    output: "Robot stopped".to_string(),
                    error: None,
                });
            }
            "forward" => {
                let dist = args["distance"].as_f64().unwrap_or(0.5);
                let duration = (dist / max_speed * 1000.0) as u64;
                (
                    max_speed,
                    0.0,
                    0.0,
                    duration.min(self.config.safety.max_drive_duration * 1000),
                )
            }
            "backward" => {
                let dist = args["distance"].as_f64().unwrap_or(0.5);
                let duration = (dist / max_speed * 1000.0) as u64;
                (
                    -max_speed,
                    0.0,
                    0.0,
                    duration.min(self.config.safety.max_drive_duration * 1000),
                )
            }
            "left" => {
                let dist = args["distance"].as_f64().unwrap_or(0.5);
                let duration = (dist / max_speed * 1000.0) as u64;
                (
                    0.0,
                    max_speed,
                    0.0,
                    duration.min(self.config.safety.max_drive_duration * 1000),
                )
            }
            "right" => {
                let dist = args["distance"].as_f64().unwrap_or(0.5);
                let duration = (dist / max_speed * 1000.0) as u64;
                (
                    0.0,
                    -max_speed,
                    0.0,
                    duration.min(self.config.safety.max_drive_duration * 1000),
                )
            }
            "rotate_left" => {
                let degrees = args["distance"].as_f64().unwrap_or(90.0);
                let radians = degrees.to_radians();
                let duration = (radians / max_rotation * 1000.0) as u64;
                (
                    0.0,
                    0.0,
                    max_rotation,
                    duration.min(self.config.safety.max_drive_duration * 1000),
                )
            }
            "rotate_right" => {
                let degrees = args["distance"].as_f64().unwrap_or(90.0);
                let radians = degrees.to_radians();
                let duration = (radians / max_rotation * 1000.0) as u64;
                (
                    0.0,
                    0.0,
                    -max_rotation,
                    duration.min(self.config.safety.max_drive_duration * 1000),
                )
            }
            "custom" => {
                let lx = args["linear_x"].as_f64().unwrap_or(0.0).clamp(-1.0, 1.0) * max_speed;
                let ly = args["linear_y"].as_f64().unwrap_or(0.0).clamp(-1.0, 1.0) * max_speed;
                let az = args["angular_z"].as_f64().unwrap_or(0.0).clamp(-1.0, 1.0) * max_rotation;
                let duration = args["duration_ms"].as_u64().unwrap_or(1000);
                (
                    lx,
                    ly,
                    az,
                    duration.min(self.config.safety.max_drive_duration * 1000),
                )
            }
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Unknown action: {action}")),
                });
            }
        };

        self.backend
            .move_robot(linear_x, linear_y, angular_z, duration_ms)
            .await?;

        Ok(ToolResult {
            success: true,
            output: format!(
                "Moved: action={}, linear=({:.2}, {:.2}), angular={:.2}, duration={}ms",
                action, linear_x, linear_y, angular_z, duration_ms
            ),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_tool_name() {
        let tool = DriveTool::new(RobotConfig::default());
        assert_eq!(tool.name(), "drive");
    }

    #[test]
    fn drive_tool_schema_has_action() {
        let tool = DriveTool::new(RobotConfig::default());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
    }

    #[tokio::test]
    async fn drive_forward_mock() {
        let tool = DriveTool::new(RobotConfig::default());
        let result = tool
            .execute(json!({"action": "forward", "distance": 1.0}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("forward"));
    }

    #[tokio::test]
    async fn drive_stop() {
        let tool = DriveTool::new(RobotConfig::default());
        let result = tool.execute(json!({"action": "stop"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("stopped"));
    }

    #[tokio::test]
    async fn drive_unknown_action() {
        let tool = DriveTool::new(RobotConfig::default());
        let result = tool.execute(json!({"action": "fly"})).await.unwrap();
        assert!(!result.success);
    }
}
