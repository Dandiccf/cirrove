# Cirrove contributor instructions

- Read README.md and docs/architecture.md before changing behavior.
- This is an independent new project; do not modify or migrate another cloud client's
  configuration, credentials, mounted files or service state as part of development.
- Keep implemented capabilities separate from roadmap promises. This is a read-only
  preview under validation. Do not claim real-provider reliability or enable writes
  based solely on synthetic tests.
- Never log tokens, cursor URLs, signed URLs or raw provider bodies.
- Provider identity is account + collection + item, not a path string.
- Keep network awaits outside SQLite transactions and shared filesystem locks.
- Preserve visible metadata and completed cursors together across interrupted refreshes.
- Use synthetic fixtures by default. Cloud mutations require explicit task authorization.
- Run formatting, clippy, workspace tests and the smoke test for relevant changes.
- Use a separate Cargo target directory for each worktree. Sharing incremental
  artifacts across checkouts can reuse a test executable from another source tree;
  verify that filtered test commands actually execute the expected tests.
- Do not add dependencies or abstractions for unimplemented features without a concrete use case.

## Destructive operations on a developer machine

This project is developed on machines that also run it. At the time of writing this
host carried a live `cirroved` user service and a real-account FUSE mount at
`~/Cloud/Cirrove-OneDrive`, alongside 2.8 GB of gitignored measurement evidence that
every recorded benchmark depends on and that exists in no other copy.

`rm -rf` recurses into a mounted filesystem. A cleanup that walks a live Cirrove mount
issues `unlink` and `rmdir` through the filesystem layer, and on a writable mount those
reach the provider. The failure is not a lost temporary directory; it is the user's
OneDrive.

Two incidents have already happened here, both survivable, both the same shape — delete
first, discover what was using it second. One removed the temporary directory of a
running 28-minute measurement, which died with exit 101. The other was
`gh pr merge --delete-branch`, which in gh 2.100.0 removes a linked worktree, directory
and all, not merely the ref.

Before any recursive delete or move, in this order:

1. `findmnt -R -T <path>` returns nothing under the path. Mounts are checked first
   because that failure is the unbounded one.
2. The path neither lies under nor contains `~/Cloud/Cirrove-OneDrive`,
   `~/.local/state/cirrove`, `~/.local/bin/cirroved` or this repository's
   `.local-state/`, after resolving symlinks.
3. No measurement is running against it: `pgrep -f cirrove-service` is empty, and no
   run manifest lists it as live.
4. `fuser -m <path>` reports no user.

Then move it to `.local-state/quarantine/<date>/` rather than deleting it. A human
empties that directory.

Kill a measurement by its recorded process id, never by pattern: `pkill -f cirrove`
matches the running daemon.

Long or exclusive runs record a manifest before they start — command, binary sha256,
private `TMPDIR`, expected duration — so that anyone can tell what a directory is for
before touching it.

## Measurement discipline

A gate closes on a controlled measurement, not on a passing test.

- `TMPDIR` and `SQLITE_TMPDIR` must never be tmpfs. `/tmp` is tmpfs on a typical
  desktop, and a RAM-backed temporary directory moves bytes out of process RSS without
  reducing host memory, which flatters every memory result. Assert the filesystem with
  `findmnt` and record it in the artifact.
- One measurement at a time, with nothing else compiling. `cargo build` perturbs the
  quantity being measured as much as a second fixture does.
- Register the question, the arms, the endpoint and a written prediction before the run,
  in `docs/benchmarks/`, so a result cannot be re-narrated afterwards. Evidence that
  lives only in `.local-state/` supports no claim.
- A test offered as proof of a fix must first be shown to fail without it. This project
  has twice nearly shipped a test that passed either way.
- One run per arm supports a direction, not a result. Quote the measured within-arm
  spread beside any difference; retained RSS varies by about 6 percent between identical
  runs here, while live bytes per view reproduce to 0.2 percent.
- Corrections belong in the same artifact as the claim, not only in a working note.
