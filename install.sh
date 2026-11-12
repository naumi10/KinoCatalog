#!/usr/bin/env bash

# Stop the script if any command fails
set -e

# installing to user's local directory doesn't require sudo
INSTALL_DIR="$HOME/.local/share/KinoCatalog"
BIN_DIR="$HOME/.local/bin"
SHORTCUT_DIR="$HOME/.local/share/applications"

echo "Installing KinoCatalog..."

mkdir -p "$INSTALL_DIR"
mkdir -p "$BIN_DIR"
mkdir -p "$SHORTCUT_DIR"

# Copy but exclude the install script itself to keep it clean
rsync -av --exclude='install.sh' ./ "$INSTALL_DIR/"

# Make sure the executable is runnable
chmod +x "$INSTALL_DIR/KinoCatalog"
# Create a symlink so users can run it from terminal
ln -sf "$INSTALL_DIR/KinoCatalog" "$BIN_DIR/KinoCatalog"

# Create the .desktop file for the Menu
cat <<EOF > "$SHORTCUT_DIR/KinoCatalog.desktop"
[Desktop Entry]
Version=1.0
Type=Application
Name=KinoCatalog
Comment=Movies catalog application
Exec="$INSTALL_DIR/KinoCatalog"
Icon=$INSTALL_DIR/KinoCatalog.png
Terminal=false
Categories=AudioVideo;Video;Utility;
EOF

chmod +x "$SHORTCUT_DIR/KinoCatalog.desktop"

echo "Installation complete!"
