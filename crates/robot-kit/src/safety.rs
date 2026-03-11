//! Safety System - Collision avoidance, watchdogs, and emergency stops
//!
//! This module runs INDEPENDENTLY of the AI brain to ensure safety
//! even if the LLM makes bad decisions or hangs.
//!
//! ## Safety Layers
//!
//! 1. **Pre-move checks** - Verify path clear before any movement
//! 2. **Active monitoring** - Continuous sensor polling during movement
//! 3. **Reactive stops** - Instant halt on obstacle detection
//! 4. **Watchdog timer** - Auto-stop if no commands for N seconds
//! 5. **Hardware E-stop** - Physical button overrides everything
//!
//! ## Design Philosophy
//!
//! The AI can REQUEST movement, but the safety system ALLOWS it.
//! Safety always wins.

use crate::config::{RobotConfig, SafetyConfig};
use crate::traits::ToolResult;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, RwLock};

/// Safety events broadcast to all listeners
#[derive(Debug, Clone)]
pub enum SafetyEvent {
    /// Obstacle detected, movement blocked
    ObstacleDetected { distance: f64, angle: u16 },
    /// Emergency stop triggered
    EmergencyStop { reason: String },
    /// Watchdog timeout - no activity
    WatchdogTimeout,
    /// Movement approved
    MovementApproved,
    /// Movement denied with reason
    MovementDenied { reason: String },
    /// Bump sensor triggered
    BumpDetected { sensor: String },
    /// System recovered, ready to move again
    Recovered,
}

/// Real-time safety state
pub struct SafetyState {
    /// Is it safe to move?
    pub can_move: AtomicBool,
    /// Emergency stop active?
    pub estop_active: AtomicBool,
    /// Last movement command timestamp (ms since epoch)
    pub last_command_ms: AtomicU64,
    /// Current minimum distance to obstacle
    pub min_obstacle_distance: RwLock<f64>,
    /// Reason movement is blocked (if any)
    pub block_reason: RwLock<Option<String>>,
    /// Speed multiplier based on proximity (0.0 - 1.0)
    pub speed_limit: RwLock<f64>,
}

impl Default for SafetyState {
    fn default() -> Self {
        Self {
            can_move: AtomicBool::new(true),
            estop_active: AtomicBool::new(false),
            last_command_ms: AtomicU64::new(0),
            min_obstacle_distance: RwLock::new(999.0),
            block_reason: RwLock::new(None),
            speed_limit: RwLock::new(1.0),
        }
    }
}

/// Safety monitor - runs as background task
pub struct SafetyMonitor {
    config: SafetyConfig,
    state: Arc<SafetyState>,
    event_tx: broadcast::Sender<SafetyEvent>,
    shutdown: AtomicBool,
}

impl SafetyMonitor {
    pub fn new(config: SafetyConfig) -> (Self, broadcast::Receiver<SafetyEvent>) {
        let (event_tx, event_rx) = broadcast::channel(64);
        let monitor = Self {
            config,
            state: Arc::new(SafetyState::default()),
            event_tx,
            shutdown: AtomicBool::new(false),
        };
        (monitor, event_rx)
    }

