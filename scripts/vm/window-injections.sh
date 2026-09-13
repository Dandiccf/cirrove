#!/usr/bin/env bash
# The two deliberate injections the sustained-operation row asks for, performed
# inside the window rather than against a daemon that has just started.
#
# The row's shape names them: a restart at a recorded point, and a network
# outage at another, each observed in a daemon that has been up for hours. This
# waits for the recorded offsets, performs them, and writes what it did to
# ~/window/injections.log inside the VM -- a separate file from the samples,
# because the sampler holds that one open and two writers would interleave.
#
# Usage: scripts/vm/window-injections.sh <distro> <window-start-unix>
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
distro=${1:?usage: window-injections.sh <distro> <window-start-unix>}
start=${2:?window start, unix seconds}
dir="$HOME/Work/cirrove-vms/$distro"
vm() { bash "$here/run.sh" "$distro" ssh "$@"; }
note() { vm "printf '%s\n' \"\$(date -Is) $1\" >> ~/window/injections.log"; }

RESTART_AT=$((start + 4 * 3600))
OUTAGE_AT=$((start + 8 * 3600))
OUTAGE_FOR=300

wait_until() {
  local when=$1
  while (( $(date +%s) < when )); do sleep 60; done
}

# ssh rides the same virtual cable the outage unplugs, so nothing may assume it
# is there the instant the link comes back.
wait_for_ssh() {
  for _ in $(seq 1 60); do
    vm true 2>/dev/null && return 0
    sleep 5
  done
  return 1
}

# --- the restart -------------------------------------------------------------
wait_until "$RESTART_AT"
note "RESTART begins; the daemon has been up $(( (RESTART_AT - start) / 3600 ))h"
vm 'systemctl --user restart cirroved.service'
# The account must come back on its own. Nothing is unlocked, nothing is
# re-signed-in: that is the prediction.
began=$(date +%s)
for _ in $(seq 1 60); do
  if vm 'cirrove status 2>/dev/null | python3 -c "
import json,sys
a=json.load(sys.stdin)[\"accounts\"][0]
sys.exit(0 if a[\"mounted\"] and a[\"state\"]==\"ready\" and all(f[\"state\"]==\"ready\" for f in a[\"feeds\"]) else 1)"' 2>/dev/null; then
    note "RESTART recovered unattended after $(( $(date +%s) - began )) s"
    break
  fi
  sleep 10
done
vm 'cirrove status 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin); a=d[\"accounts\"][0]
print(f\"  after restart: {a[\\\"state\\\"]} mounted={a[\\\"mounted\\\"]} items={d[\\\"indexed_items\\\"]} feeds={[f[\\\"state\\\"] for f in a[\\\"feeds\\\"]]}\")" >> ~/window/injections.log'

# --- the outage --------------------------------------------------------------
wait_until "$OUTAGE_AT"
note "OUTAGE begins; the link goes down for ${OUTAGE_FOR}s"
python3 "$here/qmp.py" "$dir/qmp.sock" link n0 off
# A listing must not stall while the provider is unreachable: that is P4, and
# the sampler is timing it every minute throughout.
sleep "$OUTAGE_FOR"
python3 "$here/qmp.py" "$dir/qmp.sock" link n0 on
wait_for_ssh || { echo "ssh did not come back after the outage" >&2; exit 1; }
note "OUTAGE ends; the link is back"
for _ in $(seq 1 90); do
  if vm 'cirrove status 2>/dev/null | python3 -c "
import json,sys
a=json.load(sys.stdin)[\"accounts\"][0]
sys.exit(0 if a[\"mounted\"] and all(f[\"state\"]==\"ready\" for f in a[\"feeds\"]) else 1)"' 2>/dev/null; then
    note "OUTAGE: both feeds returned to ready without intervention"
    break
  fi
  sleep 10
done
# And a change made afterwards is delivered.
stamp=$(date +%s)
vm "echo 'after the outage, $stamp' > ~/OneDrive/outage-check-$stamp.txt"
for _ in $(seq 1 60); do
  if vm "cirrove recent 2>/dev/null | grep -q 'uploaded  outage-check-$stamp.txt'"; then
    note "OUTAGE: a change made afterwards reached the cloud (outage-check-$stamp.txt)"
    break
  fi
  sleep 10
done
vm "cirrove recent 2>/dev/null | head -6 >> ~/window/injections.log"
note "injections complete"
