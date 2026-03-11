//! Robot configuration

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Robot hardware configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RobotConfig {
    /// Communication method with motor controller
    pub drive: DriveConfig,

    /// Camera settings
    pub camera: CameraConfig,

    /// Audio settings
    pub audio: AudioConfig,

    /// Sensor settings
    pub sensors: SensorConfig,

    /// Safety limits
    pub safety: SafetyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriveConfig {
    /// "ros2", "gpio", "serial", or "mock"
    pub backend: String,

    /// ROS2 topic for cmd_vel (if using ROS2)
    pub ros2_topic: String,

    /// Serial port (if using serial)
    pub serial_port: String,

    /// Max speed in m/s
    pub max_speed: f64,

    /// Max rotation in rad/s
    pub max_rotation: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraConfig {
    /// Camera device (e.g., "/dev/video0" or "picam")
    pub device: String,

    /// Resolution
    pub width: u32,
    pub height: u32,

    /// Vision model for description ("llava", "moondream", or "none")
    pub vision_model: String,

    /// Ollama URL for vision
    pub ollama_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    /// Microphone device (ALSA name or "default")
    pub mic_device: String,

    /// Speaker device
    pub speaker_device: String,

    /// Whisper model size ("tiny", "base", "small")
    pub whisper_model: String,

    /// Path to whisper.cpp binary
    pub whisper_path: PathBuf,

    /// Path to piper binary
    pub piper_path: PathBuf,

    /// Piper voice model
    pub piper_voice: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorConfig {
    /// LIDAR device (e.g., "/dev/ttyUSB0")
    pub lidar_port: String,

    /// LIDAR type ("rplidar", "ydlidar", "mock")
    pub lidar_type: String,

    /// GPIO pins for motion sensors (BCM numbering)
    pub motion_pins: Vec<u8>,

    /// Ultrasonic sensor pins (trigger, echo)
    pub ultrasonic_pins: Option<(u8, u8)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyConfig {
    /// Minimum obstacle distance before auto-stop (meters)
    /// Robot will NOT move if obstacle is closer than this
    /// Default: 0.3m (30cm)
    pub min_obstacle_distance: f64,

    /// Slow-down zone multiplier
    /// Robot starts reducing speed when obstacle is within:
    ///   min_obstacle_distance * slow_zone_multiplier
    /// Default: 3.0 (starts slowing at 90cm if min is 30cm)
    pub slow_zone_multiplier: f64,

    /// Maximum speed when approaching obstacles (0.0 - 1.0)
    /// Limits speed in the slow-down zone
    /// Default: 0.3 (30% max speed near obstacles)
    pub approach_speed_limit: f64,

    /// Maximum continuous drive time (seconds)
    /// Robot auto-stops after this duration without new commands
    /// Prevents runaway if LLM hangs or loses connection
    /// Default: 30 seconds
    pub max_drive_duration: u64,

    /// Emergency stop GPIO pin (BCM numbering)
    /// Wire a big red button - pulling LOW triggers immediate stop
    /// Default: GPIO 4
    pub estop_pin: Option<u8>,

    /// Bump sensor GPIO pins (BCM numbering)
    /// Microswitches on chassis that trigger on physical collision
    /// Default: [5, 6] (front-left, front-right)
    pub bump_sensor_pins: Vec<u8>,

    /// Distance to reverse after bump detection (meters)
    /// Robot backs up this far after hitting something
    /// Default: 0.15m (15cm)
    pub bump_reverse_distance: f64,

    /// Require verbal confirmation for movement
    /// If true, robot asks "Should I move?" before moving
    /// Default: false (for responsive play)
    pub confirm_movement: bool,

    /// Enable collision prediction using LIDAR
    /// Estimates if current trajectory will intersect obstacle
    /// Default: true
    pub predict_collisions: bool,

    /// Sensor data timeout (seconds)
    /// Block all movement if no sensor updates for this long
    /// Prevents blind movement if sensors fail
    /// Default: 5 seconds
    pub sensor_timeout_secs: u64,

    /// Speed limit when sensors are in mock/unavailable mode (0.0 - 1.0)
    /// Extra caution when flying blind
    /// Default: 0.2 (20% speed)
    pub blind_mode_speed_limit: f64,
}

impl Default for RobotConfig {
    fn default() -> Self {
        Self {
            drive: DriveConfig {
                backend: "mock".to_string(),
                ros2_topic: "/cmd_vel".to_string(),
                serial_port: "/dev/ttyACM0".to_string(),
                max_speed: 0.5,
                max_rotation: 1.0,
            },
            camera: CameraConfig {
                device: "/dev/video0".to_string(),
                width: 640,
                height: 480,
                vision_model: "moondream".to_string(),
                ollama_url: "http://localhost:11434".to_string(),
            },
            audio: AudioConfig {
                mic_device: "default".to_string(),
                speaker_device: "default".to_string(),
                whisper_model: "base".to_string(),
                whisper_path: PathBuf::from("/usr/local/bin/whisper-cpp"),
                piper_path: PathBuf::from("/usr/local/bin/piper"),
                piper_voice: "en_US-lessac-medium".to_string(),
            },
            sensors: SensorConfig {
                lidar_port: "/dev/ttyUSB0".to_string(),
                lidar_type: "mock".to_string(),
                motion_pins: vec![17, 27],
                ultrasonic_pins: Some((23, 24)),
            },
            safety: SafetyConfig {
                min_obstacle_distance: 0.3,   // 30cm - absolute minimum
                slow_zone_multiplier: 3.0,    // Start slowing at 90cm
                approach_speed_limit: 0.3,    // 30% max speed near obstacles
                max_drive_duration: 30,       // Auto-stop after 30s
                estop_pin: Some(4),           // GPIO 4 for big red button
                bump_sensor_pins: vec![5, 6], // Front bump sensors
                bump_reverse_distance: 0.15,  // Back up 15cm after bump
                confirm_movement: false,      // Don't require verbal confirm
                predict_collisions: true,     // Use LIDAR prediction
                sensor_timeout_secs: 5,       // Block if sensors stale 5s
                blind_mode_speed_limit: 0.2,  // 20% speed without sensors
            },
        }
    }
}

impl RobotConfig {
    /// Load from TOML file
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    /// Save to TOML file
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}
