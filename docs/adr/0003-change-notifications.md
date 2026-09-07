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
failures are visible. Healthy sessions renew after 50 minutes; an isolated
business-drive run has passed actual renewal and a subsequent notification.
Graph's account-wide cooldown still applies.

Refresh starts are spaced by at least 250 ms to coalesce bursts without indefinitely
postponing continuous activity. A hint received during a failed request cannot
override its retry deadline. Linked-library discovery uses one coalescing worker,
not an unbounded task queue. Unsupported adapters retain their existing polling.
Per-feed status separates notification connectivity from successful reconciliation.

Periodic checks remain at the configured interval, currently 30 seconds by default,
even when notifications work. Removing this safeguard or lengthening it before
measuring provider delivery would risk making freshness worse. Reconnect catch-up
does not depend on receiving another remote notification.

## Recently used directory revalidation

Late provider hints need a bounded complement. Filesystem directory reads now
register a 60-second lease, with at most 32 entries per account and one worker.
Successful listings are eligible again after five seconds; starts are spaced by
at least two seconds across the account. Large or multiple directories therefore
take longer. Pagination, provider cooldown and the existing listing deadline all
still apply. This is targeted metadata polling, not a push-delivery guarantee.

Cached listings stay readable during refresh. The worker shares each directory's
foreground gate and skips a currently running cold request. Failures back off;
repeated use cannot bypass that backoff. It emits filesystem invalidations only
when listing metadata changes, and does not renew its own activity lease. An empty
or unrelated delta cannot overwrite a newer observed directory with older indexed
metadata. A relevant delta or replacement baseline still reconciles observations.

Activity is inferred from actual filesystem requests. Detecting a window that
remains visible without issuing further reads would need an explicit desktop
integration. No such integration or always-visible-folder guarantee is claimed.

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
  The notification validator's optional `--check-renewal` waits for the actual
  approximately 50-minute renewal and checks a new notification after reconnecting;
  one isolated business-drive run has now passed that real renewal, reconnection,
  and a fourth generated-fixture change through notification-triggered delta.
  The process finished successfully and retained its private event evidence.
  This does not establish personal-account behavior, outage recovery or 24-hour
  sustained operation.
- Revalidation fixtures cover bounded activity, backoff, cached navigation while
  another directory stalls, and actual mounted create/rename/delete visibility
  during a blocked content read. Push is disabled and the delta timer is one hour
  in the activity tests. Unchanged observations do not request another reload.
- `validate-onedrive-freshness` complements the deterministic checks with real
  Graph directory listings through an isolated kernel mount. Its fixture-only
  baseline and disabled push exclude other refresh mechanisms. A limited
  business-drive run passed new-folder and conditional Unicode rename visibility;
  timings and identifiers stay in private evidence. See
  [the repeat command and limits](../write-validation.md#check-directory-freshness-through-an-actual-mount).
- Measure multiple large directories and ordinary file-manager windows. A small
  mounted fixture does not establish desktop event handling, production request
  cost, personal-account behavior or a general Microsoft latency guarantee.

## Sources

- [Microsoft Graph WebSocket endpoint](https://learn.microsoft.com/en-us/graph/api/subscriptions-socketio?view=graph-rest-1.0)
- [Engine.IO protocol](https://socket.io/docs/v4/engine-io-protocol/)
- [Socket.IO protocol](https://socket.io/docs/v4/socket-io-protocol/)
- [Google Drive push notifications](https://developers.google.com/workspace/drive/api/guides/push)
