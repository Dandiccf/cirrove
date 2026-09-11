# 0007: A push channel from the daemon to local desktop clients

Status: implemented, tested, and consumed by a running tray. `capabilities`, the
per-exchange timeout, split connection budgets, `subscribe` carrying `account`,
`mount` and `ready` events, a client `Subscription`, and `cirrove-tray` as a
StatusNotifierItem that renders the stream. Verified against the real session
bus: the item registers with the shell's watcher and its properties change with
the daemon.

The timeout split is held by `a_subscription_outlives_the_exchange_timeout`,
which was shown to fail without it -- the subscription is cut at 3.75 s with "the
stream closed instead of delivering" -- as this project requires of a test
offered as proof of a fix.

Not implemented: `pin` and `transfer` events, and the `paths` batch query
Nautilus needs. All three reach into the engine, and the argument below is that
the protocol groundwork should land first.

## Problem

Three separate milestone 5 boxes -- tray status, Nautilus badges and actionable
progress -- are all blocked on the same missing thing, and none of them is a
user-interface problem. Each needs to know *when something changed*, and the
daemon cannot tell anyone.

The control socket is strictly one request, one response, connection closed
(`crates/cirrove-service/src/lib.rs:350-382`):

- a client writes `verb body\n`, bounded at 8 KiB by `read_request_line`
- the daemon writes one JSON object and drops the stream
- three verbs exist: `status`, `pin`, `unpin`
- the whole handler is wrapped in `tokio::time::timeout(Duration::from_secs(3))`
- at most 16 requests are in flight; a seventeenth connection is dropped without
  a reply

So the only way to notice a change is to reconnect and ask again. The desktop
window does not even do that -- it loads on demand and on `Retry`
(`crates/cirrove-desktop/src/main.rs`), which is honest for a settings window and
useless for a status indicator.

What that costs each of the three, concretely:

- **Tray.** A mount going away, an account needing reauthentication and a
  transfer finishing are exactly the events a tray exists to show. Polling once a
  second is 86,400 connections a day to learn that nothing happened, and it still
  shows a stale icon for up to a second.
- **Notifications.** A notification is an edge, not a level. Polling reconstructs
  edges by diffing two samples, which means every missed sample is a missed
  notification and every restart of the client is a burst of false ones.
- **Nautilus.** This one is worse in kind, not degree. A file manager asks about
  *the files currently on screen* -- tens to hundreds of paths, changing as the
  user scrolls. Per-file polling over a connection-per-request socket is not a
  tuning problem, it is the wrong shape.

## Constraint that shapes the whole design

`STATUS_PROTOCOL_VERSION` cannot move. The desktop compares it for **equality**
(`crates/cirrove-desktop/src/model.rs:129`) and reports `IncompatibleService`
otherwise, so a bump makes every mismatched pair refuse each other. The comment at
`lib.rs:358-362` already records this: the verb dispatch was widened for `pin`
without moving the version precisely because the version must not move for a
change that alters no payload an existing client reads.

A push channel is exactly such a change for existing clients -- they never ask
for it -- so it must be discoverable without a version bump. This rules out the
obvious design, which is to bump the version and let clients branch on it.

## Decision

### 1. Capability discovery by asking, not by version

Add a verb `capabilities`, answered with a JSON object listing named features and
their individual versions:

```json
{"capabilities": {"events": 1, "paths": 1}}
```

An older daemon answers an unknown verb through `handle_control`'s fallback,
which today returns a `PinReply` with `refusal: Some("unknown control request
...")`. So the rule for a client is **"the reply has no `capabilities` key,
therefore this daemon has no events"** -- not a match on the refusal text, which
is a human-readable sentence and will be reworded. The same rule survives the
other refusal an old daemon can give here, `"this daemon manages no accounts"`,
which also carries no such key.

That gives discovery with no new failure mode and no version bump. `status` keeps
its current payload byte-for-byte.

This is the part to get right first, because it is the part that cannot be
changed later: every future capability rides on it.

### 2. Subscriptions are a different connection class, not a long `status`

`subscribe <json>\n` switches the connection to a stream of newline-delimited
JSON events and does not close it. This forces two changes to the accept loop,
and both are the actual work:

- **The 3-second timeout must become per-verb.** Today one `timeout` wraps the
  whole handler. A subscription lives for hours. The timeout has to apply to
  reading the request line and to writing a single event, never to the
  connection's lifetime.
- **The 16-slot budget must be split.** Long-lived subscribers would otherwise
  starve `status`: a tray, a file manager and a window are three permanent slots
  out of sixteen, and a leaked subscription holds one forever. Separate budgets --
  a proposal is 16 request slots unchanged plus 8 subscription slots -- keep a
  misbehaving client from denying service to the CLI. A subscriber over budget is
  refused *with a reply saying so*, not dropped silently as today.