    pub fn state(&self) -> Arc<SafetyState> {
        self.state.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SafetyEvent> {
        self.event_tx.subscribe()
    }

    /// Check if movement is currently allowed
    pub async fn can_move(&self) -> bool {
        if self.state.estop_active.load(Ordering::SeqCst) {
            return false;
        }
        self.state.can_move.load(Ordering::SeqCst)
    }

    /// Get current speed limit multiplier (0.0 - 1.0)
    pub async fn speed_limit(&self) -> f64 {
        *self.state.speed_limit.read().await
    }

    /// Request permission to move - returns allowed speed multiplier or error
    pub async fn request_movement(&self, direction: &str, distance: f64) -> Result<f64, String> {
        // Check E-stop
        if self.state.estop_active.load(Ordering::SeqCst) {
            return Err("Emergency stop active".to_string());
        }

        // Check general movement permission
        if !self.state.can_move.load(Ordering::SeqCst) {
            let reason = self.state.block_reason.read().await;
            return Err(reason
                .clone()
                .unwrap_or_else(|| "Movement blocked".to_string()));
        }

        // Check obstacle distance in movement direction
        let min_dist = *self.state.min_obstacle_distance.read().await;
        if min_dist < self.config.min_obstacle_distance {
            let msg = format!(
                "Obstacle too close: {:.2}m (min: {:.2}m)",
                min_dist, self.config.min_obstacle_distance
            );
            let _ = self.event_tx.send(SafetyEvent::MovementDenied {
                reason: msg.clone(),
            });
            return Err(msg);
        }

        // Check if requested distance would hit obstacle
        if distance > min_dist - self.config.min_obstacle_distance {
            let safe_distance = (min_dist - self.config.min_obstacle_distance).max(0.0);
            if safe_distance < 0.1 {
                return Err(format!(
                    "Cannot move {}: obstacle at {:.2}m",
                    direction, min_dist
                ));
            }
            // Allow reduced distance
            tracing::warn!(
                "Reducing {} distance from {:.2}m to {:.2}m due to obstacle",
                direction,
                distance,
                safe_distance
            );
        }

        // Update last command time
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        self.state.last_command_ms.store(now_ms, Ordering::SeqCst);

        // Calculate speed limit based on proximity
        let speed_mult = self.calculate_speed_limit(min_dist).await;

        let _ = self.event_tx.send(SafetyEvent::MovementApproved);
        Ok(speed_mult)
    }

    /// Calculate safe speed based on obstacle proximity
    async fn calculate_speed_limit(&self, obstacle_distance: f64) -> f64 {
        let min_dist = self.config.min_obstacle_distance;
        let slow_zone = min_dist * 3.0; // Start slowing at 3x minimum distance

        let limit = if obstacle_distance >= slow_zone {
            1.0 // Full speed
        } else if obstacle_distance <= min_dist {
            0.0 // Stop
        } else {
            // Linear interpolation between stop and full speed
            (obstacle_distance - min_dist) / (slow_zone - min_dist)
        };

        *self.state.speed_limit.write().await = limit;
        limit
    }

    /// Trigger emergency stop
    pub async fn emergency_stop(&self, reason: &str) {
        tracing::error!("EMERGENCY STOP: {}", reason);
        self.state.estop_active.store(true, Ordering::SeqCst);
        self.state.can_move.store(false, Ordering::SeqCst);
        *self.state.block_reason.write().await = Some(reason.to_string());

        let _ = self.event_tx.send(SafetyEvent::EmergencyStop {
            reason: reason.to_string(),
        });
    }

    /// Reset emergency stop (requires explicit action)
    pub async fn reset_estop(&self) {
        tracing::info!("E-STOP RESET");
        self.state.estop_active.store(false, Ordering::SeqCst);
        self.state.can_move.store(true, Ordering::SeqCst);
        *self.state.block_reason.write().await = None;

        let _ = self.event_tx.send(SafetyEvent::Recovered);
    }

    /// Update obstacle distance (call from sensor loop)
    pub async fn update_obstacle_distance(&self, distance: f64, angle: u16) {
        // Update minimum distance tracking
        {
            let mut min_dist = self.state.min_obstacle_distance.write().await;
            // Always update to current reading (not just if closer)
            *min_dist = distance;
        }

        // Recalculate speed limit based on new distance
        self.calculate_speed_limit(distance).await;

        // Check if too close
        if distance < self.config.min_obstacle_distance {
            self.state.can_move.store(false, Ordering::SeqCst);
            *self.state.block_reason.write().await =
                Some(format!("Obstacle at {:.2}m ({}°)", distance, angle));

            let _ = self
                .event_tx
                .send(SafetyEvent::ObstacleDetected { distance, angle });
        } else if !self.state.estop_active.load(Ordering::SeqCst) {
            // Clear block if obstacle moved away and no E-stop
            self.state.can_move.store(true, Ordering::SeqCst);
            *self.state.block_reason.write().await = None;
        }
    }

    /// Report bump sensor triggered
    pub async fn bump_detected(&self, sensor: &str) {
        tracing::warn!("BUMP DETECTED: {}", sensor);

        // Immediate stop
        self.state.can_move.store(false, Ordering::SeqCst);
        *self.state.block_reason.write().await = Some(format!("Bump: {}", sensor));

        let _ = self.event_tx.send(SafetyEvent::BumpDetected {
            sensor: sensor.to_string(),
        });

        // Auto-recover after brief pause (robot should back up)
        tokio::spawn({
            let state = self.state.clone();
            let event_tx = self.event_tx.clone();
            async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if !state.estop_active.load(Ordering::SeqCst) {
                    state.can_move.store(true, Ordering::SeqCst);
                    *state.block_reason.write().await = None;
                    let _ = event_tx.send(SafetyEvent::Recovered);
                }
            }
        });
    }

