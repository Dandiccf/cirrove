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
| `cirrove-desktop` | `/usr/bin/cirrove-desktop`, `/usr/bin/cirrove-tray`, the desktop entry, the tray's `/etc/xdg/autostart` entry, the hicolor icons, the AppStream metainfo, licence | `cirrove`, `gtk4`, `libadwaita`; optionally the GNOME AppIndicator extension |

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
