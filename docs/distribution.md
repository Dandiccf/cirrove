# Distribution and installation plan

Status: no tagged release. The Arch packages build from the committed tree
(see [Arch](#arch) below) and CI installs and removes them on a clean Arch
container on every push; Debian and Fedora packages do not exist yet.

Cirrove is a Linux application, not an Omarchy-specific service. The current
development machine uses Arch and CI uses Ubuntu 24.04. Building on Ubuntu is not
evidence that installation, login startup, keyring access or upgrades work there.
The earlier distribution roadmap listed only Arch/AUR; that was too narrow for
the intended Linux audience.

Distribution coverage and desktop coverage are separate release gates. Follow the
[cross-desktop checks](product-milestones.md#5-polished-desktop-experience) for
GNOME/Plasma, Wayland/X11, portals, tray and file-manager integration, and the
[platform constraints](product-milestones.md#platform-integration-constraints)
for the native host-service delivery model.

## Initial release targets

| Target | Native package and update route | Acceptance still required |
| --- | --- | --- |
| Arch / Omarchy | PKGBUILD, pacman package and AUR recipe | Clean build, install, login, upgrade and removal |
| Ubuntu LTS / Debian stable | `.deb`, Debian packaging source and signed APT update channel | Declared supported releases and clean desktop/VM installation matrix |
| Fedora | `.rpm`, spec/source RPM and COPR build/update channel | Declared supported Fedora releases and SELinux-enabled desktop/VM matrix |

Start with x86_64 across those families. Additional architectures and distributions
require their own build and runtime evidence; do not label an untested generic
binary as supported. Pin the actual release versions in the packaging matrix before
release. Respect the GTK 4.14/libadwaita 1.5 minimum and required FUSE kernel capability;
unsupported older distributions need an explicit explanation, not a silently broken
package or an ad-hoc replacement of system libraries.

Keep daemon/CLI packaging separable from the GTK desktop. Core package builds and
installations must not require desktop libraries: plain root
`cargo build --locked` now selects the non-GTK crates; explicit `--workspace`
still builds the desktop too. Clean package installation remains a release gate.
Native packages must resolve FUSE helpers, keyring and optional GTK/libadwaita
dependencies for the component being installed. Use
standard binary, desktop-entry and systemd user-unit locations. Package scripts
must not assume Omarchy, a particular shell, a logged-in user's home directory or
access to their session D-Bus. Avoid root-running account services.

APT/COPR availability is planned, not an assertion that repositories already exist.
Choose and validate the APT hosting/build service and signing-key procedure during
milestone 6. COPR is a Fedora community build/update service, not inclusion in
Fedora's official package repositories. The comparison motivating this work is
supported by [onedriver's installation documentation](https://github.com/jstaf/onedriver#quick-start),
which describes COPR and Debian/Ubuntu package-manager installation through OBS.
Its documentation alone does not verify every listed distribution's current builds.

## Arch

`packaging/arch/PKGBUILD` builds two packages from one source, because a
headless host should be able to install the daemon without a desktop library:

| Package | Contents | Depends on |
| --- | --- | --- |
| `cirrove` | `/usr/bin/cirroved`, `/usr/bin/cirrove`, `/usr/lib/systemd/user/cirroved.service`, licence | `fuse3` (for `fusermount3`); optionally a Secret Service keyring and `xdg-utils` |
| `cirrove-desktop` | `/usr/bin/cirrove-desktop`, `/usr/bin/cirrove-tray`, the desktop entry, the tray's `/etc/xdg/autostart` entry, the hicolor icons, the AppStream metainfo, the Files extension under `/usr/share/nautilus-python/extensions/`, licence | `cirrove`, `gtk4`, `libadwaita`; optionally the GNOME AppIndicator extension and `nautilus-python` for the Files badges and menu |

The PKGBUILD is written for a tagged release and downloads the tarball by
version. There is no tag yet, so `scripts/build-arch-package.sh` builds HEAD: it
archives the commit under the name the source line expects, sets `pkgver` to
`0.1.0dev.r<commits>.g<hash>` (which sorts before `0.1.0` for pacman and
upgrades from one dev build to the next), and points makepkg at the archive.
It then lists each package and fails if a file is missing or the daemon package
has acquired a desktop dependency. It builds the commit, not the working copy;
uncommitted edits are not in the package, and it says so.

```sh
scripts/build-arch-package.sh
sudo pacman -U target/arch/cirrove-*.pkg.tar.zst   # the two it names, not the -debug ones
systemctl --user enable --now cirroved.service
cirrove status
```

The packaged unit runs `/usr/bin/cirroved`; the template in `packaging/systemd/`
runs `%h/.local/bin/cirroved` for a developer install, and `package()` rewrites
the path and checks the rewrite took. **Moving from a developer install to the
package**: the user-local files shadow the packaged ones, so remove them first --
`~/.config/systemd/user/cirroved.service` (after `systemctl --user disable --now
cirroved`), the binaries in `~/.local/bin/`, and
`~/.config/autostart/io.github.Dandiccf.Cirrove.Tray.desktop`
(`scripts/install-tray-autostart.py --remove`) -- then `systemctl --user
daemon-reload` and enable the packaged unit. Accounts, credentials and the
journal under `~/.local/state/cirrove` are not touched by any of this.

`pacman -R cirrove-desktop cirrove` removes only package-owned files. The state
directory, the keyring entries and any unsent bytes in the journal stay, which
is the retention the milestone asks for; removing them is a separate, explicit
step (`cirrove forget` per account, or deleting the state directory) and never
happens through a mounted path.

CI's `arch-package` job does the build in an `archlinux:base-devel` container as
an unprivileged user with the distribution's rust, installs both packages,
runs the binaries, validates the desktop entries and metainfo from their
installed locations, removes the packages, and checks nothing package-owned
survived. A container has no login session: it shows the packages are correct,
not that the service starts at login. The AUR recipe and `.SRCINFO` follow the
first tag.

## Real desktops, in virtual machines

CI's containers prove the packages; they have no login session, no display
manager, no shell to show a tray in. For that there are throwaway desktop
machines on the development host, installed without a hand on them and
checked by a script, so the run is the same each time and a person's
attention goes to the screenshots rather than the clicking.

- `scripts/vm/run.sh <ubuntu|fedora|arch> install` installs Ubuntu 24.04 Desktop
  (Subiquity autoinstall) or Fedora Workstation (anaconda kickstart) into a
  QEMU/KVM machine: nothing needs root, the installer's answers are served
  over HTTP on the address the guest sees as its gateway, the kernel and
  initrd come out of the ISO with `bsdtar` so no boot menu is ever driven.
  The test account logs in automatically and can sudo without a password;
  the machine is not meant to be kept. Arch is the third, and the odd one:
  archiso has no kickstart and no autoinstall, and its `script=` boot
  parameter is the only unattended hook there is -- `scripts/vm/arch/install.sh`
  is what it runs.
- `scripts/vm/check.sh <ubuntu|fedora|arch>` boots it, installs the packages CI
  built,
  starts the service as the user, starts the tray the way the autostart
  entry will and reads it back from the shell's StatusNotifierWatcher, opens
  Files with the extension, reboots and looks again, removes the packages and
  checks nothing package-owned survived -- a screenshot at each stage,
  under `~/Work/cirrove-vms/<distro>/checks/`. It clears the machine's package
  directory before downloading, because one checked before still holds the
  previous run's packages: `pacman -U ./pkgs/*` refuses the lot as duplicate
  targets, and apt and dnf would silently install the newest rather than what
  was just fetched.
- `scripts/vm/qmp.py` is the hand on the machine: a screenshot, a key, a
  click, the power button, through QEMU's monitor socket.

What the checks cannot do is sign in: that is a person's browser and
credentials. Run the machine with a window (`CIRROVE_VM_DISPLAY=gtk
scripts/vm/run.sh <distro> boot`), sign in there, and the tray -- Passive
without an account, which a shell hides -- gets its icon. One thing the
first attempt found: the test account logs in automatically, so no keyring
is ever unlocked and there is no default one to keep the grant in --
`echo -n cirrove | gnome-keyring-daemon --unlock` in the session creates it,
and the same is true of any real machine with automatic login, which the
user guide now says.

**Fedora 44 Workstation, 2026-09-13:** everything above passed on a clean
machine: the fc42-built rpms install on 44, the service runs as the user,
the tray registers with GNOME's watcher through the AppIndicator extension
(installed by the package's recommendation, enabled by the user -- or here,
by the check in the user's place), Files loads the extension, the tray comes
back after a reboot from the packaged autostart entry, `dnf remove` leaves
the state directory and nothing else. Not seen: an icon in the top bar,
for want of an account. Fedora 44 is what "current Fedora" meant on the day;
CI's container is 42, and the packages built there installed on 44 without
complaint.

**Arch, 2026-09-15:** the same again on a clean Arch machine, this time from a
script rather than by hand: the CI-built packages install with pacman, both
desktop entries and the metainfo validate, the service runs as the user, the
tray names its own icon, all of it is back after a reboot from the packaged
autostart alone, and `pacman -Rns` leaves nothing named cirrove under `/usr` or
`/etc`. The two desktop dependencies are `optdepends` here and are absent before
and after, which is the distribution's convention -- pacman prints them at
install time -- and is now visible in every run rather than in one person's
notes.

**Ubuntu 24.04.4 Desktop, 2026-09-13:** the same, on a clean machine
installed by autoinstall: the CI-built debs install with apt, the service
runs as the user, the tray registers with the watcher Ubuntu's own
AppIndicator extension provides (enabled in the Ubuntu session by default,
so nothing to enable), Files loads the extension, the tray comes back after
a reboot, `apt purge` leaves the state directory and nothing else. The tray
was Passive throughout, again for want of an account.

## Dependency and security review

What is reviewed, and where the review is repeated so it does not go stale:

- **Advisories.** `cargo audit` against the RustSec database, in CI on every
  push (`dependency-audit`). One advisory is passed over, with its reason in
  `.cargo/audit.toml`: RUSTSEC-2023-0071, a timing side channel in RSA
  private-key operations in the `rsa` crate, which reaches the tree through
  `openidconnect` for verifying ID-token signatures -- public-key operations;
  Cirrove holds no RSA private key. No fixed release exists yet.
- **Licences.** 416 crates outside the workspace, every one under a licence
  the binaries may be distributed under (MIT, Apache-2.0, BSD, ISC, Zlib,
  Unicode-3.0, CDLA-Permissive-2.0 and their combinations; the single crate
  that offers LGPL offers it as one alternative among MIT and Apache-2.0).
  `scripts/licence-check.py` evaluates each crate's SPDX expression against
  the permissive list and fails CI on anything else, so a copyleft dependency
  arriving through a transitive bump is a failure and not a surprise at
  release time.
- **Unsafe code.** `unsafe_code = "forbid"` across the workspace; the FUSE,
  SQLite and TLS surfaces are reached through crates that carry their own
  unsafe, not through any of ours.
- **What the binaries touch.** Tokens live only in the desktop keyring
  (Secret Service); the control socket is under `$XDG_RUNTIME_DIR` with mode
  0700 on its directory; the state directory is 0700; the mount is
  user-private FUSE. The daemon runs as the user, never as root, and the
  package installs no setuid binary of its own (`fusermount3` is the
  distribution's).

Not done: a review of the update channels' signing (there are none yet), and
a third party's reading of any of this.

## What blocks the first release, and what does not

Every open row in the [acceptance ledger](acceptance-ledger.json) carries a
decision -- `blocks_release` is `yes` or `no`, and a `no` carries its reason --
so "is it ready" has one answer rather than fifty-four, and the answer cannot
go stale. `scripts/acceptance-ledger.py --blockers` prints both lists, and the
script refuses an open row with no decision and a `no` with no reason. This
paragraph used to be the list itself, written by hand, and it had already
drifted: it named localization and the recent-activity view as not blocking
after both were done.

**The test is not "is this unfinished"** -- everything open is -- **but "would
a person reasonably say you shipped 1.0 with this broken".** A row blocks when
the core job needs a capability that is not there: connecting a drive, seeing
and opening files, keeping some offline, being told the truth about what has
and has not reached the cloud, installing and removing cleanly on a declared
platform. A row does not block when the capability exists and is asserted and
only an outside verification is missing, when it is breadth beyond the agreed
scope, or when its absence is honestly written down where a user will meet it.

As of 2026-09-14 that is 22 blocking and 12 not, against 20 rows closed. The
shape of it: all seven of milestone 1, because a filesystem that has not been
run for a day and through a suspend is not a thing to hand people; three of
milestone 4, deletion and the compatibility matrix among them; seven of
milestone 5, the session matrix and actionable errors among them; and five of
milestone 6, ending with the tag itself.

## Supported versions

There is no release yet. Until there is, what is supported is the current
build of the main branch on Arch, on the machine it is developed on. From the
first release on: the latest release and the one before it, for the length of
one release cycle; the distribution floor is Ubuntu 24.04 (GTK 4.14,
libadwaita 1.5), current Fedora and current Arch, x86_64 only. Anything older
or elsewhere may work and is not claimed. The same policy, for users, is in
the [user guide](user-guide.md#supported-versions).

## Milestone-6 acceptance gates

- [ ] Build native packages from the same release tag and locked source/dependency
      inputs in clean environments; publish source, checksums and provenance.
      Record binary reproducibility results, rather than calling a Cargo lockfile
      alone a reproducible distribution.
- [ ] Verify dependencies and file ownership with distro-native package linting.
      Package transactions must not need a compiler on the end user's computer.
- [ ] Install the published packages on clean Arch, Ubuntu/Debian and Fedora
      desktops/VMs. Validate graphical sign-in, keyring, FUSE mount, Files access,
      user-service login startup and reboot. Container build success is insufficient.
- [ ] Validate upgrades from the previous supported release, including schema
      compatibility and recovery of pending writes after interruption. Define
      rollback behavior before enabling package updates.
- [ ] Verify update delivery through AUR/APT/COPR and supported-version policy.
      Test repository signing/key rotation procedures and artifact verification.
- [ ] Exercise Fedora with SELinux enabled and supported Ubuntu security defaults;
      do not solve packaging failures by disabling system protections.
- [ ] Uninstall package-owned files cleanly while retaining accounts, credentials
      and unsent bytes unless the user explicitly chooses data removal. Never
      delete cloud data through a mounted path in package scripts.
- [ ] Publish distro-specific installation, updates, diagnostics and removal steps
      alongside the same tagged release. Record unsupported versions/architectures.

Arch remains a development target, not the sole release gate. General Linux
OneDrive 1.0 requires the declared native-package matrix to pass. Portable desktop
formats can be evaluated later, but do not replace validating a host FUSE daemon,
its user service and file-manager integration.
