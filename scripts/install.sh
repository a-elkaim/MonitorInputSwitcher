#!/bin/sh
# Installs the Monitor Input Switcher built by ./scripts/build.sh for the current user (~/.local).
# Pass --debug to install the debug build (./scripts/build.sh --debug) instead of the release one.
# No root needed; run "sudo ./scripts/setup-system.sh" once first so the app may talk to the monitor
# (see README.md).
set -eu

APP=monitor-input-switcher
ROOT=$(cd "$(dirname "$0")/.." && pwd)
case "${1:-}" in
    "") PROFILE=release; BUILD_CMD="$ROOT/scripts/build.sh" ;;
    --debug) PROFILE=debug; BUILD_CMD="$ROOT/scripts/build.sh --debug" ;;
    *) echo "Usage: $0 [--debug]"; exit 2 ;;
esac
BUILT="$ROOT/target/$PROFILE/$APP"
BIN="$HOME/.local/bin/$APP"
ICON_DIR="$HOME/.local/share/icons/hicolor/256x256/apps"
APPS="$HOME/.local/share/applications"

[ -x "$BUILT" ] || { echo "$BUILT not found: build it first with $BUILD_CMD"; exit 1; }

# Stop a running copy so the new binary is used.
if [ -x "$BIN" ]; then
    "$BIN" quit >/dev/null 2>&1 || true
    sleep 1
fi

mkdir -p "$(dirname "$BIN")" "$ICON_DIR" "$APPS"
install -m 755 "$BUILT" "$BIN"
install -m 644 "$ROOT/assets/icon-256.png" "$ICON_DIR/$APP.png"

cat > "$APPS/$APP.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Monitor Input Switcher
Comment=Switch the monitor input over DDC/CI from the system tray
Exec=$BIN
Icon=$APP
Terminal=false
Categories=Utility;
Keywords=monitor;display;input;kvm;ddc;
EOF
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$APPS" || true

# Point an existing autostart entry at the installed binary (the first run creates it otherwise).
if [ -f "$HOME/.config/autostart/$APP.desktop" ]; then
    sed -i "s|^Exec=.*|Exec=$BIN|" "$HOME/.config/autostart/$APP.desktop"
fi

echo "Detected displays:"
if out=$("$BIN" list 2>&1); then
    echo "$out" | sed 's/^/  /'
else
    echo "  $out"
    echo "  Could not reach the monitor. If this is a first install, run: sudo $ROOT/scripts/setup-system.sh"
fi

nohup "$BIN" >/dev/null 2>&1 &
echo "Installed to $BIN. Monitor Input Switcher is running in the system tray."
echo "CLI: $APP switch <name|code> | next | get | list | capabilities | settings | quit"
