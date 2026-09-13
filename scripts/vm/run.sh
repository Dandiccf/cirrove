#!/usr/bin/env bash
# A throwaway desktop VM for the real-desktop checks in docs/distribution.md:
# Ubuntu 24.04 or Fedora, installed without a hand on it, then booted from its
# own disk so the packages CI built can be installed, the session looked at,
# and the machine rebooted.
#
# Nothing here needs root: QEMU with KVM (/dev/kvm is world-usable on the
# development machine), user-mode networking with ssh forwarded to a local
# port, the installer's answers served over HTTP on the user network's gateway
# address, and the kernel and initrd pulled out of the ISO with bsdtar so the
# boot menu never has to be driven.
#
# Usage:
#   scripts/vm/run.sh ubuntu install   # unattended install; exits when done
#   scripts/vm/run.sh ubuntu boot      # boot the installed disk (background)
#   scripts/vm/run.sh ubuntu stop      # power it off
#   scripts/vm/run.sh ubuntu ssh CMD   # run a command in it
#   scripts/vm/run.sh ubuntu shot X    # screenshot to X.png via QMP
#   (fedora likewise)
#
# Layout under $CIRROVE_VMS (default ~/Work/cirrove-vms):
#   iso/            the installer images
#   pkgs/deb, pkgs/rpm   the packages to test (CI artefacts)
#   <distro>/       disk.qcow2, kernel, initrd, serial.log, qmp.sock, pids
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
vms=${CIRROVE_VMS:-$HOME/Work/cirrove-vms}
distro=${1:?ubuntu or fedora}
action=${2:?install, boot, stop, ssh or shot}
shift 2 || true
dir="$vms/$distro"
mkdir -p "$dir"

case $distro in
  ubuntu)
    iso=$(ls "$vms"/iso/ubuntu-24.04.*-desktop-amd64.iso 2>/dev/null | tail -1)
    kernel_in_iso=casper/vmlinuz; initrd_in_iso=casper/initrd
    seed="$here/ubuntu"
    # Subiquity looks for user-data and meta-data under the seed URL.
    http_port=8000
    append="autoinstall ds=nocloud-net;s=http://10.0.2.2:$http_port/ console=ttyS0"
    ssh_port=2222; vnc=10
    ;;
  fedora)
    iso=$(ls "$vms"/iso/Fedora-Everything-netinst-x86_64-*.iso 2>/dev/null | tail -1)
    kernel_in_iso=images/pxeboot/vmlinuz; initrd_in_iso=images/pxeboot/initrd.img
    seed="$here/fedora"
    label=$(python3 -c "f=open('$iso','rb'); f.seek(32768+40); print(f.read(32).decode().strip())")
    http_port=8001
    append="inst.ks=http://10.0.2.2:$http_port/ks.cfg inst.stage2=hd:LABEL=$label inst.text console=ttyS0"
    ssh_port=2223; vnc=11
    ;;
  *) echo "unknown distro $distro" >&2; exit 1 ;;
esac

qemu_common=(
  qemu-system-x86_64 -enable-kvm -cpu host -m 8G -smp 4
  -drive "file=$dir/disk.qcow2,if=virtio,format=qcow2"
  -netdev "user,id=n0,hostfwd=tcp:127.0.0.1:$ssh_port-:22"
  -device virtio-net-pci,netdev=n0
  -device virtio-vga
  -display none -vnc "127.0.0.1:$vnc"
  -qmp "unix:$dir/qmp.sock,server,nowait"
  -serial "file:$dir/serial.log"
  -pidfile "$dir/qemu.pid"
)

serve() {
  # The installer's answers, and the packages, on the gateway the guest sees.
  # One directory: the seed files at the top, pkgs/ beside them.
  local root="$dir/www"
  rm -rf "$root"; mkdir -p "$root"
  cp "$seed"/* "$root/"
  ln -s "$vms/pkgs" "$root/pkgs"
  # One port per distro, so both machines can install at once.
  (cd "$root" && python3 -m http.server "$http_port" --bind 127.0.0.1 >"$dir/http.log" 2>&1 &
   echo $! > "$dir/http.pid")
}
unserve() {
  [[ -f $dir/http.pid ]] && kill "$(cat "$dir/http.pid")" 2>/dev/null || true
  rm -f "$dir/http.pid"
}

case $action in
  install)
    [[ -f $iso ]] || { echo "no installer image for $distro under $vms/iso" >&2; exit 1; }
    rm -f "$dir/disk.qcow2"
    qemu-img create -f qcow2 "$dir/disk.qcow2" 40G >/dev/null
    (cd "$dir" && bsdtar -xf "$iso" "$kernel_in_iso" "$initrd_in_iso")
    serve
    trap unserve EXIT
    echo "installing $distro from $(basename "$iso"); serial console in $dir/serial.log"
    # -no-reboot: the installer's final reboot ends the process, which is how
    # this knows the install is over.
    "${qemu_common[@]}" -cdrom "$iso" \
      -kernel "$dir/$kernel_in_iso" -initrd "$dir/$initrd_in_iso" \
      -append "$append" -no-reboot
    echo "installer finished; boot it with: $0 $distro boot"
    ;;
  boot)
    [[ -f $dir/disk.qcow2 ]] || { echo "no installed disk; run install first" >&2; exit 1; }
    serve
    "${qemu_common[@]}" -daemonize
    echo "$distro booting; ssh -p $ssh_port tester@127.0.0.1, vnc 127.0.0.1:$vnc"
    ;;
  stop)
    if [[ -f $dir/qemu.pid ]]; then
      python3 "$here/qmp.py" "$dir/qmp.sock" system_powerdown || true
      for _ in $(seq 1 60); do kill -0 "$(cat "$dir/qemu.pid" 2>/dev/null)" 2>/dev/null || break; sleep 1; done
      kill "$(cat "$dir/qemu.pid" 2>/dev/null)" 2>/dev/null || true
      rm -f "$dir/qemu.pid"
    fi
    unserve
    echo "$distro stopped"
    ;;
  ssh)
    exec ssh -p "$ssh_port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      -o LogLevel=ERROR -o ConnectTimeout=10 tester@127.0.0.1 "$@"
    ;;
  shot)
    out=${1:?output path without extension}
    python3 "$here/qmp.py" "$dir/qmp.sock" screendump "$out.png"
    echo "$out.png"
    ;;
  *) echo "unknown action $action" >&2; exit 1 ;;
esac