### 3. Events are coalesced levels, not a reliable log

The daemon must never block on a slow reader, and a desktop client does not need
history. Each subscription gets a bounded queue holding **the latest value per
subject**, not a backlog. A client that stalls and resumes sees current state,
not a replay.

This is the same shape ADR 0003 already uses against the provider feed: a
generation counter coalesces notifications without losing the last wake. The
event channel should reuse that reasoning rather than invent a second one.

When coalescing drops intermediate states, the event carries a flag saying so, so
a client that is drawing progress knows it saw a jump rather than a smooth
sequence.

### 4. A first event set, deliberately small

Only what the three consumers need to stop polling:

| event | carries | consumer |
|---|---|---|
| `account` | connection state, mounted, needs-reauthentication | tray, window |
| `mount` | mount path appeared or went away | tray, Nautilus |
| `pin` | a path's pin state changed | Nautilus |
| `transfer` | in-flight count, bytes done and total, last failure category | tray, notifications |
| `ready` | priming is complete | every client |

Every subscription opens with one synthetic event per subject carrying current
state, so a client never needs a `status` call to prime itself and there is no
window between "subscribed" and "knows the state".

**Priming ends with a `ready` marker, always, including when it carried
nothing.** This was not in the design and was added after running the tray
against a daemon with no accounts: with nothing to prime with, the daemon sent
no bytes, and the client -- which read one line to tell a stream from a refusal
-- waited out its deadline and reported "the service did not answer in time"
against a service that was answering perfectly. Silence and an established
subscription are indistinguishable on the wire unless something is said. It also
earns its keep for a client that would rather render once than once per
account.

**`transfer` must be published, not polled.** This was checked rather than
assumed. The upload state exists and is reachable -- `Manager` holds
`engines: RwLock<HashMap<String, Arc<Engine>>>`, which is already how `pin` and
`unpin` reach an engine -- but it is **not** in `AccountStatus`, so it needs a new
accessor either way. The trap is the obvious implementation: `UploadJournal::list`
is a blocking SQLite call, and deriving each event by querying the journal would
put SQLite on the event path, once per subscriber per change. The status path
already avoids this by wrapping its one store read in `spawn_blocking`
(`lib.rs:374`), and AGENTS.md requires it. So the code that *changes* upload state
publishes the new level, and the event channel forwards what it was handed. The
journal stays the durable record and is read on subscribe, once, to prime.

Payloads follow the existing discipline: no tokens, no cursor or signed URLs, no
provider bodies, and failure *categories* rather than messages, as the desktop
failure model already does.

### 5. Nautilus gets a batch query, not the event stream alone

Badges need a level for a set of paths, and an event stream cannot answer "what
is the state of these 200 paths I just scrolled into view". So a second verb,
`paths <json>`, takes a bounded list and answers one record each. The event
stream then keeps those answers fresh, and the file manager re-queries only on
scroll.

The bound matters: an unbounded path list is a way for a file manager to make the
daemon walk the store synchronously while it holds a request slot. A proposal is
256 paths per request, refused above that rather than truncated, because silent
truncation would show wrong badges rather than none.

Path handling is the one place this design touches privacy: paths are the user's
file names. They stay on a local Unix socket, are never logged, and appear in no
artifact. This is worth stating because every other payload in this protocol is
already free of user data and this one is not.

## What this does not settle

- **Whether the tray needs its own process.** StatusNotifierItem over D-Bus is
  what milestone 5 asks for, and whether that lives in `cirrove-desktop` or a
  separate binary is a packaging question this does not answer.
- **Whether Nautilus integration is a python extension at all.** The batch query
  is what the extension would call; choosing the extension mechanism, and what
  happens on file managers that are not Nautilus, is separate work that milestone
  5 lists separately.
- **Where the tray's own process lives.** See above; packaging, not protocol.
- **Cost.** Every number above -- 8 subscription slots, 256 paths, the queue
  depth -- is a proposal, not a measurement. The measurement discipline in
  AGENTS.md applies: if any of them is defended later as a performance property,
  it needs a registered question and a run, not a round number in an ADR.

## Why this ordering

The capability verb and the timeout split are prerequisites for all three
features and are invisible to users. They are also the two things that are
painful to change once a client ships against them. Everything user-visible --
the tray icon, the badge, the notification -- is comparatively cheap once a
client can hold a connection and be told things.

The alternative ordering, building the tray first against polling and migrating
it later, produces a second status path that has to keep working, which is how a
protocol ends up with two of everything.
