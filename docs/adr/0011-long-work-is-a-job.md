# 0011: Long work a person asked for is a job, not a request

Status: decision, implemented 2026-09-15.

## The control socket answers questions; it cannot do work

Every control verb is one exchange: a line in, one JSON reply, the connection
closes. The client half wraps the whole exchange in a three-second deadline
(`request` in `crates/cirrove-service/src/lib.rs`), which is right for every
question the daemon is asked — status, capabilities, recent activity, path
states. All of them read local state.

Keeping something offline is not a question. It fetches every file a pin
covers, which is minutes on anything real. It was written as a request anyway,
and the result was a command that lied:

```
$ cirrove pin --path "Dokumente/Projekte/726-Kärnten Werbung" --recursive
Error: Cirrove pin timed out

Caused by:
    deadline has elapsed
```

Measured on a real drive on 2026-09-15: 3.00 seconds to that message, and the
daemon then kept all 69 files — 399 MB across 152 blocks — perfectly. The person
who asked was told their request failed; it had succeeded, and nothing anywhere
said so. The window's **Keep offline** button used the same call, held its one
operation slot for the whole fetch, showed a spinner, and offered no way to stop.

Three separate faults, one cause: work that outlives an exchange was put inside
one.

## The decision

Work a person asks for that can outlive a request is registered as a **job**.
The request does the part that is local and instant — resolve the item, walk the
subtree, reserve the space, refuse what has to be refused — and answers with the
job's id. The fetching runs on the account's task tracker.

- **Progress is a level**, reported in `status` beside the pins, which is what
  the window already polls twice a second. Filled when the status is answered,
  not by the five-second loop that rebuilds the rest: a bar that moved five
  seconds ago is a spinner with extra steps.
- **Stopping is a verb**, `stop-job`, taking the id. It cancels a token that is
  a child of the account's, so stopping a job leaves the account alone and
  stopping the account stops the job.
- **A job that finished leaves no record.** What it did is on the kept-offline
  list, which is a better receipt than a spent progress bar. One that was
  stopped or failed is retained — four of them, ten minutes — because a progress
  bar that simply vanishes tells nobody anything.
- **`cirrove pin` still waits.** The command means "it is kept offline now", so
  it follows the job and prints where the fetch has got to, on standard error,
  leaving standard output the one sentence it always was. Ctrl-C leaves the
  daemon keeping the folder, which is what someone who walked away wanted.

## What stopping means

Stopping a fetch **releases the pin it was filling**. Someone who stopped it did
not ask to keep part of a folder, and a pin that reserves the whole of one while
holding a third of it misreports both what is kept and what the cache has left.
The window's button says so before it is pressed.

A fetch that *fails* is the opposite case: the pin stays, and the blocks it did
fetch are protected. Three hundred files of three hundred and forty are three
hundred files a person can open on a train, and the failure is recorded with how
far it got, so the difference between "stopped" and "gave up" is visible.

## What is deliberately not a job

Ordinary reading. A mount stages cache on every read, so a register that
included those would be a register of everything — the same reason
[the recent-activity list](../../crates/cirrove-service/src/recent.rs) refuses
files a user merely opened — and the application doing the reading draws its own
progress. Uploads are not jobs either: they are already durable records in the
upload journal, which has always known how many bytes the cloud has taken, and
they are shown from there. This register holds work a person asked for by name.

## Cost

One module (`crates/cirrove-service/src/jobs.rs`), one field on
`AccountStatus`, one control verb, and a `job` field on `PinReply` that an older
client ignores and an older daemon never sends. No protocol version moves:
`stop-job` is announced through `capabilities`, the way `recent` and `paths`
were, so a daemon and a desktop of different ages keep working.

## Consequences

- Anything else that grows a long form — a first index of a very large drive,
  a bulk retry of refused saves, a future "download everything" — has a place to
  report from and a way to be stopped, without another one-off.
- A client that cannot render a job kind it has never heard of keeps rendering
  the rest: `JobKind` and `JobState` both carry an `Unknown` arm.
- The three-second client deadline stays, and stays correct, because nothing
  behind it does work any more.
