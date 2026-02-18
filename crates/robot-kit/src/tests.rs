//! Integration tests for robot kit
//!
//! These tests verify the robot kit works correctly in various configurations:
//! - Mock mode (no hardware) - for CI/development
//! - Hardware simulation - for testing real scenarios
//! - Live hardware - for on-device validation

#[cfg(test)]
mod unit_tests {
    use crate::config::RobotConfig;
    use crate::traits::Tool;
    use crate::{DriveTool, EmoteTool, ListenTool, LookTool, SenseTool, SpeakTool};
    use serde_json::json;

    // =========================================================================
    // TOOL TRAIT COMPLIANCE
    // =========================================================================

    #[test]
    fn all_tools_have_valid_names() {
        let config = RobotConfig::default();
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(DriveTool::new(config.clone())),
            Box::new(LookTool::new(config.clone())),
            Box::new(ListenTool::new(config.clone())),
            Box::new(SpeakTool::new(config.clone())),
            Box::new(SenseTool::new(config.clone())),
            Box::new(EmoteTool::new(config.clone())),
        ];

        for tool in &tools {
            assert!(!tool.name().is_empty(), "Tool name should not be empty");
            assert!(
                tool.name().chars().all(|c| c.is_alphanumeric() || c == '_'),
                "Tool name '{}' should be alphanumeric",
                tool.name()
            );
        }
    }

    #[test]
    fn all_tools_have_descriptions() {
        let config = RobotConfig::default();
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(DriveTool::new(config.clone())),
            Box::new(LookTool::new(config.clone())),
            Box::new(ListenTool::new(config.clone())),
            Box::new(SpeakTool::new(config.clone())),
            Box::new(SenseTool::new(config.clone())),
            Box::new(EmoteTool::new(config.clone())),
        ];

        for tool in &tools {
            assert!(
                tool.description().len() > 10,
                "Tool '{}' needs a meaningful description",
                tool.name()
            );
        }
    }

    #[test]
    fn all_tools_have_valid_schemas() {
        let config = RobotConfig::default();
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(DriveTool::new(config.clone())),
            Box::new(LookTool::new(config.clone())),
            Box::new(ListenTool::new(config.clone())),
            Box::new(SpeakTool::new(config.clone())),
            Box::new(SenseTool::new(config.clone())),
            Box::new(EmoteTool::new(config.clone())),
        ];

        for tool in &tools {
            let schema = tool.parameters_schema();
            assert!(
                schema.is_object(),
                "Tool '{}' schema should be an object",
                tool.name()
            );
            assert!(
                schema.get("type").is_some(),
                "Tool '{}' schema should have 'type' field",
                tool.name()
            );
        }
    }

    // =========================================================================
    // DRIVE TOOL TESTS
    // =========================================================================

    #[tokio::test]
    async fn drive_forward_mock() {
        let config = RobotConfig::default();
        let tool = DriveTool::new(config);

        let result = tool
            .execute(json!({"action": "forward", "distance": 1.0}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("forward"));
    }

    #[tokio::test]
    async fn drive_stop_always_succeeds() {
        let config = RobotConfig::default();
        let tool = DriveTool::new(config);

        let result = tool.execute(json!({"action": "stop"})).await.unwrap();

        assert!(result.success);
        assert!(result.output.to_lowercase().contains("stop"));
    }

    #[tokio::test]
    async fn drive_strafe_left() {
        let config = RobotConfig::default();
        let tool = DriveTool::new(config);

        let result = tool
            .execute(json!({"action": "left", "distance": 0.5}))
            .await
            .unwrap();

        assert!(result.success);
    }

    #[tokio::test]
    async fn drive_rotate() {
        let config = RobotConfig::default();
        let tool = DriveTool::new(config);

        let result = tool
            .execute(json!({"action": "rotate_left", "distance": 90.0}))
            .await
            .unwrap();

        assert!(result.success);
    }

    #[tokio::test]
    async fn drive_invalid_action_fails() {
        let config = RobotConfig::default();
        let tool = DriveTool::new(config);

        let result = tool.execute(json!({"action": "fly"})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn drive_missing_action_fails() {
        let config = RobotConfig::default();
        let tool = DriveTool::new(config);

        let result = tool.execute(json!({})).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn drive_speed_clamped() {
        let config = RobotConfig::default();
        let tool = DriveTool::new(config);

        // Speed > 1.0 should be clamped
        let result = tool
            .execute(json!({"action": "forward", "speed": 5.0}))
            .await
            .unwrap();

        assert!(result.success);
    }

    // =========================================================================
    // SENSE TOOL TESTS
    // =========================================================================

    #[tokio::test]
    async fn sense_scan_returns_distances() {
        let config = RobotConfig::default();
        let tool = SenseTool::new(config);

        let result = tool
            .execute(json!({"action": "scan", "direction": "all"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("Forward"));
        assert!(result.output.contains("Left"));
        assert!(result.output.contains("Right"));
    }

    #[tokio::test]
    async fn sense_clear_ahead_check() {
        let config = RobotConfig::default();
        let tool = SenseTool::new(config);

        let result = tool
            .execute(json!({"action": "clear_ahead"}))
            .await
            .unwrap();

        assert!(result.success);
        // Mock should report clear or blocked
        assert!(result.output.contains("CLEAR") || result.output.contains("BLOCKED"));
    }

    #[tokio::test]
    async fn sense_motion_detection() {
        let config = RobotConfig::default();
        let tool = SenseTool::new(config);

        let result = tool.execute(json!({"action": "motion"})).await.unwrap();

        assert!(result.success);
    }

    // =========================================================================
    // EMOTE TOOL TESTS
    // =========================================================================

    #[tokio::test]
    async fn emote_happy() {
        let config = RobotConfig::default();
        let tool = EmoteTool::new(config);

        let result = tool
            .execute(json!({"expression": "happy", "duration": 0}))
            .await
            .unwrap();

        assert!(result.success);
    }

    #[tokio::test]
    async fn emote_all_expressions_valid() {
        let config = RobotConfig::default();
        let tool = EmoteTool::new(config);

        let expressions = [
            "happy",
            "sad",
            "surprised",
            "thinking",
            "sleepy",
            "excited",
            "love",
            "angry",
            "confused",
            "wink",
        ];

        for expr in expressions {
            let result = tool
                .execute(json!({"expression": expr, "duration": 0}))
                .await
                .unwrap();

            assert!(result.success, "Expression '{}' should succeed", expr);
        }
    }

    #[tokio::test]
    async fn emote_invalid_expression_fails() {
        let config = RobotConfig::default();
        let tool = EmoteTool::new(config);

        let result = tool.execute(json!({"expression": "nonexistent"})).await;

        assert!(result.is_err());
    }

    // =========================================================================
    // CONFIG TESTS
    // =========================================================================

    #[test]
    fn config_default_is_safe() {
        let config = RobotConfig::default();

        // Safety defaults should be conservative
        assert!(config.safety.min_obstacle_distance >= 0.2);
        assert!(config.safety.max_drive_duration <= 60);
        assert!(config.drive.max_speed <= 1.0);
        assert!(config.safety.blind_mode_speed_limit <= 0.3);
    }

    #[test]
    fn config_serializes_to_toml() {
        let config = RobotConfig::default();
        let toml = toml::to_string(&config);

        assert!(toml.is_ok());
    }

    #[test]
    fn config_roundtrips() {
        let config = RobotConfig::default();
        let toml = toml::to_string(&config).unwrap();
        let parsed: RobotConfig = toml::from_str(&toml).unwrap();

        assert_eq!(config.drive.max_speed, parsed.drive.max_speed);
        assert_eq!(
            config.safety.min_obstacle_distance,
            parsed.safety.min_obstacle_distance
        );
    }
}

#[cfg(test)]
#[cfg(feature = "safety")]
mod safety_tests {
    use crate::config::SafetyConfig;
    use crate::safety::{SafetyEvent, SafetyMonitor};
    use std::sync::atomic::Ordering;

    fn test_safety_config() -> SafetyConfig {
        SafetyConfig {
            min_obstacle_distance: 0.3,
            slow_zone_multiplier: 3.0,
            approach_speed_limit: 0.3,
            max_drive_duration: 30,
            estop_pin: None,
            bump_sensor_pins: vec![],
            bump_reverse_distance: 0.15,
            confirm_movement: false,
            predict_collisions: true,
            sensor_timeout_secs: 5,
            blind_mode_speed_limit: 0.2,
        }
    }

    #[tokio::test]
    async fn safety_initially_allows_movement() {
        let config = test_safety_config();
        let (monitor, _rx) = SafetyMonitor::new(config);

        assert!(monitor.can_move().await);
    }

    #[tokio::test]
    async fn safety_blocks_on_close_obstacle() {
        let config = test_safety_config();
        let (monitor, _rx) = SafetyMonitor::new(config);

        // Report obstacle at 0.2m (below 0.3m threshold)
        monitor.update_obstacle_distance(0.2, 0).await;

        assert!(!monitor.can_move().await);
    }

    #[tokio::test]
    async fn safety_allows_after_obstacle_clears() {
        let config = test_safety_config();
        let (monitor, _rx) = SafetyMonitor::new(config);

        // Block
        monitor.update_obstacle_distance(0.2, 0).await;
        assert!(!monitor.can_move().await);

        // Clear
        monitor.update_obstacle_distance(1.0, 0).await;
        assert!(monitor.can_move().await);
    }

    #[tokio::test]
    async fn safety_estop_blocks_everything() {
        let config = test_safety_config();
        let (monitor, mut rx) = SafetyMonitor::new(config);

        monitor.emergency_stop("test").await;

        assert!(!monitor.can_move().await);
        assert!(monitor.state().estop_active.load(Ordering::SeqCst));

        // Check event was broadcast
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, SafetyEvent::EmergencyStop { .. }));
    }

    #[tokio::test]
    async fn safety_estop_reset() {
        let config = test_safety_config();
        let (monitor, _rx) = SafetyMonitor::new(config);

        monitor.emergency_stop("test").await;
        assert!(!monitor.can_move().await);

        monitor.reset_estop().await;
        assert!(monitor.can_move().await);
    }

    #[tokio::test]
    async fn safety_speed_limit_far() {
        let config = test_safety_config();
        let (monitor, _rx) = SafetyMonitor::new(config);

        // Far obstacle = full speed
        monitor.update_obstacle_distance(2.0, 0).await;
        let limit = monitor.speed_limit().await;

        assert!((limit - 1.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn safety_speed_limit_approaching() {
        let config = test_safety_config();
        let (monitor, _rx) = SafetyMonitor::new(config);

        // In slow zone (0.3 * 3.0 = 0.9m)
        monitor.update_obstacle_distance(0.5, 0).await;
        let limit = monitor.speed_limit().await;

        assert!(limit < 1.0);
        assert!(limit > 0.0);
    }

    #[tokio::test]
    async fn safety_movement_request_approved() {
        let config = test_safety_config();
        let (monitor, _rx) = SafetyMonitor::new(config);

        // Far obstacle
        monitor.update_obstacle_distance(2.0, 0).await;

        let result = monitor.request_movement("forward", 1.0).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn safety_movement_request_denied_close() {
        let config = test_safety_config();
        let (monitor, _rx) = SafetyMonitor::new(config);

        // Close obstacle
        monitor.update_obstacle_distance(0.2, 0).await;

        let result = monitor.request_movement("forward", 1.0).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn safety_bump_triggers_stop() {
        let config = test_safety_config();
        let (monitor, mut rx) = SafetyMonitor::new(config);

        monitor.bump_detected("front_left").await;

        assert!(!monitor.can_move().await);

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, SafetyEvent::BumpDetected { .. }));
    }
}

#[cfg(test)]
mod integration_tests {
    use crate::config::RobotConfig;
    use crate::traits::Tool;
    use crate::{create_tools, DriveTool, SenseTool};
    use serde_json::json;

    #[tokio::test]
    async fn drive_then_sense_workflow() {
        let config = RobotConfig::default();
        let drive = DriveTool::new(config.clone());
        let sense = SenseTool::new(config);

        // Check ahead
        let scan = sense
            .execute(json!({"action": "clear_ahead"}))
            .await
            .unwrap();
        assert!(scan.success);

        // Move if clear
        if scan.output.contains("CLEAR") {
            let drive_result = drive
                .execute(json!({"action": "forward", "distance": 0.5}))
                .await
                .unwrap();
            assert!(drive_result.success);

            // Wait for rate limiter (drive tool has 1 second cooldown)
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        }

        // Stop
        let stop = drive.execute(json!({"action": "stop"})).await.unwrap();
        assert!(stop.success);
    }

    #[tokio::test]
    async fn create_tools_returns_all_tools() {
        let config = RobotConfig::default();
        let tools = create_tools(&config);

        assert_eq!(tools.len(), 6);

        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"drive"));
        assert!(names.contains(&"look"));
        assert!(names.contains(&"listen"));
        assert!(names.contains(&"speak"));
        assert!(names.contains(&"sense"));
        assert!(names.contains(&"emote"));
    }

    #[cfg(feature = "safety")]
    #[tokio::test]
    async fn safe_drive_blocks_on_obstacle() {
        use crate::safety::SafetyMonitor;
        use crate::SafeDrive;
        use std::sync::Arc;

        let config = RobotConfig::default();
        let (safety_monitor, _rx) = SafetyMonitor::new(config.safety.clone());
        let safety = Arc::new(safety_monitor);

        // Report close obstacle
        safety.update_obstacle_distance(0.2, 0).await;

        let drive = Arc::new(DriveTool::new(config));
        let safe_drive = SafeDrive::new(drive, safety);

        let result = safe_drive
            .execute(json!({"action": "forward", "distance": 1.0}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("Safety"));
    }
}
