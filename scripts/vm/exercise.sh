#!/usr/bin/env bash
# The recovery checks, on a VM that has a signed-in account: what the
# milestone rows ask for against a live account, done where a machine can be
# unplugged, rebooted, suspended and cut off at will and nobody's work is on it.
#
# Each check is one function so one can be run alone; the default runs them in
# the order that leaves the machine in the best state for the next. Every
# check ends by asking the daemon for the account's state and listing the
# mount root, which is the cheapest thing that notices a daemon that is alive
# and no longer serving. Screenshots land beside the log.
#
# Usage: scripts/vm/exercise.sh <ubuntu|fedora> [check...]
#   checks: icon pin reboot outage crash powercut suspend   (default: all but suspend)
#   suspend takes CIRROVE_VM_SUSPEND_MINUTES (default 95: past the token's lifetime)
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
distro=${1:?ubuntu or fedora}
shift || true
checks=("$@")
[[ ${#checks[@]} -gt 0 ]] || checks=(icon pin reboot outage crash powercut)
vms=${CIRROVE_VMS:-$HOME/Work/cirrove-vms}
dir="$vms/$distro"
out="$dir/exercise"
mkdir -p "$out"
log="$out/exercise.log"
run() { "$here/run.sh" "$distro" "$@"; }
vm() { run ssh "$@"; }
shot() { run shot "$out/$1" >/dev/null; echo "  screenshot $1.png" | tee -a "$log"; }
say() { echo "== $(date +%H:%M:%S) $*" | tee -a "$log"; }
session='XDG_RUNTIME_DIR=/run/user/$(id -u) DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus WAYLAND_DISPLAY=wayland-0 DISPLAY=:0'

wait_ssh() {
  for _ in $(seq 1 120); do vm true 2>/dev/null && return 0; sleep 5; done
  echo "the VM did not answer on ssh" >&2; return 1
}
# The account's label, state, mount flag and feed states, and the time to
# list the mount root -- the sampler's row, by hand.
observe() {
  vm 'cirrove status 2>/dev/null | python3 -c "
import json,sys,time,os
d=json.load(sys.stdin)
for a in d[\"accounts\"]:
    t=time.time(); n=len(os.listdir(a[\"mount_path\"])) if a[\"mounted\"] else -1; dt=time.time()-t
    print(f\"  {a[\"label\"]}: {a[\"state\"]} mounted={a[\"mounted\"]} feeds={[f[\"state\"] for f in a[\"feeds\"]]} stuck={a[\"stuck_changes\"]} root_entries={n} list_s={dt:.2f}\")
"' | tee -a "$log"
}
# Wait until every feed of the account says ready, or give up after a while.
wait_ready() {
  local limit=${1:-60}
  for _ in $(seq 1 "$limit"); do
    if vm 'cirrove status 2>/dev/null | python3 -c "
import json,sys; d=json.load(sys.stdin)
ok=all(a[\"mounted\"] and a[\"state\"]==\"ready\" and a[\"feeds\"] and all(f[\"state\"]==\"ready\" for f in a[\"feeds\"]) for a in d[\"accounts\"]) and d[\"accounts\"]
sys.exit(0 if ok else 1)"' 2>/dev/null; then
      return 0
    fi
    sleep 5
  done
  return 1
}
label() { vm 'cirrove status | python3 -c "import json,sys; print(json.load(sys.stdin)[\"accounts\"][0][\"label\"])"'; }
mount_path() { vm 'cirrove status | python3 -c "import json,sys; print(json.load(sys.stdin)[\"accounts\"][0][\"mount_path\"])"'; }

check_icon() {
  say "icon: the tray with an account is Active and the shell draws it"
  vm "$session busctl --user get-property io.github.Dandiccf.Cirrove.Tray /StatusNotifierItem org.kde.StatusNotifierItem Status IconName 2>&1 | tr '\n' ' '" | tee -a "$log"; echo
  shot icon-top-bar
}

check_pin() {
  say "pin: keep a file offline from the command line and see Files badge it"
  local l m first
  l=$(label); m=$(mount_path)
  first=$(vm "ls -p '$m' | grep -v / | head -1")
  say "  pinning '$first'"
  vm "cirrove pin '$l' --path '$first'" | tee -a "$log"
  vm "cirrove paths '$l' '$first'" | tee -a "$log"
  vm "$session systemd-run --user --collect nautilus '$m' >/dev/null 2>&1; sleep 8"
  shot pin-files-badge
  vm "$session gdbus call --session --dest org.gnome.Nautilus --object-path /org/gnome/Nautilus --method org.gtk.Application.Quit >/dev/null 2>&1 || pkill -x nautilus || true"
}

check_reboot() {
  say "reboot: the service and the tray come back at login, the mount with them"
  vm 'sudo systemctl reboot' || true
  sleep 15; wait_ssh; sleep 20
  if wait_ready 36; then say "  ready after the reboot"; else say "  NOT ready 3 minutes after the reboot"; fi
  observe
  vm "pgrep -a cirrove-tray | cut -c1-50 || echo '  no tray after login'" | tee -a "$log"
  shot after-reboot
}

check_outage() {
  say "outage: the cable pulled for three minutes; the feeds must come back on their own"
  python3 "$here/qmp.py" "$dir/qmp.sock" link n0 off
  say "  link down"
  # ssh rides the user network too, so the checks are the sampler's: what
  # the daemon reports once the cable is back, and that the mount listed
  # throughout is only knowable afterwards from the journal.
  sleep 180
  python3 "$here/qmp.py" "$dir/qmp.sock" link n0 on
  say "  link up"
  sleep 10; wait_ssh
  observe
  if wait_ready 36; then say "  feeds ready again without intervention"; else say "  feeds NOT ready 3 minutes after the link came back"; fi
  observe
  vm 'journalctl --user -u cirroved.service --since -5min --no-pager 2>/dev/null | grep -iE "stopped updating|updating again|offline|unreachable" | tail -5' | tee -a "$log"
}

check_crash() {
  say "crash: the daemon killed outright; systemd restarts it and the mount returns"
  vm 'pid=$(systemctl --user show cirroved.service -p MainPID --value); echo "  pid $pid"; sudo kill -9 "$pid"; sleep 8; systemctl --user show cirroved.service -p MainPID -p NRestarts --value | tr "\n" " "' | tee -a "$log"; echo
  if wait_ready 24; then say "  ready after the crash"; else say "  NOT ready 2 minutes after the crash"; fi
  observe
}

check_powercut() {
  say "powercut: the plug pulled mid-everything; the journal must recover"
  local l m
  l=$(label); m=$(mount_path)
  # A save in flight when the power goes: the outcome to watch is that the
  # journal recovers and nothing is claimed that did not happen.
  vm "echo 'written at $(date -Is) just before the plug' > '$m/powercut-$(date +%s).txt'; sync; sleep 1" || true
  python3 "$here/qmp.py" "$dir/qmp.sock" quit
  say "  plug pulled"
  sleep 5
  run boot >/dev/null
  wait_ssh; sleep 20
  if wait_ready 36; then say "  ready after the power cut"; else say "  NOT ready 3 minutes after the power cut"; fi
  observe
  vm 'journalctl --user -u cirroved.service -b --no-pager 2>/dev/null | grep -iE "recover|journal|replay|resum" | head -5; cirrove recent --limit 3' | tee -a "$log"
}

check_suspend() {
  local minutes=${CIRROVE_VM_SUSPEND_MINUTES:-95}
  say "suspend: the guest asleep for $minutes minutes, past the token's lifetime, then woken"
  vm 'sudo systemctl suspend' || true
  sleep "$((minutes * 60))"
  python3 "$here/qmp.py" "$dir/qmp.sock" wakeup
  say "  woken"
  sleep 15; wait_ssh
  observe
  if wait_ready 36; then say "  ready after the suspend"; else say "  NOT ready 3 minutes after waking"; fi
  observe
  vm 'journalctl --user -u cirroved.service --since -3min --no-pager 2>/dev/null | grep -iE "sign|token|refresh|updating|offline" | tail -5' | tee -a "$log"
}

say "exercise on $distro: ${checks[*]}"
run boot >/dev/null
wait_ssh
observe
for check in "${checks[@]}"; do "check_$check"; done
say "done; log and screenshots in $out"