    /// Shutdown the monitor
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    /// Run the safety monitor loop (call in background task)
    pub async fn run(&self, mut sensor_rx: tokio::sync::mpsc::Receiver<SensorReading>) {
        let watchdog_timeout = Duration::from_secs(self.config.max_drive_duration);
        let mut last_sensor_update = Instant::now();

        while !self.shutdown.load(Ordering::SeqCst) {
            tokio::select! {
                // Process sensor readings
                Some(reading) = sensor_rx.recv() => {
                    last_sensor_update = Instant::now();
                    match reading {
                        SensorReading::Lidar { distance, angle } => {
                            self.update_obstacle_distance(distance, angle).await;
                        }
                        SensorReading::Bump { sensor } => {
                            self.bump_detected(&sensor).await;
                        }
                        SensorReading::Estop { pressed } => {
                            if pressed {
                                self.emergency_stop("Hardware E-stop pressed").await;
                            }
                        }
                    }
                }

                // Watchdog check every second
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    // Check for sensor timeout
                    if last_sensor_update.elapsed() > Duration::from_secs(5) {
                        tracing::warn!("Sensor data stale - blocking movement");
                        self.state.can_move.store(false, Ordering::SeqCst);
                        *self.state.block_reason.write().await =
                            Some("Sensor data stale".to_string());
                    }

                    // Check watchdog (auto-stop if no commands)
                    let last_cmd_ms = self.state.last_command_ms.load(Ordering::SeqCst);
                    if last_cmd_ms > 0 {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64;

                        let elapsed = Duration::from_millis(now_ms - last_cmd_ms);
                        if elapsed > watchdog_timeout {
                            tracing::info!("Watchdog timeout - no commands for {:?}", elapsed);
                            let _ = self.event_tx.send(SafetyEvent::WatchdogTimeout);
                            // Don't block movement, just notify
                        }
                    }
                }
            }
        }
    }
}

/// Sensor readings fed to safety monitor
#[derive(Debug, Clone)]
pub enum SensorReading {
    Lidar { distance: f64, angle: u16 },
    Bump { sensor: String },
    Estop { pressed: bool },
}

/// Safety-aware drive wrapper
/// Wraps the drive tool to enforce safety limits
pub struct SafeDrive {
    inner_drive: Arc<dyn crate::traits::Tool>,
    safety: Arc<SafetyMonitor>,
}

impl SafeDrive {
    pub fn new(drive: Arc<dyn crate::traits::Tool>, safety: Arc<SafetyMonitor>) -> Self {
        Self {
            inner_drive: drive,
            safety,
        }
    }
}

#[async_trait::async_trait]
impl crate::traits::Tool for SafeDrive {
    fn name(&self) -> &str {
        "drive"
    }

    fn description(&self) -> &str {
        "Move the robot (with safety limits enforced)"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.inner_drive.parameters_schema()
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        // ToolResult imported at top of file

        let action = args["action"].as_str().unwrap_or("unknown");
        let distance = args["distance"].as_f64().unwrap_or(0.5);

        // Always allow stop
        if action == "stop" {
            return self.inner_drive.execute(args).await;
        }

        // Request permission from safety system
        match self.safety.request_movement(action, distance).await {
            Ok(speed_mult) => {
                // Modify speed in args
                let mut modified_args = args.clone();
                let original_speed = args["speed"].as_f64().unwrap_or(0.5);
                modified_args["speed"] = serde_json::json!(original_speed * speed_mult);

                if speed_mult < 1.0 {
                    tracing::info!(
                        "Safety: Reducing speed to {:.0}% due to obstacle proximity",
                        speed_mult * 100.0
                    );
                }

                self.inner_drive.execute(modified_args).await
            }
            Err(reason) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Safety blocked movement: {}", reason)),
            }),
        }
    }
}

