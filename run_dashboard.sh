#!/bin/bash
# Quick setup script to run ZeroClaw web dashboard

echo "🦀 ZeroClaw Web Dashboard Setup"
echo "================================"

# Check if web assets are built
if [ ! -d "web/dist" ]; then
    echo "📦 Building web assets first..."
    cd web
    npm run build
    cd ..
    echo "✅ Web assets built!"
fi

# Build the project
echo "🔨 Building ZeroClaw binary with embedded web dashboard..."
cargo build --release

# Check if build was successful
if [ -f "target/release/zeroclaw" ]; then
    echo "✅ Build successful! Web dashboard is embedded in the binary."
    echo ""
    echo "🚀 Starting ZeroClaw Gateway..."
    echo "📱 Dashboard URL: http://127.0.0.1:3000/"
    echo "🔧 API Endpoint: http://127.0.0.1:3000/api/"
    echo "⏹️  Press Ctrl+C to stop the gateway"
    echo ""
    
    # Start the gateway
    ./target/release/zeroclaw gateway --open-dashboard
else
    echo "❌ Build failed! Please check the error messages above."
    exit 1
fi