#!/bin/sh
# Builds "Monitor Input Switcher.app" into target/release/ (or target/debug/ with --debug): a menu
# bar app with no Dock icon, ad-hoc signed. With --install, also copies it to /Applications, links
# the command line to ~/.local/bin/monitor-input-switcher and starts it.
set -eu

APP=monitor-input-switcher
NAME="Monitor Input Switcher"
BUNDLE_ID=io.github.monitor-input-switcher
ROOT=$(cd "$(dirname "$0")/.." && pwd)

PROFILE=release
FLAGS=--release
INSTALL=
for arg in "$@"; do
    case "$arg" in
        --debug) PROFILE=debug; FLAGS= ;;
        --install) INSTALL=1 ;;
        *) echo "Usage: $0 [--debug] [--install]"; exit 2 ;;
    esac
done

[ "$(uname -s)" = Darwin ] || { echo "This script builds the macOS app; on Linux use ./scripts/build.sh"; exit 1; }
command -v cargo >/dev/null 2>&1 || { echo "cargo not found: install Rust from https://rustup.rs"; exit 1; }

# $FLAGS is empty for a debug build, so it's left unquoted on purpose.
cargo build $FLAGS --manifest-path "$ROOT/Cargo.toml"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
OUT="$ROOT/target/$PROFILE"
BUNDLE="$OUT/$NAME.app"

rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"
cp "$OUT/$APP" "$BUNDLE/Contents/MacOS/$APP"

# App icon, from the 256 px PNG.
ICONSET=$(mktemp -d)/AppIcon.iconset
mkdir -p "$ICONSET"
for size in 16 32 128 256; do
    sips -z $size $size "$ROOT/assets/icon-256.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
done
for size in 16 128; do
    double=$((size * 2))
    sips -z $double $double "$ROOT/assets/icon-256.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$BUNDLE/Contents/Resources/AppIcon.icns"
rm -rf "$(dirname "$ICONSET")"

cat > "$BUNDLE/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$NAME</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key>
    <string>$APP</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>Copyright (C) 2026 Allan ELKAIM. GPL-3.0-or-later.</string>
</dict>
</plist>
EOF

codesign --force --sign - "$BUNDLE" >/dev/null 2>&1 || echo "Warning: could not sign the app (it still runs on this Mac)"
echo "Built $BUNDLE"

if [ -z "$INSTALL" ]; then
    if [ "$PROFILE" = debug ]; then
        echo "Install it with: $0 --debug --install"
    else
        echo "Install it with: $0 --install"
    fi
    exit 0
fi

TARGET="/Applications/$NAME.app"
# Stop the running copy before replacing it.
if [ -x "$TARGET/Contents/MacOS/$APP" ]; then
    "$TARGET/Contents/MacOS/$APP" quit >/dev/null 2>&1 || true
    sleep 1
fi
rm -rf "$TARGET"
cp -R "$BUNDLE" "$TARGET"
mkdir -p "$HOME/.local/bin"
ln -sf "$TARGET/Contents/MacOS/$APP" "$HOME/.local/bin/$APP"
echo "Installed $TARGET"
echo "Command line: ~/.local/bin/$APP (add ~/.local/bin to your PATH if it isn't)"
open "$TARGET"
