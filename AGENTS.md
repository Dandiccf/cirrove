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
