#!/usr/bin/env bash
# System libraries required to build GPUI on Linux (X11 + Wayland, Vulkan, fonts).
set -euo pipefail
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev \
  libx11-xcb-dev libxcb1-dev libxcb-xkb-dev libxcb-randr0-dev libxcb-xfixes0-dev \
  libfontconfig-dev libfreetype-dev libssl-dev libasound2-dev pkg-config
