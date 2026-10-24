#!/bin/sh
# Rig Hub installer for Linux and macOS:
#   curl -fsSL https://raw.githubusercontent.com/SusieeTheLinuxUser/rig-hub/main/install.sh | sh
set -eu

REPO="SusieeTheLinuxUser/rig-hub"
BIN="$HOME/.local/bin"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)   target=x86_64-linux ;;
  Linux-aarch64)  target=aarch64-linux ;;
  Darwin-arm64)   target=aarch64-macos ;;
  Darwin-x86_64)  target=x86_64-macos ;;
  *) echo "Sorry, no Rig Hub build for $(uname -s) $(uname -m) yet." >&2; exit 1 ;;
esac

url="https://github.com/$REPO/releases/latest/download/rig-hub-$target.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading Rig Hub ($target)…"
curl -fsSL "$url" | tar -xz -C "$tmp"
mkdir -p "$BIN"
install -m 755 "$tmp/rig-hub" "$BIN/rig-hub"

if [ "$(uname -s)" = Linux ]; then
  icons="$HOME/.local/share/icons/hicolor/scalable/apps"
  apps="$HOME/.local/share/applications"
  mkdir -p "$icons" "$apps"
  install -m 644 "$tmp/rig-hub.svg" "$icons/rig-hub.svg"
  cat > "$apps/rig-hub.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Rig Hub
Comment=Mouse, cooler LCD, headset and mic in one app
Exec=$BIN/rig-hub
Icon=rig-hub
Categories=Settings;HardwareSettings;
Keywords=mouse;dpi;cooler;lcd;nzxt;logitech;corsair;headset;
StartupWMClass=rig-hub
DESKTOP

  # NZXT coolers need a udev rule so normal users can talk to them
  rule=/etc/udev/rules.d/71-rig-hub.rules
  if ! ls /etc/udev/rules.d/*liquidctl* /usr/lib/udev/rules.d/*liquidctl* >/dev/null 2>&1 && [ ! -e "$rule" ]; then
    printf 'Allow Rig Hub to control NZXT coolers without root? Installs %s (needs sudo) [y/N] ' "$rule"
    read -r answer < /dev/tty || answer=n
    if [ "$answer" = y ] || [ "$answer" = Y ]; then
      echo 'SUBSYSTEMS=="usb", ATTRS{idVendor}=="1e71", TAG+="uaccess"' | sudo tee "$rule" >/dev/null
      sudo udevadm control --reload-rules && sudo udevadm trigger
    fi
  fi
fi

echo
echo "Installed to $BIN/rig-hub"
case ":$PATH:" in *":$BIN:"*) ;; *) echo "Add $BIN to your PATH to run it from a terminal." ;; esac
if [ "$(uname -s)" = Linux ]; then
  echo
  echo "Optional, for full support:"
  command -v ratbagctl >/dev/null 2>&1 || echo "  mouse:   install libratbag/ratbagd and run: sudo systemctl enable --now ratbagd"
  command -v headsetcontrol >/dev/null 2>&1 || echo "  headset: install headsetcontrol"
  command -v piper >/dev/null 2>&1 || echo "  mouse picture: install piper"
fi
