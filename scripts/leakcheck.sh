#!/bin/sh
# Snapshot of everything Kimi Lite could leak, locally and on an SSH host.
# Compare runs before and after fault drills: counts should not grow.
HOST="${1:?usage: leakcheck.sh <ssh host>}"
app=$(pgrep -f 'Kimi Lite.app/Contents/MacOS/kimi-lite' | head -1)
echo "== local"
echo "app pid: ${app:-none}"
if [ -n "$app" ]; then
  ps -o rss=,vsz= -p "$app" | awk '{printf "app rss: %.0f MB\n", $1/1024}'
  echo "app open fds: $(lsof -p "$app" 2>/dev/null | wc -l | tr -d ' ')"
  echo "app threads: $(ps -M -p "$app" | tail -n +2 | wc -l | tr -d ' ')"
fi
echo "ssh procs (kimi-lite): $(pgrep -f 'ssh .*kimi-lite-' | wc -l | tr -d ' ')"
pgrep -fl 'ssh .*kimi-lite-' | sed -E 's/^([0-9]+) .*( -M | -O [a-z]+ | -T ).*/  \1\2/' | sort | uniq -c | sed 's/^/  /'
echo "control sockets: $(ls /tmp/kimi-lite-$USER 2>/dev/null | tr '\n' ' ')"
echo "ssh listeners: $(lsof -a -nP -iTCP -sTCP:LISTEN -c ssh 2>/dev/null | awk 'NR>1{print $9}' | sort -u | tr '\n' ' ')"
echo "== remote"
ssh -o BatchMode=yes -o ConnectTimeout=15 -o ControlMaster=no -o ControlPath=none "$HOST" '
  procs=$(ps -u "$(id -u)" -o pid=,etime=,comm= | awk '"'"'$3 ~ /^kimi/'"'"')
  echo "kimi procs: $(printf "%s" "$procs" | grep -c .)  $(printf "%s" "$procs" | tr -s " " | tr "\n" ";")"
  echo "state: $(cd ~/.local/state/kimi-lite 2>/dev/null && for f in *; do printf "%s(%s) " "$f" "$(head -1 "$f" 2>/dev/null | cut -c1-12)"; done)"
  echo "sshd sessions: $(pgrep -u "$(id -u)" -f "sshd:" | wc -l)"
  echo "kimi listeners: $(ss -ltnp 2>/dev/null | grep -c kimi)"
' 2>&1 | sed 's/^/  /'
