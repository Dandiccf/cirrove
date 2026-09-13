#!/usr/bin/env bash
# Unlock a VM's login keyring, from the host.
#
# The VM's test account logs in automatically, so nothing unlocks its keyring
# and the Cirrove daemon cannot read the OneDrive grant -- the account sits in
# sign_in_required with a perfectly good token it cannot see. This asks the
# session's own Secret Service to unlock, which raises the session's password
# prompt, and types the password into that prompt through QEMU's monitor: the
# machine's equivalent of a person at the login screen.
#
# exercise.sh has always done this inline; it is a script of its own because
# every other use of a booted VM needs it too, and because running
# unlock-keyring.py on the host by mistake unlocks nothing and says so in a way
# that is easy to miss ("unlocked [] prompt /").
#
# Usage: scripts/vm/unlock.sh <distro> [password]      default password: cirrove
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
distro=${1:?usage: unlock.sh <distro> [password]}
password=${2:-cirrove}
dir="$HOME/Work/cirrove-vms/$distro"
vm() { bash "$here/run.sh" "$distro" ssh "$@"; }

locked() {
  vm 'export XDG_RUNTIME_DIR=/run/user/$(id -u) DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus
      busctl --user introspect org.freedesktop.secrets /org/freedesktop/secrets/collection/login 2>/dev/null |
        grep -q "\.Locked.*false"' 2>/dev/null
}

if locked; then echo "keyring already unlocked"; exit 0; fi

vm 'cat > /tmp/unlock-keyring.py' < "$here/unlock-keyring.py"
vm 'export XDG_RUNTIME_DIR=/run/user/$(id -u) DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus
    for _ in 1 2 3 4 5 6; do busctl --user status org.freedesktop.secrets >/dev/null 2>&1 && break; sleep 5; done
    nohup setsid python3 /tmp/unlock-keyring.py >/tmp/unlock-keyring.out 2>&1 </dev/null &
    sleep 4' || true

# One key at a time; the prompt is a GTK dialog on the session's screen.
for (( i=0; i<${#password}; i++ )); do
  python3 "$here/qmp.py" "$dir/qmp.sock" keys "${password:$i:1}" >/dev/null 2>&1 || true
done
python3 "$here/qmp.py" "$dir/qmp.sock" keys ret >/dev/null 2>&1 || true
sleep 3

if locked; then
  echo "keyring unlocked"
else
  echo "keyring still locked -- what the unlock asked for:" >&2
  vm 'cat /tmp/unlock-keyring.out 2>/dev/null' >&2 || true
  exit 1
fi
