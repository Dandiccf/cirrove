# Distribution and installation plan

Status: planned; no installable OneDrive 1.0 release is available.

Cirrove is a Linux application, not an Omarchy-specific service. The current
development machine uses Arch and CI uses Ubuntu 24.04. Building on Ubuntu is not
evidence that installation, login startup, keyring access or upgrades work there.
The earlier distribution roadmap listed only Arch/AUR; that was too narrow for
the intended Linux audience.

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

Keep daemon/CLI packaging separable from the GTK desktop. Native package managers
must resolve FUSE helpers, GTK/libadwaita and desktop/keyring dependencies. Use
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
