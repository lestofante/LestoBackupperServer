# Event Log Library

A generic event-sourcing layer over SQL. Domain-agnostic: it knows nothing about
files, albums or users. See `core.md` for shared conventions.

Scope: minimalistic implementation with minimal depedency but without sacrifing type safety.

## Command vs. apply

**Command** — runs once. May be non-deterministic: resolves names, validates
permissions, reads the clock, generates IDs.

**`apply()`** — pure. No clock, no RNG, no lookups, no validation, no network.
Receives only `&Transaction` and `&Event`.

```rust
pub fn add_to_album(conn, actor: UserId, album_name: &str, hash: &ObjectId) -> Result<()> {
    let album_id = resolve_album(conn, actor, album_name)?;
    check_can_write(conn, actor, album_id)?;
    let ev = Event {
        seq:    next_seq(conn)?,
        actor,
        at:     Utc::now(),
        action: Action::AddToAlbum { album_id, hash: hash.clone() },
    };
    conn.transaction(|c| {
        append(c, &ev)?;
        apply(c, &ev)          // both commit, or neither
    })
}
```

**An event must be self-sufficient.** If `apply()` would need a lookup, that lookup
belongs in the command and its result in the event.

## No DB-generated values

No autoincrement or `SERIAL` for anything an event references. Generate **ULIDs** in
Rust (timestamp-prefixed, so they still sort chronologically) and timestamps from the
command clock, both written explicitly into the event. Otherwise `apply()` needs two
modes and replay on an empty DB assigns different IDs.

`seq` comes from a counter row incremented **inside** the transaction — autoincrement
does not roll back.

## Actions

One typed enum, serde JSON, one `match` in `apply()`. Exhaustiveness makes a new
unhandled variant a compile error. Arms delegate to small functions.

```rust
match &ev.action {
    Action::UploadFile { hash, name, mime }     => file::insert(tx, ev.actor, hash, name, mime),
    Action::AddToAlbum { album_id, hash }       => album::add_file(tx, album_id, hash),
    Action::ShareAlbum { album_id, user, role } => album::add_member(tx, album_id, user, role),
}
```

**Old actions stay replayable forever.** Never change a variant's semantics — add `V2`
and keep the old arm.

## Schema

```
event(
  seq          INTEGER PRIMARY KEY,   -- counter row, inside txn
  node_id      TEXT NOT NULL,
  actor        TEXT NOT NULL,         -- indexed
  at           TEXT NOT NULL,         -- indexed
  action_type  TEXT NOT NULL,
  subject_a    TEXT,                  -- nullable, indexed
  subject_b    TEXT,                  -- nullable, indexed
  version      INTEGER NOT NULL,
  payload      TEXT NOT NULL          -- serde JSON of Action
)
```

The hoisted `subject_*` columns are a denormalized index over the payload, not a second
source of truth. `apply()` reads only `payload`; on disagreement the payload wins. The
domain supplies them via `fn index_fields(&Action) -> IndexFields`, so the library owns
the table and the caller owns what is queryable.

`(node_id, seq)` is also the replication cursor. Timestamps are unusable for ordering
across machines with drifting clocks.

## API

```rust
pub trait Projection {
    type Action: Serialize + DeserializeOwned;
    fn apply(tx: &Transaction, ev: &Event<Self::Action>) -> Result<()>;
    fn index_fields(a: &Self::Action) -> IndexFields;
}
```

The library provides: the `event` table and its migrations, sequence allocation,
append-and-apply in one transaction, `replay()` and `replay_from(seq)`, and the
determinism harness. There is no API that writes an event without applying it, or
applies without writing.

## Enforcing determinism

- `apply()` lives in a module that does not import `chrono` or `rand`.
- `clippy.toml`: disallow `SystemTime::now`, `Utc::now`, `rand::random`.
- **Never iterate a `HashMap`** in `apply()` — order is randomized per process. Use
  `BTreeMap` or sort explicitly. Same for `read_dir` order and locale-dependent sorts.

## The proof

```rust
let (state_live, log) = run_commands(&cmds);
let state_replayed    = replay(&log);
assert_eq!(state_live, state_replayed);
```

Drive with `proptest` over random valid command sequences; shrinking gives minimal
repros. Note: "call the same function twice" does not work — the second call runs
against state the first mutated.

## Do not use an existing framework

`cqrs-es`, `eventually-rs`, `disintegrate`, `event_sourcing.rs` are maintained but built
around DDD aggregates with concurrent writers, target Postgres/MySQL/DynamoDB, are
async-first, and mostly treat read-model updates as *eventually* consistent — updated by
handlers after the fact, not in the same transaction. That last point is the guarantee
this design rests on.

## Dependencies

`rusqlite`, `serde`, `ulid`, `proptest`.
