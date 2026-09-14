# Two packages from one build, the same split as packaging/arch/PKGBUILD:
# cirrove (daemon, CLI, user unit) and cirrove-desktop (window, tray, entries,
# icons, metainfo, Files extension). scripts/build-rpm-package.sh rewrites
# Version with the commit it builds and supplies the tarball.
#
# The build uses the cargo on PATH -- a rustup toolchain in CI, since the
# crates need rust 1.98 -- and fetches crates under the lockfile. A package for
# COPR would vendor them and use the %%cargo_* macros; that is the update-
# channel work, not this.

%global debug_package %{nil}
%global app_id io.github.Dandiccf.Cirrove

Name:           cirrove
Version:        0.1.0~dev
Release:        1%{?dist}
Summary:        Cloud drive filesystem that keeps the cloud where it is
License:        Apache-2.0
URL:            https://github.com/Dandiccf/cirrove
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  gcc
BuildRequires:  cmake
BuildRequires:  pkgconf-pkg-config
BuildRequires:  gtk4-devel >= 4.14
BuildRequires:  libadwaita-devel >= 1.5
BuildRequires:  gettext
Requires:       fuse3
Recommends:     gnome-keyring
Recommends:     xdg-utils
Suggests:       %{name}-desktop

%description
Cirrove mounts a cloud drive -- OneDrive first -- as a filesystem that
downloads files when they are opened and uploads changes in the background,
without syncing everything. This package is the daemon, its user service unit
and the command line; it needs no desktop library.

%package        desktop
Summary:        Settings window, tray and desktop integration for Cirrove
Requires:       %{name} = %{version}-%{release}
Requires:       gtk4 >= 4.14
Requires:       libadwaita >= 1.5
Recommends:     nautilus-python
Recommends:     gnome-shell-extension-appindicator

%description    desktop
The settings window, the tray, the desktop entry, the icons, the AppStream
metainfo and the Files extension for Cirrove.

%prep
%autosetup -n %{name}-%{version}

%build
# --workspace: the root's default members exclude the desktop so that a plain
# build needs no GTK; the package builds both halves.
cargo build --locked --release --workspace

%install
install -Dm755 target/release/cirroved %{buildroot}%{_bindir}/cirroved
install -Dm755 target/release/cirrove %{buildroot}%{_bindir}/cirrove
# The unit in the tree runs %%h/.local/bin/cirroved for a developer install; a
# package puts the binary in /usr/bin and the unit must say so.
mkdir -p %{buildroot}%{_userunitdir}
sed 's|%%h/.local/bin/cirroved|/usr/bin/cirroved|' packaging/systemd/cirroved.service \
  > %{buildroot}%{_userunitdir}/cirroved.service
grep -q '^ExecStart=/usr/bin/cirroved ' %{buildroot}%{_userunitdir}/cirroved.service

install -Dm755 target/release/cirrove-desktop %{buildroot}%{_bindir}/cirrove-desktop
install -Dm755 target/release/cirrove-tray %{buildroot}%{_bindir}/cirrove-tray
install -Dm644 packaging/desktop/%{app_id}.desktop -t %{buildroot}%{_datadir}/applications/
install -Dm644 packaging/desktop/%{app_id}.Tray.desktop -t %{buildroot}%{_sysconfdir}/xdg/autostart/
install -Dm644 packaging/icons/scalable/apps/*.svg -t %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/
install -Dm644 packaging/icons/symbolic/apps/*.svg -t %{buildroot}%{_datadir}/icons/hicolor/symbolic/apps/
install -Dm644 packaging/metainfo/%{app_id}.metainfo.xml -t %{buildroot}%{_metainfodir}/
install -Dm644 packaging/nautilus/cirrove.py -t %{buildroot}%{_datadir}/nautilus-python/extensions/
# Translations. The window binds its catalogue relative to its own binary, so
# /usr/bin/cirrove-desktop finds /usr/share/locale without being told.
for po in po/*.po; do
  lang=$(basename "$po" .po)
  install -d %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES
  msgfmt --check -o %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES/cirrove.mo "$po"
done

%files
%license LICENSE
%{_bindir}/cirroved
%{_bindir}/cirrove
%{_userunitdir}/cirroved.service

%files desktop
%license LICENSE
%{_bindir}/cirrove-desktop
%{_bindir}/cirrove-tray
%{_datadir}/applications/%{app_id}.desktop
%config %{_sysconfdir}/xdg/autostart/%{app_id}.Tray.desktop
# Globbed to match the install above. Enumerating them meant that adding an
# icon failed the build with "installed but unpackaged", which is a true
# statement about the spec and a confusing one about the change. What each
# package must actually contain is asserted by scripts/build-rpm-package.sh.
%{_datadir}/icons/hicolor/scalable/apps/%{app_id}*.svg
%{_datadir}/icons/hicolor/symbolic/apps/%{app_id}*.svg
%{_metainfodir}/%{app_id}.metainfo.xml
%{_datadir}/nautilus-python/extensions/cirrove.py
%{_datadir}/locale/*/LC_MESSAGES/cirrove.mo

%changelog
* Sat Sep 13 2026 Christian Dandachi <dandiccf@users.noreply.github.com> - 0.1.0~dev-1
- Development build from the tree.
