#!/bin/sh
# Puts the taskbar in the panel where COSMIC's app tray is. The dock keeps
# its own; the pinned apps are the same in both.
#   ./setup.sh          -> use the taskbar
#   ./setup.sh --undo   -> back to COSMIC's app tray
set -e
cfg="${XDG_CONFIG_HOME:-$HOME/.config}/cosmic/com.system76.CosmicPanel.Panel/v1"
ours=io.github.Aenes0.CosmicTaskbar
theirs=com.system76.CosmicAppList

for f in "$cfg/plugins_center" "$cfg/plugins_wings"; do
    [ -f "$f" ] || continue
    if [ "$1" = "--undo" ]; then
        sed -i "s|\"$ours\"|\"$theirs\"|" "$f"
    else
        sed -i "s|\"$theirs\"|\"$ours\"|" "$f"
    fi
done
if [ "$1" = "--undo" ]; then
    echo "COSMIC's app tray is back in the panel."
elif grep -qs "\"$ours\"" "$cfg/plugins_center" "$cfg/plugins_wings"; then
    echo "Taskbar set in the panel."
else
    echo "No app tray in the panel: add \"Taskbar\" in Settings > Desktop > Panel > Applets."
fi
