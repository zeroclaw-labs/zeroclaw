#!/bin/bash
# Stop virtual display and VNC server
# Usage: ./stop-vnc.sh [display_num]

DISPLAY_NUM=${1:-99}

pkill -f "Xvfb :$DISPLAY_NUM" 2>/dev/null || true
pkill -f "x11vnc.*:$DISPLAY_NUM" 2>/dev/null || true
pkill -f "websockify.*6080" 2>/dev/null || true

echo "VNC server stopped"
