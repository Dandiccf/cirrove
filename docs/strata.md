# Strata integration preview

This integration requires the external file-provider API in the companion Strata
change. Stock Strata 0.20.1 cannot load it. Its custom actions support scripts and
commands, but their static filters cannot hide actions based on active mount
state, and they cannot supply live badges. Nautilus extensions do not load in
Strata. No global custom actions are installed as a workaround.

Cirrove remains the authority for availability. A separate Python helper speaks
the daemon's local control protocol; Strata receives bounded JSON answers and
never imports Cirrove Python code or a cloud SDK into its UI process.

## Behavior

- **Keep offline**, or **Keep folder(s) offline**, applies to a fully eligible
  selection within one active account. Folders send `recursive: true`.
- **Stop keeping offline** appears only if every selected item has a direct pin.
  Inherited pins belong to the ancestor folder, so they offer Keep instead.
- **Show availability** reports each selected item's on-demand/kept/fetching
  state, including inherited pins. This is also available on a folder background.
- Cirrove's own kept/fetching artwork decorates pinned entries. Ordinary on-demand
  files have no badge. Folder badges follow the existing Nautilus contract; a
  folder pin is not proof that every descendant has finished downloading.
- Mixed selections containing unrelated/unresolved paths or multiple accounts
  have no actions. Nested mounts use the longest matching mount boundary.
- Menus and visible badges refresh on account/pin/mount events. Disconnects and
  unknown state withdraw presentation. Activation rechecks eligibility; no
  uncertain operation is automatically retried.

`paths-cached` is an additive daemon capability/verb, with the same request shape
as `paths`: `{"label":"account label","paths":["mount/relative/path",""]}`.
The empty string names the mount root. It returns the usual per-path state and
an additive `can_pin` boolean. It resolves only indexed metadata, including the
writable mount's namespace overlay, and never calls the provider. Native package
containers do not advertise a pin; ordinary files require a content revision.
Unknown metadata is refused rather than fetched. A writable directory status walk
is bounded to 10,000 children; larger uncached projection decisions remain absent.
The existing `paths` command/API continues to behave as before.

The helper explicitly requires `paths-cached: 1` from `capabilities`. Against an
older daemon it draws nothing, rather than falling back to a status lookup that
might hydrate a Google export. Normal file-manager thumbnails/previews are
separate and may still read content according to Strata's settings.

The daemon must also advertise `pin-identity`. Each menu leaf carries a bounded
opaque context for the exact selection and its account/collection/item identities
and mount incarnation. Activation rechecks that context; pin/unpin then send the
daemon's `expected` identity with the literal path. The service compares it with
the resolved target before mutation and operates on that resolved identity.
Replacing an object, account or mount invalidates old contexts. A bounded cache
retains at most 32 menu contexts; evicted contexts require reopening the
menu. Legacy control clients can omit `expected`, but Strata never does.

The helper accepts at most 200 paths and splits metadata queries to fit the
daemon's 8 KiB request limit, counting JSON escaping. Menu eligibility still
covers the whole selection. A metadata query has a four-second dispatch budget;
an individual path too large for the daemon is refused without fallback.
The helper uses one-second bounded socket exchanges,
a two-second mount cache, generation checks across responses, and the daemon's
`subscribe` stream with reconnect backoff. It performs no filesystem stat, content
read, symlink resolution or shell interpolation on selected paths. Non-UTF-8
names are unsupported by this JSON API and are left undecorated. Selections above
200 are deliberately not offered actions. A large/slow explicit pin batch can be
partially accepted; the result says how many. Its remaining items are not retried.
Activation outcomes distinguish confirmed acceptance, rejection and partial
acceptance from a lost reply. A lost reply retains the confirmed acceptance lower
bound and never causes automatic replay. Explicit service refusals show their
reason; failed connections report the unsent remainder without obscuring any
confirmed acceptance. A single submitted job includes its reference. Messages
are bounded by UTF-8 bytes to fit the host protocol.

## Install and remove

First build a daemon containing `paths-cached` and the companion provider-enabled
Strata branch. A normal developer daemon deployment uses
`scripts/install-developer.sh`, subject to the repository's package/developer
installation exclusivity. Do not replace a running daemon during a measurement.

The integration itself is opt-in and can be installed separately:

```sh
scripts/install-developer.sh --strata-only
# Equivalent:
/usr/bin/python3 scripts/install-strata.py
```

Restart the provider-enabled Strata build after installing. This only installs
`~/.config/strata/providers/cirrove/` (or `$XDG_CONFIG_HOME`); it does not modify
Strata preferences, default file-manager associations, the Cirrove daemon, accounts,
credentials or mount state. The helper and PNG artwork are copied there, so runtime
does not depend on the source checkout. `/usr/bin/python3` is the only helper runtime.
The raster artwork is rendered from Cirrove's existing shipped SVGs at 48×48.

```sh
/usr/bin/python3 scripts/install-strata.py --remove
```

Removal verifies hashes and removes only the installer-owned files. User edits,
unknown files and symlink destinations are preserved with an error. It never
recursively deletes a directory. Registration updates/removal take effect on the
next Strata start; daemon state changes are live.

For isolated testing, pass `--config-home /absolute/private/config` and
`--socket /absolute/private/control.sock` to the installer. Do not point an
experimental Strata build at your normal configuration just to test it.

## Validation

```sh
/usr/bin/python3 scripts/test-strata-provider.py
CARGO_TARGET_DIR=target/strata cargo test -p cirrove-service --lib cached_status --locked
CARGO_TARGET_DIR=target/strata scripts/check.sh
```

The protocol fixture covers direct/inherited/mixed pins, capability refusal,
mount boundaries/disappearance, literal Unicode/quotes/newlines, stale-generation
responses, absence and install/uninstall preservation. The Rust fixture uses a
provider that panics on every metadata/content call to prove cached status does
not touch the cloud, including unknown export children.

`scripts/validate-strata.py` drives the real companion Strata binary with the real
Cirrove helper and an isolated synthetic daemon. It uses Strata's private Xvfb,
D-Bus, HOME and AT-SPI harness, captures PNGs and writes a result artifact. It never
uses the user's graphical session. Run it with Strata's E2E Python environment,
Xvfb on PATH, and a fresh output directory:

```sh
/path/to/strata/target/e2e-venv/bin/python scripts/validate-strata.py \
  --strata-checkout /path/to/strata --output /absolute/new/artifact-directory
```

This proves the integration wiring on synthetic data, not real-provider
reliability. Native Wayland, an installed packaged build, multi-window stress and
long-session resource behavior remain separate acceptance work. This preview is
not published upstream and is not enabled in the normal installed applications.