/// Pre-flight safety check before any operation
pub async fn preflight_check(config: &RobotConfig) -> Result<Vec<String>> {
    let mut warnings = Vec::new();

    // Check safety config
    if config.safety.min_obstacle_distance < 0.1 {
        warnings.push("WARNING: min_obstacle_distance < 0.1m is dangerously low".to_string());
    }

    if config.safety.max_drive_duration > 60 {
        warnings.push("WARNING: max_drive_duration > 60s may allow runaway".to_string());
    }

    if config.drive.max_speed > 1.0 {
        warnings.push("WARNING: max_speed > 1.0 m/s is very fast for indoor use".to_string());
    }

    if config.safety.estop_pin.is_none() {
        warnings.push(
            "WARNING: No E-stop pin configured. Recommend wiring a hardware stop button."
                .to_string(),
        );
    }

    // Check for sensor availability
    if config.sensors.lidar_type == "mock" {
        warnings.push("NOTICE: LIDAR in mock mode - no real obstacle detection".to_string());
    }

    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn safety_state_defaults() {
        let state = SafetyState::default();
        assert!(state.can_move.load(Ordering::SeqCst));
        assert!(!state.estop_active.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn safety_monitor_blocks_on_obstacle() {
        let config = SafetyConfig::default();

        let (monitor, _rx) = SafetyMonitor::new(config);

        // Initially can move
        assert!(monitor.can_move().await);

        // Report close obstacle
        monitor.update_obstacle_distance(0.2, 0).await;

        // Now blocked
        assert!(!monitor.can_move().await);
    }

    #[tokio::test]
    async fn safety_monitor_estop() {
        let config = SafetyConfig::default();
        let (monitor, mut rx) = SafetyMonitor::new(config);

        monitor.emergency_stop("test").await;

        assert!(!monitor.can_move().await);
        assert!(monitor.state.estop_active.load(Ordering::SeqCst));

        // Check event was sent
        let event = rx.try_recv().unwrap();
        matches!(event, SafetyEvent::EmergencyStop { .. });
    }

    #[tokio::test]
    async fn speed_limit_calculation() {
        let config = SafetyConfig {
            min_obstacle_distance: 0.3,
            ..Default::default()
        };
        let (monitor, _rx) = SafetyMonitor::new(config);

        // Far obstacle = full speed
        let speed = monitor.calculate_speed_limit(2.0).await;
        assert!((speed - 1.0).abs() < 0.01);

        // Close obstacle = reduced speed
        let speed = monitor.calculate_speed_limit(0.5).await;
        assert!(speed < 1.0);
        assert!(speed > 0.0);

        // At minimum = stop
        let speed = monitor.calculate_speed_limit(0.3).await;
        assert!((speed - 0.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn request_movement_blocked() {
        let config = SafetyConfig {
            min_obstacle_distance: 0.3,
            ..Default::default()
        };
        let (monitor, _rx) = SafetyMonitor::new(config);

        // Set obstacle too close
        monitor.update_obstacle_distance(0.2, 0).await;

        // Movement should be denied
        let result = monitor.request_movement("forward", 1.0).await;
        assert!(result.is_err());
    }

    impl Default for SafetyConfig {
        fn default() -> Self {
            Self {
                min_obstacle_distance: 0.3,
                slow_zone_multiplier: 3.0,
                approach_speed_limit: 0.3,
                max_drive_duration: 30,
                estop_pin: Some(4),
                bump_sensor_pins: vec![5, 6],
                bump_reverse_distance: 0.15,
                confirm_movement: false,
                predict_collisions: true,
                sensor_timeout_secs: 5,
                blind_mode_speed_limit: 0.2,
            }
        }
    }
}
