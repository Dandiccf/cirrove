#!/usr/bin/env bash
# Arch, installed without a hand on it, for the clean-system checks in
# docs/distribution.md. Arch is the development family and the one family whose
# package had never been installed on a machine that was not this one.
#
# archiso runs this at boot when the kernel is given
# script=http://10.0.2.2:8002/install.sh -- that hook is the whole reason this
# can be unattended; Arch has no kickstart and no autoinstall.
#
# A throwaway machine: the test account's password is the word "cirrove" so a
# person can log in at the console when something goes wrong.
set -euxo pipefail
exec > >(tee /dev/ttyS0) 2>&1

disk=/dev/vda
# GPT with a BIOS boot partition and one root. BIOS rather than UEFI to match
# the Ubuntu and Fedora machines, which boot the same way -- this measures a
# package installation, and firmware is not the variable under test. No swap
# for the same reason.
sgdisk --zap-all "$disk"
sgdisk -n1:0:+1M -t1:ef02 -c1:bios -n2:0:0 -t2:8300 -c2:root "$disk"
mkfs.ext4 -F "${disk}2"
mount "${disk}2" /mnt

# reflector picks mirrors by speed; the guest's network is user-mode NAT, so
# take the geo mirror directly rather than waiting on a ranking.
echo 'Server = https://geo.mirror.pkgbuild.com/$repo/os/$arch' > /etc/pacman.d/mirrorlist

# GNOME rather than a bare system: the row this serves needs a session to sign
# in from. gnome-shell-extension-appindicator is NOT installed -- the package
# must bring its own desktop integration, the same rule the Fedora kickstart
# now follows. Nor is fuse3, which the package declares.
# A browser, because a sign-in needs one and this machine exists to be signed
# in to: on 2026-09-15 the owner reached the Sign in with Microsoft button on a
# machine with neither a browser nor xdg-open, which is a fixture gap and, as it
# turned out, a packaging one as well. wl-clipboard so a long client id can be
# pasted rather than typed. Deliberately NOT xdg-utils: the package under test
# requires it now, and installing it here would hide that the next time.
pacstrap -K /mnt base linux linux-firmware grub \
  gnome-shell gdm nautilus gnome-console firefox wl-clipboard \
  networkmanager openssh sudo python

genfstab -U /mnt >> /mnt/etc/fstab

arch-chroot /mnt /bin/bash -euxo pipefail <<'CHROOT'
ln -sf /usr/share/zoneinfo/UTC /etc/localtime
hwclock --systohc
echo 'en_US.UTF-8 UTF-8' > /etc/locale.gen
# The window's German is worth being able to see on this machine too.
echo 'de_DE.UTF-8 UTF-8' >> /etc/locale.gen
locale-gen
echo 'LANG=en_US.UTF-8' > /etc/locale.conf
echo cirrove-arch > /etc/hostname

useradd -m -G wheel -s /bin/bash tester
echo 'tester:cirrove' | chpasswd
passwd -l root
echo 'tester ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/tester
chmod 440 /etc/sudoers.d/tester

systemctl enable NetworkManager sshd gdm
# Log the test user in at boot: the session is what is under test.
mkdir -p /etc/gdm
printf '[daemon]\nAutomaticLoginEnable=True\nAutomaticLogin=tester\n' > /etc/gdm/custom.conf

# The serial console is how the host watches an unattended boot.
sed -i 's/^GRUB_CMDLINE_LINUX_DEFAULT=.*/GRUB_CMDLINE_LINUX_DEFAULT="loglevel=3 console=ttyS0"/' /etc/default/grub
sed -i 's/^GRUB_TIMEOUT=.*/GRUB_TIMEOUT=0/' /etc/default/grub
grub-install --target=i386-pc /dev/vda
grub-mkconfig -o /boot/grub/grub.cfg
CHROOT

echo "CIRROVE-ARCH-INSTALL-COMPLETE"
sync
# -no-reboot means powering off is how the host knows the install is over.
systemctl poweroff
