Core Concepts

Shared conventions for all components. Read this first.

Components:

eventlog.md — generic event-sourcing library over SQLite
server.md — sync server built on it
web.md — web interface
Principles
Content-addressed — identity is the hash of the bytes, not the path.
Nothing is overwritten. An edit is a new object. This removes conflict resolution entirely: there is no mutable slot for two versions to contend over.
Deletes are explicit, never implicit.
The event log is the source of truth. All other tables are rebuildable projections.
Attribution belongs to actions, never to objects (content is shared and deduped).
State must be inspectable by hand.
Hashing

SHA-256 — the most portable choice, hardware-accelerated on essentially all modern hardware, and available in every language and CLI toolchain.

Tagged hashes

Hashes are self-describing:

sha2-256:e3b0c44298fc1c14...

The event log is immutable, so an untagged digest becomes unidentifiable after any future algorithm migration and history cannot be rewritten to fix it. On migration, new ingests use the new algorithm and existing objects keep their identity — a file gains a second hash row. No historical event is ever rewritten.

Wrap in a Rust newtype so an untagged hash cannot be constructed.

Size

Store and verify on every ingest. Hash match with size mismatch is a hard error surfaced to a human — it indicates corruption or a local bug, and auto-recovering would mask it.

Identity

Internal integer row IDs are fine for joins inside one database. Externally — in events and on the wire — always the tagged hash.

A local ID is meaningless on another node. Two nodes ingesting the same bytes must independently arrive at the same identity, or dedup and mirror reconciliation break.

Language

Rust for the server and the Linux client. The Android client already exists and is out of scope here.
