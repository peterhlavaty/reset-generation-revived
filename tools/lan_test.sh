#!/bin/bash
# lan_test.sh [players] -- local multiplayer demo on one PC, no internet needed.
# Starts one game that hosts a match for `players` (2-4, default 2) and players-1 games that join
# it through Quick Match. The host's game runs the relay (server=auto); the others find it by LAN
# discovery. The menus are driven by key scripts (--keys); afterwards each window is played with
# its own keyboard focus.
# Needs: tools/build_windows.sh (dist/).
set -e
R="$(cd "$(dirname "$0")/.." && pwd)"
N=${1:-2}
EXE="$R/dist/ResetGeneration-Revived/ResetGeneration-Revived.exe"
LOG=${TEMP:-/tmp}/rg-lan; mkdir -p "$LOG"
[ -x "$EXE" ] || { echo "build first: tools/build_windows.sh"; exit 1; }
# Host: skip the intro, then host automatically (host=auto waits for the main menu).
"$EXE" --nickname=Host --server=auto --host=auto --host_players=$N --keys=6000:ESC --window_scale=1 \
  --window_x=20 --window_y=20 > "$LOG/host.log" 2>&1 &
# Start the others once the host is hosting: that also keeps them from patching the game data
# while the host does (first launch per resolution), which leaves the host with a black screen.
for _ in $(seq 90); do
  grep -q "HostServer: hosting" "$LOG/host.log" 2>/dev/null && break
  sleep 1
done
grep -q "HostServer: hosting" "$LOG/host.log" || { echo "the host did not start hosting, see $LOG/host.log"; exit 1; }
# Clients: intro, main menu (ESC opens Quit -> No), down to Quick Match, pick the first hero.
K="6000:ESC,8000:ESC,9500:RETURN,10500:DOWN,11000:DOWN,11500:RETURN,14500:RETURN"
for i in $(seq 2 $N); do
  "$EXE" --nickname=Player$i --server=auto "--keys=$K" --window_scale=1 \
    --window_x=$(( (i - 1) % 2 * 680 + 20 )) --window_y=$(( (i - 1) / 2 * 420 + 20 )) > "$LOG/player$i.log" 2>&1 &
  sleep 2
done
echo "started $N games (logs: $LOG); the match begins about 15 s after the last one"
