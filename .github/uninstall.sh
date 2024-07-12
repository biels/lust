#!/bin/bash

set -e

if [ "$EUID" -ne 0 ]; then
  echo "Please run as root"
  exit
fi

SERVICE_NAME="lust"

echo "Stopping and disabling Lust service..."
systemctl stop $SERVICE_NAME
systemctl disable $SERVICE_NAME

echo "Removing files..."
rm -f /etc/systemd/system/$SERVICE_NAME.service
rm -f /usr/local/bin/lust
rm -rf /etc/lust

echo "Reloading systemd..."
systemctl daemon-reload

echo "Uninstallation complete!"