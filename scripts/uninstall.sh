#!/bin/sh
# Removes Monitor Input Switcher. Pass --purge to also delete the settings (~/.config/monitor-input-switcher).
set -eu

APP=monitor-input-switcher
BIN="$HOME/.local/bin/$APP"

if [ -x "$BIN" ]; then
    "$BIN" quit >/dev/null 2>&1 || true
    sleep 1
fi

# Forget the global shortcuts (the app marks them inactive when it quits).
busctl --user call org.kde.kglobalaccel /component/monitor_input_switcher \
    org.kde.kglobalaccel.Component cleanUp >/dev/null 2>&1 || true

rm -f "$BIN" \
      "$HOME/.local/share/icons/hicolor/256x256/apps/$APP.png" \
      "$HOME/.local/share/applications/$APP.desktop" \
      "$HOME/.config/autostart/$APP.desktop"

if [ "${1:-}" = "--purge" ]; then
    rm -rf "$HOME/.config/$APP"
    echo "Uninstalled, settings removed."
else
    echo "Uninstalled. Settings kept in ~/.config/$APP (use --purge to remove them)."
fi
echo "To also undo the system setup (udev rule, i2c-dev at boot): sudo $(cd "$(dirname "$0")" && pwd)/setup-system.sh --remove"
