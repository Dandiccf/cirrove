# Contributing to Cirrove

Cirrove is early-stage infrastructure. Start with a focused issue or draft change
that states the behavior, its failure cases and the validation evidence.

Read [Architecture](docs/architecture.md) and [Roadmap](docs/roadmap.md). Run the
checks in [Development](docs/development.md). Keep provider details out of the
filesystem layer and long network requests out of database transactions.

Tests should cover behavior and failure boundaries: dropped requests, interrupted
pagination, stale cursors, authentication expiry, throttling and concurrent reads.
Use fake providers and sanitized fixtures by default. A passing mock test is not a
claim that a real account has been validated.

Never include access/refresh tokens, signed download URLs, account identifiers,
real filenames, tenant configuration or user documents in fixtures or issue logs.
Use scoped synthetic examples. Do not run mutation tests against production drives.

Changes are contributed under Apache-2.0. Preserve attribution when importing code;
do not copy code from another project without checking license compatibility.
Keep changes small enough to review and update the implemented/planned distinction
in the README when capabilities change.
