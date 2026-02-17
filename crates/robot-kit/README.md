# ZeroClaw Robot Kit

A complete toolkit for building AI-powered robots with ZeroClaw. Designed for Raspberry Pi deployment with offline Ollama inference.

## Features

| Tool | Description |
|------|-------------|
| `drive` | Omni-directional movement (forward, strafe, rotate) |
| `look` | Camera capture + vision model description |
| `listen` | Speech-to-text via Whisper.cpp |
| `speak` | Text-to-speech via Piper TTS |
| `sense` | LIDAR, motion sensors, ultrasonic distance |
| `emote` | LED expressions and sound effects |

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                 ZeroClaw + Ollama                       │
│              (High-Level AI Brain)                      │
└─────────────────────┬───────────────────────────────────┘
                      │
        ┌─────────────┼─────────────┐
        ▼             ▼             ▼
   ┌─────────┐  ┌──────────┐  ┌──────────┐
   │ drive   │  │  look    │  │  speak   │
   │ sense   │  │  listen  │  │  emote   │
   └────┬────┘  └────┬─────┘  └────┬─────┘
        │            │             │
        ▼            ▼             ▼
   ┌─────────────────────────────────────┐
   │        Hardware Layer               │
   │  Motors, Camera, Mic, Speaker, LEDs │
   └─────────────────────────────────────┘
```

## Hardware Requirements

### Minimum
- Raspberry Pi 4 (4GB) or Pi 5
- USB webcam
- USB microphone
- Speaker with amp
- Motor controller (L298N, TB6612, etc.)
- 4 DC motors + omni wheels

### Recommended
- Raspberry Pi 5 (8GB)
- RPLidar A1 for obstacle avoidance
- LED matrix (8x8) for expressions
- PIR motion sensors
- HC-SR04 ultrasonic sensor

## Software Dependencies

```bash
# Install on Raspberry Pi OS

# Audio
sudo apt install alsa-utils pulseaudio

# Camera
sudo apt install ffmpeg fswebcam

# Ollama (local LLM)
curl -fsSL https://ollama.ai/install.sh | sh
ollama pull llama3
ollama pull moondream  # Vision model

# Whisper.cpp (speech-to-text)
git clone https://github.com/ggerganov/whisper.cpp
cd whisper.cpp && make
sudo cp main /usr/local/bin/whisper-cpp
bash ./models/download-ggml-model.sh base

# Piper TTS (text-to-speech)
pip install piper-tts
# Or download binary from github.com/rhasspy/piper/releases

# ROS2 (optional, for advanced robotics)
# See: docs.ros.org/en/humble/Installation.html
```

## Quick Start

### 1. Build ZeroClaw with robot tools

```bash
# Clone and build
git clone https://github.com/your/zeroclaw
cd zeroclaw
cargo build --release

# Copy robot kit to src/tools/
cp -r examples/robot_kit src/tools/
# Add to src/tools/mod.rs (see Integration section)
```

### 2. Configure

```bash
# Copy config
mkdir -p ~/.zeroclaw
cp examples/robot_kit/robot.toml ~/.zeroclaw/
cp examples/robot_kit/SOUL.md ~/.zeroclaw/workspace/

# Edit for your hardware
nano ~/.zeroclaw/robot.toml
```

### 3. Test

```bash
# Start Ollama
ollama serve &

# Test in mock mode
./target/release/zeroclaw agent -m "Say hello and show a happy face"

# Test with real hardware
# (after configuring robot.toml)
./target/release/zeroclaw agent -m "Move forward 1 meter"
```

## Integration

Add to `src/tools/mod.rs`:

```rust
mod robot_kit;

pub fn robot_tools(config: &RobotConfig) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(robot_kit::DriveTool::new(config.clone())),
        Arc::new(robot_kit::LookTool::new(config.clone())),
        Arc::new(robot_kit::ListenTool::new(config.clone())),
        Arc::new(robot_kit::SpeakTool::new(config.clone())),
        Arc::new(robot_kit::SenseTool::new(config.clone())),
        Arc::new(robot_kit::EmoteTool::new(config.clone())),
    ]
}
```

## Usage Examples

### Play Hide and Seek

```
User: Let's play hide and seek!
Robot:
  1. emote(expression="excited")
  2. speak(text="Okay! I'll count to 20. Go hide!")
  3. [waits 20 seconds]
  4. speak(text="Ready or not, here I come!")
  5. sense(action="scan")
  6. drive(action="forward", distance=1)
  7. look(action="find", prompt="a child hiding")
  ...
```

### Patrol Mode

```
User: Patrol the living room
Robot:
  1. sense(action="scan", direction="all")
  2. drive(action="forward", distance=2)
  3. sense(action="motion")
  4. look(action="describe")
  5. [repeat]
```

### Interactive Conversation

```
User: [speaks] "Hey Buddy, what do you see?"
Robot:
  1. listen(duration=5) → "Hey Buddy, what do you see?"
  2. look(action="describe")
  3. speak(text="I see a couch, a TV, and some toys on the floor!")
  4. emote(expression="happy")
```

## Creating a Bootable USB Tarball

```bash
# Package everything needed
mkdir zeroclaw-robot-kit
cp -r target/release/zeroclaw zeroclaw-robot-kit/
cp -r examples/robot_kit zeroclaw-robot-kit/
cp -r ~/.zeroclaw zeroclaw-robot-kit/dot-zeroclaw

# Include models
mkdir -p zeroclaw-robot-kit/models
cp ~/.zeroclaw/models/ggml-base.bin zeroclaw-robot-kit/models/
# Note: Ollama models are large, may want to download on target

# Create tarball
tar -czvf zeroclaw-robot-kit.tar.gz zeroclaw-robot-kit/

# Copy to USB
cp zeroclaw-robot-kit.tar.gz /media/usb/TarBalls/
```

## Safety Notes

1. **Test in mock mode first** - Always verify behavior before enabling real motors
2. **Set conservative speed limits** - Start with `max_speed = 0.3`
3. **Use emergency stop** - Wire a physical E-stop button to the GPIO pin
4. **Supervise with children** - Robot is a toy, not a babysitter
5. **Obstacle avoidance** - Enable LIDAR if available, or keep `confirm_movement = true`

## License

MIT - Same as ZeroClaw
