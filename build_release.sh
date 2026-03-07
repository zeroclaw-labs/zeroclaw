#!/bin/bash
# Build ZeroClaw in release mode
set -e
echo "Building ZeroClaw in release mode..."
cd /Users/argenisdelarosa/Downloads/zeroclaw
cargo build --release
echo "Build completed successfully!"
echo "Binary location: target/release/zeroclaw"