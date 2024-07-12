#!/bin/bash

set -e

# Check if script is run as root
if [ "$EUID" -ne 0 ]; then
  echo "Please run as root"
  exit
fi

# Set variables
INSTALL_DIR="/opt/lust"
CONFIG_DIR="/etc/lust"
BINARY_DIR="/usr/local/bin"
SERVICE_NAME="lust"
USER="lust"
GROUP="lust"

echo "Starting installation of Lust..."

# Create necessary directories
mkdir -p $INSTALL_DIR $CONFIG_DIR

# Compile the Rust binary (adjust as needed)
echo "Compiling Rust binary..."
cargo build --release

# Copy the binary
echo "Copying binary to $BINARY_DIR..."
cp target/release/lust $BINARY_DIR/

# Copy the config file
echo "Copying config file to $CONFIG_DIR..."
cp examples/configs/example.yaml $CONFIG_DIR/config.yaml

# Copy the systemd service file
echo "Setting up systemd service..."
cp lust.service /etc/systemd/system/

# Create a user for the service if it doesn't exist
if ! id "$USER" &>/dev/null; then
    echo "Creating user $USER..."
    useradd -r -s /bin/false $USER
fi

# Set permissions
echo "Setting permissions..."
chown -R $USER:$GROUP $INSTALL_DIR $CONFIG_DIR
chmod 644 $CONFIG_DIR/config.yaml
chmod 755 $BINARY_DIR/lust

# Reload systemd, enable and start the service
echo "Configuring and starting systemd service..."
systemctl daemon-reload
systemctl enable $SERVICE_NAME
systemctl start $SERVICE_NAME

echo "Installation complete!"
echo "Lust service is now running. You can check its status with: systemctl status $SERVICE_NAME"
echo "Configuration file is located at: $CONFIG_DIR/config.yaml"