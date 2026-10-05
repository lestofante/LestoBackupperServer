# Sync Server

The domain application, built on `01-eventlog.md`. See `00-core.md` for shared
conventions.

## Blob storage

Blobs live **on disk**, not in the DB: `sendfile()` for range requests, incremental
`rsync` backup, per-object `sha256sum` verification, no huge-DB rewrite on delete.
Access via a single storage trait so the backend is swappable.

Flat layout, all objects in one directory, named by tagged hash:

```
objects/sha2-256:e3b0c44298fc1c14...
```

(ext4 with `dir_index` and XFS handle very large flat directories fine. If a target
filesystem degrades, sharding is a change to the storage trait only.)

**Previews in the DB** as BLOBs — tens of KB, the size class where the DB beats the
filesystem, and gallery rendering stays one query.

### Write ordering (critical)

1. Write blob to temp, `fsync`
2. `rename()` into place (atomic on POSIX)
3. **Then** commit the DB transaction

Crash between 2 and 3 leaves an orphan blob — harmless. The reverse order leaves a row
pointing at a missing file, i.e. corruption. **Create the orphan-able thing first;
commit the reference last.**

## Schema

### Files

```
file(id PK, size, mime, width, height, duration, previous_version NULL)
file_hash(file_id, algo, digest, PRIMARY KEY (algo, digest))
```

`file` is pure content — **no creator, no owner column.** With dedup the same content is
one row; an owner column would misattribute a second uploader's file and leak that
another user holds those exact bytes.

`previous_version` is advisory provenance only, a DAG not a chain (two people can edit
one parent). Nothing correctness-critical may depend on it.

`file_hash` holds N rows per file — migration adds a row rather than replacing one.

```
user_file(user_id, file_id, added_at, source, PRIMARY KEY (user_id, file_id))
```

A user's library and the attribution record. **"All Photos" is this table**, not an
album row — the invariant holds structurally because having a row is what "this user has
this file" means. Album-shaped rendering is a UI concern.

### Albums

```
album(album_id PK, name, kind)             -- kind: user | system
album_member(album_id, user_id, role)      -- role: owner | admin | writer | reader
album_file(album_id, file_id, added_at, added_by)
```

**ACL, not owner+group.** Unix allows one group per object, so "Alice owns, Bob writes,
Carol reads" would need a group per permission combination. With an ACL a personal album
is just an album with one `owner` row; **sharing is inserting a row, unsharing is
deleting one.** No `is_shared` flag, no separate table, no branching.

No principal/group indirection yet — direct user rows are cheap at family scale and the
layer can be added later without changing this shape.

Name uniqueness is scoped per owner, so two users' "Vacation" coexist.

**Exactly one owner**, plus `admin` for membership management. Ownership transfer is an
explicit logged action — avoids the "owner vanished" deadlock without losing per-owner
name scoping.

### Auth and replication

```
credential(user_id PK, password_hash)
session(token PK, user_id, issued_at, expires_at, device_label)
object_replica(file_id, node_id, state, confirmed_at)
preview(file_id, kind, blob)
```

One `password_hash` column — Argon2id encodes salt, algorithm and cost inside the
string. A separate salt column signals a hand-rolled scheme.

Devices hold long-lived revocable tokens, never re-login. A lost phone must be killable.

`object_replica` confirmation count is the real "is this safe yet" signal.

### library vs. organization

`user_file` = your library (uploaded, or explicitly saved from a shared album).
`album_file` = organization, and may include content not in your library.

Joining a shared album must not dump someone else's thousands of photos into your All
Photos — but you can browse them and save individual ones.

### Blob lifetime

Removing a `user_file` row deletes only that row. A blob survives while any `user_file`
or `album_file` references it. GC over unreferenced files is a separate optional pass,
never inline, and low priority — it will rarely reclaim much.

## Naming

Server layout: `username/albumname/hash-originalname.ext` — always hash-prefixed, one
uniform rule, no conflict detection needed.

Clients preserve the original filename. On a genuine namesake collision within an album,
the newcomer is disambiguated by **actor + timestamp**
(`report (edited by bob, 14:32).txt`), which is readable. Hash suffix is last resort
only. For the symmetric case (both edited offline), the log's attribution resolves it;
any further tiebreak must be deterministic and symmetric so both sides converge without
coordination.

Album names may contain `/`, but **names are opaque and hierarchy is display-only** —
`a/b` does not imply `a` exists. Client folder depth is carried through, not flattened.

## Topology

**Explicitly configured, not peer-to-peer.** No discovery — "what talks to what" is
always answerable by reading a config.

- **Servers**: full mirrors of each other, configured full mesh.
- **Clients**: `role = mirror | partial` per device. `mirror` behaves like a server.
  `partial` (phones) is preview-only until an explicit pull — a distinct code path from
  the mirror reconcile loop.

Server-to-server reconciliation is "send me events after seq N".

**Auth**: the server rejects any action whose claimed actor differs from the
authenticated session. On shared machines, attribute by OS user subtree — the boundary
the OS already enforces is the attribution boundary. One OS account shared by multiple
humans is out of scope.

## Watching (Linux client)

- inotify is **not recursive** — walk at startup, watch per directory, add watches on
  `IN_CREATE` for new subdirs.
- Check `fs.inotify.max_user_watches`.
- **Watch `IN_MOVED_TO` as well as `IN_CLOSE_WRITE`.** vim, LibreOffice and most IDEs
  write to a temp file and rename over the target, which never gets close-write.
- **Self-feedback**: the daemon's own writes fire local events. Keep a short-lived
  "just wrote this path, ignore next event" set.

## Previews

Images: downscaled proxy. Video: **single extracted frame only** — no proxy clips, no
transcode, no streaming. Extraction shells out to `ffmpeg`.

**Generated exactly once, by the ingesting node**, then replicated as an ordinary
object. Per-mirror generation would produce differing bytes for the same logical
thumbnail (encoder nondeterminism) and break dedup.

## Database

**SQLite** via `rusqlite` — compiles in, one file, inspectable via the `sqlite3` CLI.
Pure-Rust KV stores (`redb`, `fjall`) would mean hand-rolling every join, index and
constraint. (`sled` is stalled — avoid.)

Postgres is viable if ad-hoc analytical queries over event payloads matter (`jsonb` +
GIN). Cost: a separate always-on daemon and `pg_dump`/WAL backups. If Postgres, force
`COLLATE "C"` anywhere ordering affects replay — glibc has changed collation between
versions and corrupted indexes across distro upgrades.

## Accepted limitations

- **In-place writers.** Dedup safety assumes write-new-then-rename. Software that writes
  into an existing file in place mutates the shared inode. Covers nearly all real
  software; doing it properly means FUSE.
- **Trusted attribution.** Sessions stop cross-user forgery, but a compromised device
  with a valid session acts freely as that user. No per-event signing.
