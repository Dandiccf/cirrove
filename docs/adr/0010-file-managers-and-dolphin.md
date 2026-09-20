# 0010: Dolphin was out of 1.0

Status: superseded on 2026-09-20 by the implementation described below.

## 2026-09-20: decision revisited

The circumstances behind this decision changed. The first release is tagged,
the provider-neutral reshaping and the Google Drive adapter have landed, and a
user explicitly asked for Dolphin integration. The implementation now has an
owner and an isolated KF6 CMake target, an Arch/Fedora package boundary and a
dedicated KF6 CI job, so KDE libraries remain outside the daemon and GTK
packages.

Dolphin is therefore supported through two KF6 plugins:

- a `KFileItemActionPlugin` adds **Keep offline** and **Stop keeping offline**
  only for selections inside a mounted Cirrove account;
- a `KOverlayIconPlugin` shows the same kept/fetching badges as Files and
  follows the daemon's `kept_generation` event so an open view does not stay
  stale.

Both are clients of the existing provider-neutral `status`, `paths`, `pin`,
`unpin` and `subscribe` contracts. They contain no OneDrive or Google Drive
branch. Dolphin columns and a custom properties page are still absent; those
are presentation extras, while offline state and control are present. KF5 is
not targeted. The plugins compile and their selection, path-boundary, badge
and metadata behavior have automated KF6 coverage; a real Dolphin session is
still an acceptance check rather than a claim made here.

## What was already decided, and what was not

[The desktop notes](../desktop.md) already establish *how* Dolphin integration
would have to be built, and did the legwork properly: service menus were
checked rather than assumed, the KF6 condition keys were read out of the
installed libraries, and the conclusion was that nothing there tests a path or
asks a running service -- so a service menu would offer "Keep offline" on every
file on the machine. Actions need a `KFileItemActionPlugin`, badges a
`KOverlayIconPlugin`, a column a KFileMetaData extractor, all compiled against
KF6.

What that leaves unanswered is the only question M5 line 292 actually asks for
an explicit answer to: **is it in 1.0?** "Decided, not done -- the plugin is now
the only thing outstanding" is a plan, not a scope decision, and a plan that
sits in a milestone list is how a release slips without anyone choosing that it
should.

## The evidence that decided it

The cost is not writing the plugin. It is owning a C++ plugin ABI across KDE
major versions, and there is a public record of what that bill looks like:

- Nextcloud's Dolphin overlay icons stopped rendering under Plasma 6 and KF6,
  and the issue stayed open while users saw sync state silently disappear.
- Insync's Dolphin plugin needed a separate repository for Plasma 6; the
  KDE 5 one does not work on 6, and the 6 one does not work on 5.
- Even the service-menu directory moved between Plasma 5 and 6.

Cirrove's entire file-manager surface today is one Python file with no build
step. The first C++/CMake/KF6 target brings a second toolchain into the build,
CI and three packaging formats -- and brings it permanently, because the
distributions will move again.

Against that: Cirrove has no users on Plasma yet, because it has no release.
Buying a maintenance obligation to serve a population that does not exist, in
the release where the account file, the auth crate and the status wire format
are all due to be reshaped for a second provider ([ADR 0009](0009-a-second-provider.md)),
is the wrong order of work.

## Decision

**Dolphin has no plugin in 1.0.** It is not "outstanding"; it is out, and the
milestone should say so rather than carry it as unfinished business.

Revisit when all three hold:

1. 1.0 is released, so the ADR 0009 reshaping is not happening concurrently.
2. There is evidence of Plasma users reaching for Cirrove, rather than an
   assumption that they would.
3. Someone will own a C++ target across a KDE major bump, because that bill
   arrives whether or not anyone planned for it.

## Why this costs a Dolphin user little

The mount is a filesystem, so every file manager can use it -- Dolphin,
Konqueror, Krusader, Thunar, a terminal, any application's open dialog --
reading, writing, renaming, moving and deleting, with nothing installed. No
file is gated behind a file manager.

What Dolphin does not get is the state badge and the right-click pin. Both are
in the Cirrove window and in `cirrove pin` / `unpin` / `pins` / `paths`, for
every file on every desktop. That is the standing rule from M5 rather than a
consolation: no control and no state may be reachable only through one file
manager, which is exactly what makes this deferral affordable.

The daemon side needs nothing. The Nautilus extension is a client of the
control socket -- one line of JSON in, one out, the same contract the CLI and
the window speak -- so a future Dolphin plugin is a fourth client of an
existing contract, not a change to the daemon.

## Sources

- [KOverlayIconPlugin](https://api.kde.org/koverlayiconplugin.html)
- [Creating Dolphin service menus](https://develop.kde.org/docs/apps/dolphin/service-menus/)
- [Nextcloud desktop issue 6577: Plasma 6/KF6 overlay icons not rendering](https://github.com/nextcloud/desktop/issues/6577)
- [dolphin-insync-plugin-plasma-6](https://github.com/kevinbburns/dolphin-insync-plugin-plasma-6)
