#!/bin/sh
# One-time system setup (run with sudo): lets the logged-in user talk DDC/CI to monitors.
#   sudo ./setup-system.sh            install
#   sudo ./setup-system.sh --remove   undo
#
# - Loads the i2c-dev kernel module now and at every boot (it provides /dev/i2c-N).
# - Installs a udev rule that tags the I2C buses of graphics adapters (PCI class 0x03xxxx) with
#   "uaccess", so the user of the active session gets read/write access to them. No group needed.
#   Other I2C buses (SMBus, touchpads, sensors) stay root-only.
set -eu

RULE=/etc/udev/rules.d/60-monitor-input-switcher.rules
MODULES=/etc/modules-load.d/monitor-input-switcher.conf

if [ "$(id -u)" -ne 0 ]; then
    echo "Run with sudo: sudo $0 $*"
    exit 1
fi

if [ "${1:-}" = "--remove" ]; then
    rm -f "$RULE" "$MODULES"
    udevadm control --reload-rules
    udevadm trigger --subsystem-match=i2c-dev
    echo "Removed the udev rule and the i2c-dev boot entry."
    exit 0
fi

cat > "$RULE" <<'EOF'
# Monitor Input Switcher: let the active session's user use DDC/CI on graphics adapters' I2C buses.
SUBSYSTEM=="i2c-dev", KERNEL=="i2c-[0-9]*", ATTRS{class}=="0x03*", TAG+="uaccess"
EOF
echo i2c-dev > "$MODULES"

modprobe i2c-dev
udevadm control --reload-rules
udevadm trigger --subsystem-match=i2c-dev --action=add
udevadm settle

echo "Done. Display I2C buses are now accessible to the logged-in user:"
for dev in /dev/i2c-*; do
    [ -e "$dev" ] || continue
    if udevadm info "$dev" 2>/dev/null | grep -q 'TAGS=.*uaccess'; then
        echo "  $dev"
    fi
done
