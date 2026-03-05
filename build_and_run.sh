#!/bin/bash

# Build ZeroClaw in release mode
echo "Building ZeroClaw in release mode..."
cargo build --release

# Check if build was successful
if [ $? -eq 0 ]; then
    echo "Build successful!"
    echo "To start the web dashboard, run:"
    echo "./target/release/zeroclaw gateway"
    echo ""
    echo "The dashboard will typically be available at http://127.0.0.1:3000/"
    echo "You can also specify a custom port with -p, e.g.:"
    echo "./target/release/zeroclaw gateway -p 8080"
else
    echo "Build failed!"
    exit 1
fi