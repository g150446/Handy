#!/bin/bash
# Handy macOS Installer
# Builds and installs Handy to /Applications, preserving user data (models, settings)

set -e

APP_NAME="Handy"
BUILD_PATH="src-tauri/target/release/bundle/macos/${APP_NAME}.app"
INSTALL_DIR="/Applications"

echo "🔧 Building Handy..."
bun run tauri build

echo ""
echo "📦 Installing to /Applications..."
sudo rm -rf "${INSTALL_DIR}/${APP_NAME}.app"
sudo cp -r "${BUILD_PATH}" "${INSTALL_DIR}/"

echo ""
echo "✅ Installation complete!"
echo ""
echo "📍 Your models and settings are preserved in:"
echo "   ~/Library/Application Support/com.pais.handy/"
echo ""
echo "🚀 You can now launch Handy from /Applications or Spotlight."
