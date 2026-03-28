#!/bin/bash
# Start virtual display with VNC access for browser GUI
# Usage: ./start-vnc.sh [display_num] [vnc_port] [novnc_port] [resolution]

set -e

DISPLAY_NUM=${1:-99}
VNC_PORT=${2:-5900}
NOVNC_PORT=${3:-6080}
RESOLUTION=${4:-1920x1080x24}

echo "Starting virtual display :$DISPLAY_NUM at $RESOLUTION"

# Kill any existing sessions
pkill -f "Xvfb :$DISPLAY_NUM" 2>/dev/null || true
pkill -f "x11vnc.*:$DISPLAY_NUM" 2>/dev/null || true
pkill -f "websockify.*$NOVNC_PORT" 2>/dev/null || true
sleep 1

# Start Xvfb (virtual framebuffer)
Xvfb :$DISPLAY_NUM -screen 0 $RESOLUTION -ac &
XVFB_PID=$!
sleep 1

# Set DISPLAY
export DISPLAY=:$DISPLAY_NUM

# Start window manager
fluxbox -display :$DISPLAY_NUM 2>/dev/null &
sleep 1

# Start x11vnc
x11vnc -display :$DISPLAY_NUM -rfbport $VNC_PORT -forever -shared -nopw -bg 2>/dev/null
sleep 1

# Start noVNC (web-based VNC client)
websockify --web=/usr/share/novnc $NOVNC_PORT localhost:$VNC_PORT &
NOVNC_PID=$!

echo ""
echo "==================================="
echo "VNC Server started!"
echo "==================================="
echo "VNC Direct:  localhost:$VNC_PORT"
echo "noVNC Web:   http://localhost:$NOVNC_PORT/vnc.html"
echo "Display:     :$DISPLAY_NUM"
echo "==================================="
echo ""
echo "To start a browser, run:"
echo "  DISPLAY=:$DISPLAY_NUM google-chrome &"
echo ""
echo "To stop, run: pkill -f 'Xvfb :$DISPLAY_NUM'"
