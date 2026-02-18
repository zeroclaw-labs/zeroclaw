# Raspberry Pi 5 Robot Setup Guide

Complete guide to setting up a ZeroClaw-powered robot on Raspberry Pi 5.

## Hardware Requirements

### Minimum Setup
| Component | Recommended | Notes |
|-----------|-------------|-------|
| **Pi 5** | 8GB model | 4GB works but limits model size |
| **Storage** | 64GB+ NVMe or SD | NVMe recommended for speed |
| **Power** | 27W USB-C PSU | Official Pi 5 PSU recommended |
| **Cooling** | Active cooler | Required for sustained inference |

### Robot Hardware
| Component | Model | Connection | Price (approx) |
|-----------|-------|------------|----------------|
| **Motor Controller** | L298N or TB6612FNG | GPIO PWM | $5-15 |
| **Motors** | 4× TT Motors + Omni wheels | Via controller | $30-50 |
| **LIDAR** | RPLidar A1 | USB `/dev/ttyUSB0` | $100 |
| **Camera** | Pi Camera 3 or USB webcam | CSI or USB | $25-50 |
| **Microphone** | USB mic or ReSpeaker | USB | $10-30 |
| **Speaker** | 3W amp + speaker | I2S or 3.5mm | $10-20 |
| **E-Stop** | Big red mushroom button | GPIO 4 | $5 |
| **Bump Sensors** | 2× Microswitches | GPIO 5, 6 | $3 |
| **LED Matrix** | 8×8 WS2812B | GPIO 18 (PWM) | $10 |

### Wiring Diagram

```
                    ┌─────────────────────────────────────┐
                    │          Raspberry Pi 5             │
                    │                                     │
  ┌─────────────────┤ GPIO 4  ←── E-Stop Button (NC)      │
  │                 │ GPIO 5  ←── Bump Sensor Left        │
  │                 │ GPIO 6  ←── Bump Sensor Right       │
  │                 │ GPIO 12 ──→ Motor PWM 1             │
  │                 │ GPIO 13 ──→ Motor PWM 2             │
  │                 │ GPIO 17 ←── PIR Motion 1            │
  │                 │ GPIO 18 ──→ LED Matrix (WS2812)     │
  │                 │ GPIO 23 ──→ Ultrasonic Trigger      │
  │                 │ GPIO 24 ←── Ultrasonic Echo         │
  │                 │ GPIO 27 ←── PIR Motion 2            │
  │                 │                                     │
  │ ┌───────────────┤ USB-A   ←── RPLidar A1              │
  │ │               │ USB-A   ←── USB Microphone          │
  │ │               │ USB-A   ←── USB Webcam (if no CSI)  │
  │ │               │ CSI     ←── Pi Camera 3             │
  │ │               │ I2S/3.5mm → Speaker/Amp             │
  │ │               └─────────────────────────────────────┘
  │ │
  │ │  ┌──────────────────┐
  │ └──┤    RPLidar A1    │
  │    │  /dev/ttyUSB0    │
  │    └──────────────────┘
  │
  │    ┌──────────────────┐      ┌─────────────┐
  └────┤  Motor Controller├──────┤  4× Motors  │
       │  (L298N/TB6612)  │      │ Omni Wheels │
       └──────────────────┘      └─────────────┘
```

## Software Setup

### 1. Base OS

```bash
# Flash Raspberry Pi OS (64-bit, Bookworm) to NVMe/SD
# Use Raspberry Pi Imager with these settings:
# - Enable SSH
# - Set hostname: robot
# - Set username/password
# - Configure WiFi

# After boot, update everything
sudo apt update && sudo apt upgrade -y

# Install build essentials
sudo apt install -y \
    build-essential \
    git \
    curl \
    cmake \
    pkg-config \
    libssl-dev \
    libasound2-dev \
    libclang-dev
```

### 2. Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

### 3. Install Ollama (Local LLM)

```bash
curl -fsSL https://ollama.ai/install.sh | sh

# Pull models (choose based on RAM)
# 8GB Pi: Use smaller models
ollama pull llama3.2:3b      # 3B params, fast
ollama pull moondream        # Vision model, small

# 4GB Pi: Use tiny models
ollama pull phi3:mini        # 3.8B, very fast
ollama pull moondream        # Vision

# Start Ollama service
sudo systemctl enable ollama
sudo systemctl start ollama

# Test
curl http://localhost:11434/api/tags
```

### 4. Install Whisper.cpp (Speech-to-Text)

```bash
git clone https://github.com/ggerganov/whisper.cpp
cd whisper.cpp

# Build with ARM optimizations
make -j4

# Download model (base is good balance)
bash ./models/download-ggml-model.sh base

# Install
sudo cp main /usr/local/bin/whisper-cpp
mkdir -p ~/.zeroclaw/models
cp models/ggml-base.bin ~/.zeroclaw/models/
```

### 5. Install Piper TTS (Text-to-Speech)

