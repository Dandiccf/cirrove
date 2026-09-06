# ADR 0001: Provider-native service, staged metadata, on-demand content

Status: accepted for the initial foundation.

## Context

A full mirror is unsuitable for large cloud accounts. A generic command wrapper
alone does not model linked libraries, stable identity, token refresh, provider
cooldowns or content versioning well enough for an interactive filesystem.

## Decision

Use Rust with Tokio for the service, native provider adapters behind a small typed
contract, SQLite for durable metadata and a future disk content cache. Begin with
Microsoft Graph. Keep Linux filesystem integration outside provider code. Version
and pin the toolchain, commit Cargo.lock, and test recovery invariants before UI work.
Use a private Unix socket for the first status API; evaluate D-Bus for desktop
integration after the service's account and operation model stabilizes.

The initial interface covers metadata only. Do not pretend all providers support
identical writes, shared links, exports, trash, locks or permissions. Add capability
interfaces alongside implemented use cases rather than empty generic methods.

## Consequences

We own provider integration and the difficult consistency/recovery behavior.
Direct Graph access does not remove throttling or guarantee latency. iCloud needs
an isolated compatibility strategy. The current milestone is intentionally too
small to mount or write user cloud data. No existing integration is replaced.
