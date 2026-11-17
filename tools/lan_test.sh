#!/bin/bash
# lan_test.sh [players] -- local multiplayer demo on one PC, no internet needed.
# Starts the relay (server/, if nothing listens on :8787 yet), one game that hosts a match for
# `players` (2-4, default 2) and players-1 games that join it through Quick Match. The menus are
# driven by key scripts (--keys); afterwards each window is played with its own keyboard focus.
# Needs: tools/build_windows.sh (dist/) and `npm install` in server/.
set -e
R="$(cd "$(dirname "$0")/.." && pwd)"
N=${1:-2}
EXE="$R/dist/ResetGeneration-Revived/ResetGeneration-Revived.exe"
LOG=${TEMP:-/tmp}/rg-lan; mkdir -p "$LOG"
[ -x "$EXE" ] || { echo "build first: tools/build_windows.sh"; exit 1; }
if ! (exec 3<>/dev/tcp/127.0.0.1/8787) 2>/dev/null; then
  (cd "$R/server" && node node/server.js > "$LOG/relay.log" 2>&1 &)
  sleep 1
  echo "relay started (log: $LOG/relay.log)"
fi
# Host: skip the intro, then host automatically (host=auto waits 15 s for the main menu).
"$EXE" --nickname=Host --server=ws://127.0.0.1:8787/ws --host=auto --host_players=$N --keys=6000:ESC --window_scale=1 --window_x=20 --window_y=20 &
sleep 4   # let the first instance patch the game data (first launch per resolution)
# Clients: intro, main menu (ESC opens Quit -> No), down to Quick Match, pick the first hero.
K="6000:ESC,8000:ESC,9500:RETURN,10500:DOWN,11000:DOWN,11500:RETURN,14500:RETURN"
for i in $(seq 2 $N); do
  "$EXE" --nickname=Player$i --server=ws://127.0.0.1:8787/ws "--keys=$K" --window_scale=1 \
    --window_x=$(( (i - 1) % 2 * 680 + 20 )) --window_y=$(( (i - 1) / 2 * 420 + 20 )) &
  sleep 2
done
echo "started $N games; the match begins about 20 s after launch"
