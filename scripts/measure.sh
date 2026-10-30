#!/bin/sh
# Peso do applet da barra: PSS_kB RSS_kB THREADS FDS CPU%
# Uso: scripts/measure.sh [pid]   (dentro do sandbox, reexecuta no host)
if [ -e /.flatpak-info ]; then
    exec flatpak-spawn --host sh "$0" "$@"
fi
pid=${1:-$(pgrep -f '^[^ ]*/cosmic-ext-applet-taskbar' | head -1)}
[ -n "$pid" ] || { echo "applet nao esta rodando" >&2; exit 1; }
pss=$(awk '/^Pss:/{s+=$2}END{print s}' /proc/$pid/smaps)
rss=$(awk '/^VmRSS:/{print $2}' /proc/$pid/status)
thr=$(awk '/^Threads:/{print $2}' /proc/$pid/status)
fds=$(ls /proc/$pid/fd | wc -l)
cpu=$(ps -o pcpu= -p $pid | tr -d ' ')
echo "PSS_kB=$pss RSS_kB=$rss THREADS=$thr FDS=$fds CPU=$cpu"