```bash
# Download Piper binary
wget https://github.com/rhasspy/piper/releases/download/v1.2.0/piper_arm64.tar.gz
tar -xzf piper_arm64.tar.gz
sudo cp piper/piper /usr/local/bin/

# Download voice model
mkdir -p ~/.zeroclaw/models/piper
cd ~/.zeroclaw/models/piper
wget https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium/en_US-lessac-medium.onnx
wget https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium/en_US-lessac-medium.onnx.json

# Test
echo "Hello, I am your robot!" | piper --model ~/.zeroclaw/models/piper/en_US-lessac-medium.onnx --output_file test.wav
aplay test.wav
```

### 6. Install RPLidar SDK

```bash
# Install rplidar_ros or standalone SDK
sudo apt install -y ros-humble-rplidar-ros  # If using ROS2

# Or use standalone Python/Rust driver
pip3 install rplidar-roboticia

# Add user to dialout group for serial access
sudo usermod -aG dialout $USER
# Logout and login for group change to take effect
```

### 7. Build ZeroClaw Robot Kit

```bash
# Clone repo (or copy from USB)
git clone https://github.com/theonlyhennygod/zeroclaw
cd zeroclaw

# Build robot kit
cargo build --release -p zeroclaw-robot-kit

# Build main zeroclaw (optional, if using as agent)
cargo build --release
```

## Configuration

### Create robot.toml

```bash
mkdir -p ~/.zeroclaw
nano ~/.zeroclaw/robot.toml
```

```toml
# ~/.zeroclaw/robot.toml - Real Hardware Configuration

# =============================================================================
# DRIVE SYSTEM
# =============================================================================
[drive]
# Use serial for Arduino-based motor controller
# Or "ros2" if using ROS2 nav stack
backend = "serial"
serial_port = "/dev/ttyACM0"  # Arduino
# backend = "ros2"
# ros2_topic = "/cmd_vel"

# Speed limits - START CONSERVATIVE!
max_speed = 0.3        # m/s - increase after testing
max_rotation = 0.5     # rad/s

# =============================================================================
# CAMERA / VISION
# =============================================================================
[camera]
# Pi Camera 3
device = "/dev/video0"
# Or for USB webcam:
# device = "/dev/video1"

width = 640
height = 480

# Vision model
vision_model = "moondream"
ollama_url = "http://localhost:11434"

# =============================================================================
# AUDIO (SPEECH)
# =============================================================================
[audio]
# Find devices with: arecord -l && aplay -l
mic_device = "plughw:1,0"      # USB mic
speaker_device = "plughw:0,0"  # Default output

whisper_model = "base"
whisper_path = "/usr/local/bin/whisper-cpp"

piper_path = "/usr/local/bin/piper"
piper_voice = "en_US-lessac-medium"

# =============================================================================
# SENSORS
# =============================================================================
[sensors]
# RPLidar A1
lidar_port = "/dev/ttyUSB0"
lidar_type = "rplidar"

# PIR motion sensors
motion_pins = [17, 27]

# HC-SR04 ultrasonic (optional backup for LIDAR)
ultrasonic_pins = [23, 24]

# =============================================================================
# SAFETY - CRITICAL!
# =============================================================================
[safety]
min_obstacle_distance = 0.3    # 30cm - don't go closer
slow_zone_multiplier = 3.0     # Start slowing at 90cm
approach_speed_limit = 0.3     # 30% speed near obstacles
max_drive_duration = 30        # Auto-stop after 30s
estop_pin = 4                  # GPIO 4 for E-STOP
bump_sensor_pins = [5, 6]      # Front bump switches
bump_reverse_distance = 0.15   # Back up 15cm after bump
confirm_movement = false
predict_collisions = true
sensor_timeout_secs = 5
blind_mode_speed_limit = 0.2
```

### Test Each Component

```bash
# Test LIDAR
python3 -c "
from rplidar import RPLidar
lidar = RPLidar('/dev/ttyUSB0')
for scan in lidar.iter_scans():
    print(f'Got {len(scan)} points')
    break
lidar.stop()
lidar.disconnect()
"

# Test camera
ffmpeg -f v4l2 -video_size 640x480 -i /dev/video0 -frames:v 1 test.jpg
xdg-open test.jpg  # View on desktop

# Test microphone
arecord -D plughw:1,0 -f S16_LE -r 16000 -c 1 -d 3 test.wav
aplay test.wav

# Test speaker
echo "Testing speaker" | piper --model ~/.zeroclaw/models/piper/en_US-lessac-medium.onnx --output_file - | aplay -D plughw:0,0

# Test Ollama
curl http://localhost:11434/api/generate -d '{"model":"llama3.2:3b","prompt":"Say hello"}'

# Test motors (careful!)
# Write a simple test script for your motor controller
```

## Running the Robot

### Start Sensor Loop (Background)

