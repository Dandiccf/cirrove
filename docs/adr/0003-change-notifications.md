# 0003: Provider notifications wake incremental reconciliation

Status: accepted; OneDrive implementation under validation.

## Problem

Cirrove previously started its background delta feed only on a timer, normally
30 seconds after the previous successful round. Content was already on demand,
but a change on another device could remain invisible until a later round.
Polling is a form of metadata synchronization; replacing its trigger does not
eliminate reconciliation or make a cloud filesystem a local disk.

## Decision and implementation

Adapters may maintain a collection's change-notification session. They send a
provider-neutral hint and connected status, never mutate the metadata store from
an event payload. A generation counter coalesces notifications without losing the
last wake during active work. The service runs the existing cursor-based delta
operation, commits it atomically, then invalidates mounted views.

OneDrive uses the official Graph v1.0
`GET /drives/{driveId}/root/subscriptions/socketIo` endpoint. The returned URL is
a signed Socket.IO endpoint: an outbound WebSocket connection, with no public
callback server. Cirrove implements the documented Engine.IO v4 and Socket.IO v5
text-notification subset over tokio-tungstenite/Rustls. The URL path identifies the
namespace, while the transport uses `/socket.io/` and its signed query. It performs
the handshake and namespace acknowledgement, answers heartbeats and event ACKs,
and recognizes `notification` events. It does not implement general binary events,
multiple namespaces on one socket, long polling or a public Socket.IO client API.

The socket has bounded frames/messages, connection/send deadlines, cancellation and
a heartbeat deadline which unrelated messages cannot extend. A successful initial
connection or reconnection requests catch-up. Failed endpoint acquisition does not
permanently disable notifications: bounded backoff retries it. Authentication
failures are visible. Healthy sessions renew after 50 minutes; an actual long
renewal test remains required. Graph's account-wide cooldown still applies.

Refresh starts are spaced by at least 250 ms to coalesce bursts without indefinitely
postponing continuous activity. A hint received during a failed request cannot
override its retry deadline. Linked-library discovery uses one coalescing worker,
not an unbounded task queue. Unsupported adapters retain their existing polling.
Per-feed status separates notification connectivity from successful reconciliation.

Periodic checks remain at the configured interval, currently 30 seconds by default,
even when notifications work. Removing this safeguard or lengthening it before
measuring provider delivery would risk making freshness worse. Reconnect catch-up
does not depend on receiving another remote notification.

## Provider independence

The core knows only scope, change hints, connection state and incremental changes.
Socket.IO, signed subscription URLs and Graph endpoints remain in the OneDrive
crate. Future adapters can implement another notification mechanism or explicitly
fall back to polling; a shared core must not impose the least capable transport on
all providers.

Google Drive's documented watch channels deliver HTTPS webhooks and expire. Their
receiver/renewal architecture requires a separate decision; a desktop service cannot
simply reuse the OneDrive socket. iCloud Drive has a separate API feasibility gate.
Neither adapter is implemented or promised to provide equivalent latency.

## Validation and next acceptance gates

- Deterministic tests: actual loopback WebSockets, authenticated endpoint acquisition,
  bearer isolation, heartbeat loss, oversized messages, wrong namespaces, renewal,
  cancellation, busy delta workers, event bursts, throttling and reconnect catch-up.
- Isolated live command: `validate-onedrive-notifications` requires the existing
  disabled write-test connection, creates one uniquely named fixture and renames
  only that fixture. Each change must appear in a delta fetched because of a
  notification. No polling is used to satisfy this test; identifiers, event timing
  and evidence remain private, and the fixture is retained.
  Repeat using the disabled connection from [isolated write setup](../write-validation.md):
  `./target/debug/cirrove validate-onedrive-notifications --label upload-validation --state-dir "$PWD/.local-state/write-validation"`.
- Measure remote operation acknowledgement to notification receipt, receipt to
  delta start, and receipt to committed/visible metadata separately. The local
  scheduling target is p95 below 500 ms when no current refresh or cooldown blocks
  it; this is a target, not a measured end-to-end provider guarantee.
- Validate actual reconnect/renewal after long sessions, suspend/network loss,
  personal OneDrive, linked libraries and restricted permissions.
- Determine a bounded policy for refreshing actively viewed folders when upstream
  notifications arrive late. Do not replace a slow timer with a global aggressive
  polling loop or describe push alone as a latency solution.

## Sources

- [Microsoft Graph WebSocket endpoint](https://learn.microsoft.com/en-us/graph/api/subscriptions-socketio?view=graph-rest-1.0)
- [Engine.IO protocol](https://socket.io/docs/v4/engine-io-protocol/)
- [Socket.IO protocol](https://socket.io/docs/v4/socket-io-protocol/)
- [Google Drive push notifications](https://developers.google.com/workspace/drive/api/guides/push)
