# Throwaway desktop VMs

Ubuntu 24.04 and Fedora Workstation VMs for the real-desktop checks that CI's
containers cannot do: install a package, log in, look at the tray and Files,
reboot, pull the plug. Nothing here needs root -- QEMU/KVM, user-mode
networking, ssh on a forwarded port.

## Where everything lives

Under `~/Work/cirrove-vms/` (not in the repo -- large disk images):

| Path | What |
| --- | --- |
| `iso/` | the installer images |
| `pkgs/deb`, `pkgs/rpm` | the packages under test (downloaded from a CI run) |
| `ubuntu/disk.qcow2` | the installed Ubuntu machine -- **has a signed-in OneDrive** |
| `fedora/disk.qcow2` | the installed Fedora machine (no account signed in) |
| `id_ed25519`, `id_ed25519.pub` | the ssh key `run.sh` uses to log in |

## Credentials

- **Test user:** `tester`  ·  **password:** `cirrove`  (sudo without a password)
- **ssh key:** `~/Work/cirrove-vms/id_ed25519` (passwordless ssh after `run.sh <distro> key`)
- **Keyring password:** also `cirrove`. The account logs in automatically, so
  nothing unlocks the keyring on boot and the Cirrove daemon cannot read the
  OneDrive token until it is unlocked -- `scripts/vm/exercise.sh` does this
  after every boot (`unlock-keyring.py` asks the Secret Service to unlock and
  types the password into its prompt through the QEMU monitor). By hand:
  `echo -n cirrove | gnome-keyring-daemon --unlock` from inside the session, or
  just log in once with the password.
- **The OneDrive sign-in** in the Ubuntu VM is the machine owner's own Microsoft
  account. It is stored in the VM's keyring, not written down anywhere; it
  survives in `ubuntu/disk.qcow2`, so the drive is there again after a boot
  (once the keyring is unlocked).

## Reusing them later

```sh
scripts/vm/run.sh ubuntu boot          # headless (ssh + vnc)
CIRROVE_VM_DISPLAY=gtk scripts/vm/run.sh ubuntu boot   # in a window
scripts/vm/run.sh ubuntu ssh 'cirrove status'
scripts/vm/run.sh ubuntu stop          # power off cleanly
```

Ports: ssh **2222** (ubuntu) / **2223** (fedora); vnc **127.0.0.1:10** / **:11**;
the installer answer files are served on http **8000** / **8001** during an
install. `run.sh <distro> shot OUT` screenshots via the monitor;
`scripts/vm/qmp.py` sends keys, clicks, `link off/on`, `wakeup`, `quit`.

To re-run the checks: `scripts/vm/check.sh <distro>` (install/tray/Files/reboot/
remove) or, on a VM with a signed-in account, `scripts/vm/exercise.sh ubuntu`
(reboot, outage, crash, power cut; add `suspend` for the token-lifetime run).

## Reinstalling from scratch

`scripts/vm/run.sh <distro> install` wipes `<distro>/disk.qcow2` and installs
again, unattended, from `iso/`. The account and everything else are gone; you
sign in again in the window (`CIRROVE_VM_DISPLAY=gtk ... boot`).