```bash
# Create sensor feeder script
cat > ~/sensor_loop.py << 'EOF'
#!/usr/bin/env python3
"""Feed sensor data to safety monitor via FIFO."""
import os
import json
import time
from rplidar import RPLidar

FIFO_PATH = "/tmp/zeroclaw_sensors.fifo"

def main():
    if not os.path.exists(FIFO_PATH):
        os.mkfifo(FIFO_PATH)

    lidar = RPLidar('/dev/ttyUSB0')

    try:
        with open(FIFO_PATH, 'w') as fifo:
            for scan in lidar.iter_scans():
                # Find minimum distance
                if scan:
                    min_dist = min(p[2]/1000 for p in scan)  # mm to m
                    min_angle = min(scan, key=lambda p: p[2])[1]

                    msg = json.dumps({
                        "type": "lidar",
                        "distance": min_dist,
                        "angle": int(min_angle)
                    })
                    fifo.write(msg + "\n")
                    fifo.flush()

                time.sleep(0.1)  # 10Hz
    finally:
        lidar.stop()
        lidar.disconnect()

if __name__ == "__main__":
    main()
EOF

chmod +x ~/sensor_loop.py

# Run in background
nohup python3 ~/sensor_loop.py &
```

### Start ZeroClaw Agent

```bash
# Configure ZeroClaw to use robot tools
cat > ~/.zeroclaw/config.toml << 'EOF'
api_key = ""  # Not needed for local Ollama
default_provider = "ollama"
default_model = "llama3.2:3b"

[memory]
backend = "sqlite"
embedding_provider = "noop"  # No cloud embeddings

[autonomy]
level = "supervised"
workspace_only = true
EOF

# Copy robot personality
cp ~/zeroclaw/crates/robot-kit/SOUL.md ~/.zeroclaw/workspace/

# Start agent
./target/release/zeroclaw agent
```

### Full Robot Startup Script

```bash
#!/bin/bash
# ~/start_robot.sh

set -e

echo "Starting robot..."

# Start Ollama if not running
if ! pgrep -x "ollama" > /dev/null; then
    ollama serve &
    sleep 5
fi

# Start sensor loop
if [ ! -p /tmp/zeroclaw_sensors.fifo ]; then
    mkfifo /tmp/zeroclaw_sensors.fifo
fi
python3 ~/sensor_loop.py &
SENSOR_PID=$!

# Start zeroclaw
cd ~/zeroclaw
./target/release/zeroclaw daemon &
AGENT_PID=$!

echo "Robot started!"
echo "  Sensor PID: $SENSOR_PID"
echo "  Agent PID: $AGENT_PID"

# Wait for Ctrl+C
trap "kill $SENSOR_PID $AGENT_PID; exit" INT
wait
```

## Systemd Services (Auto-Start on Boot)

```bash
# /etc/systemd/system/zeroclaw-robot.service
sudo tee /etc/systemd/system/zeroclaw-robot.service << 'EOF'
[Unit]
Description=ZeroClaw Robot
After=network.target ollama.service

[Service]
Type=simple
User=pi
WorkingDirectory=/home/pi/zeroclaw
ExecStart=/home/pi/start_robot.sh
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable zeroclaw-robot
sudo systemctl start zeroclaw-robot

# Check status
sudo systemctl status zeroclaw-robot
journalctl -u zeroclaw-robot -f  # View logs
```

## Troubleshooting

### LIDAR not detected
```bash
ls -la /dev/ttyUSB*
# If missing, check USB connection
dmesg | grep -i usb
# Add udev rule if needed
echo 'SUBSYSTEM=="tty", ATTRS{idVendor}=="10c4", ATTRS{idProduct}=="ea60", MODE="0666", SYMLINK+="rplidar"' | sudo tee /etc/udev/rules.d/99-rplidar.rules
sudo udevadm control --reload-rules
```

### Audio not working
```bash
# List devices
arecord -l
aplay -l

# Test with specific device
arecord -D plughw:1,0 -f S16_LE -r 16000 -c 1 -d 3 /tmp/test.wav
aplay -D plughw:0,0 /tmp/test.wav
```

### Ollama slow or OOM
```bash
# Check memory
free -h

# Use smaller model
ollama rm llama3.2:3b
ollama pull phi3:mini

# Set memory limit
export OLLAMA_MAX_LOADED_MODELS=1
```

### Motors not responding
```bash
# Check serial connection
ls -la /dev/ttyACM*

# Test serial communication
screen /dev/ttyACM0 115200
# Type commands to motor controller

# Check permissions
sudo usermod -aG dialout $USER
```

## Performance Tips

1. **Use NVMe** - SD cards are slow for model loading
2. **Active cooling** - Pi 5 throttles without it
3. **Smaller models** - llama3.2:3b or phi3:mini
4. **Disable GPU** - Pi doesn't have one, saves confusion
5. **Preload models** - `ollama run llama3.2:3b "warmup"` before use

## Safety Checklist Before First Run

- [ ] E-stop button wired and tested
- [ ] Bump sensors wired and tested
- [ ] LIDAR spinning and returning data
- [ ] max_speed set to 0.3 or lower
- [ ] Robot on blocks/stand (wheels not touching ground)
- [ ] First test with `backend = "mock"` in config
- [ ] Adult supervision ready
- [ ] Clear space around robot
