#!/bin/bash
# Start a browser on a virtual display
# Usage: ./start-browser.sh [display_num] [url]

set -e

DISPLAY_NUM=${1:-99}
URL=${2:-"https://google.com"}

export DISPLAY=:$DISPLAY_NUM

# Check if display is running
if ! xdpyinfo -display :$DISPLAY_NUM &>/dev/null; then
    echo "Error: Display :$DISPLAY_NUM not running."
    echo "Start VNC first: ./start-vnc.sh"
    exit 1
fi

google-chrome --no-sandbox --disable-gpu --disable-setuid-sandbox "$URL" &
echo "Chrome started on display :$DISPLAY_NUM"
echo "View via VNC or noVNC"
