# 0008: Deletion, the provider's recycle bin, and the local one that must not exist

Status: partially implemented. The mount root now refuses to become a local
wastebasket (`is_trash_directory`, `filesystem.rs`), held by a test shown to fail
without it. Everything else here is a decision, not yet code: the honest delete
prompt, the second context-menu entry, and the per-provider capability that
decides whether either may be offered.

## What prompted it

A question -- *is there a provider-dependent check for safe delete versus
permanent delete, and what does the default do?* -- and then, while answering it,
a directory: `.Trash-1000/` with `files/` and `info/`, sitting in a real OneDrive,
created by GNOME Files on a writable Cirrove mount. Empty, so nothing was lost.
It would not have stayed empty.

That is the shape of the problem. The interesting failure was not in the delete
path at all. It was that the delete path was never asked.

## What is actually true today

**Graph's `DELETE` is already the safe variant.** `mutation.rs` issues
`DELETE /drives/{drive}/items/{id}` with an `If-Match` on the original eTag, and
Microsoft is explicit: *"Deleting items using this method moves the items to the
recycle bin instead of permanently deleting the item."* `architecture.md` has
said so since writable mounts existed. There is no permanent-delete path in the
tree; `permanentDelete` appears in no line of code.

**The opposite operation exists.** `POST /drives/{drive-id}/items/{item-id}/permanentDelete`,
Graph v1.0, same `Files.ReadWrite` permission as `DELETE`, personal and business.
So offering both variants is a product decision, not an API limitation.

**There is no capability interface.** ADR 0001 said not to build one early:
*"Do not pretend all providers support identical writes, shared links, exports,
trash, locks or permissions. Add capability interfaces alongside implemented use
cases rather than empty generic methods."* That was right, and the use case has
now arrived, which is when the interface is supposed to appear.

**A `mkdir` the user never asked for.** `mkdir` was gated on write access and
nothing else. The freedesktop trash specification tells a file manager to look
for `$topdir/.Trash` and, failing that, to create `$topdir/.Trash-$uid` -- and on
a mount, `$topdir` is the mount point. So the first Delete in a file manager
created a wastebasket inside the user's cloud drive.

## The decision

### 1. The mount root holds no trash

`mkdir` refuses `.Trash` and `.Trash-$uid` at the root with `EOPNOTSUPP`. Only at
the root: a `.Trash-1000` the user keeps somewhere inside their drive is their
folder, and no trash implementation looks there.

The cost is that GIO reads `EOPNOTSUPP` as "no trash here" and offers *permanent*
deletion instead -- a prompt warning that the file cannot be recovered, in front
of a delete that goes to the OneDrive recycle bin. Safe, and still not true. It
is a better lie than the one it replaces: a wastebasket in the user's drive that
makes the provider's recycle bin look empty is a wrong answer to *"where is my
file"*, and this one is only a wrong answer to *"can I get it back"*.

Fixing the prompt properly means implementing the trash specification against the
provider's recycle bin -- intercepting the rename into `.Trash-*/files/`,
synthesising `.trashinfo`, and listing the recycle bin as the trash's contents.
That is the right destination and it is not this change.

### 2. The default delete stays the safe one, and says so

Ordinary `unlink` and `rmdir` keep mapping to Graph `DELETE`: the recycle bin,
with the eTag precondition. No configuration makes the default destructive. A
user who deletes a file through any file manager, on any desktop, gets the
recoverable outcome without having installed anything.

### 3. Permanent deletion is an explicit second gesture, never a default

POSIX has one `unlink`, so the filesystem cannot carry the distinction: there is
no flag on the call in which "and skip the recycle bin" could live. Permanent
deletion therefore needs a control-socket verb and a file-manager extension that
calls it -- the same route pinning took, and for the same reason.

Milestone 5's rule applies unchanged: *"no control or state may be reachable only
through a tray or only through one file manager."* So the window gets the action
too, or the extension does not ship it.

### 4. A provider capability decides whether either may be offered

`supports_soft_delete` and `supports_permanent_delete`, per provider, alongside
this use case rather than ahead of it. OneDrive answers yes to both. A provider
without a recycle bin must not have its ordinary `DELETE` presented as
recoverable, and a provider without `permanentDelete` must not show a menu entry
that quietly falls back to the ordinary one. Both of those are the failure this
ADR exists to prevent, one level up.

## What is not closed

- **The rename edge.** The guard stops `.Trash-*` being *created* at the root. If
  one already exists in a user's drive -- put there by an earlier Cirrove, or by
  another tool -- GIO will find it and use it. Detecting and adopting existing
  ones belongs with the trash-specification work above, not with a `mkdir` guard,
  and pretending otherwise would be the same mistake in a smaller font.
- **The prompt still misstates the outcome**, as described in 1.
- **Folder deletion keeps its measured hazard.** Graph's `DELETE` on a folder is
  recursive and a folder's eTag does not move when a child is added -- measured,
  `folder_etag_and_mtime_ignore_their_children` -- so `rmdir` narrows the window
  between an emptiness check and the delete to one round trip and cannot close
  it. The recycle bin is the recovery path for exactly that case, which is one
  more reason the default must never bypass it.

## Consequences

A cloud mount is not a POSIX filesystem that happens to be remote, and deletion
is where that stops being a slogan. Two wastebaskets is the specific way it goes
wrong, and the specification that produces it is doing what it was designed to
do. Guarding one `mkdir` is cheap; the reason it was needed is that a writable
mount inherits every assumption the desktop makes about local filesystems, and
those assumptions are worth enumerating rather than meeting one at a time in a
user's live drive.
