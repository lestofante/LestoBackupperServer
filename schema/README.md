# lestobackupper-schema

> **Heads up:** this is a heavily "vibe-coded" experiment. The design and just
> about all of the code came out of an extended back-and-forth with an AI
> assistant, not a from-scratch human implementation, and it hasn't run in
> production anywhere. Treat it as exploratory rather than something to
> depend on as-is.

The wire contract for the whole system: `fbs/protocol.fbs`, a FlatBuffers
schema covering every request/response the client/server protocol supports
(mirrors `server::commands`/`server::queries` one-for-one — see
`../server-api`'s dispatch layer). This is the one file every client
language reads, so it deliberately avoids anything Rust-specific.

## Why FlatBuffers, why `planus`

FlatBuffers over Cap'n Proto: `flatc` (the official compiler) has
first-class, actively-maintained generators for Rust, Kotlin, and Swift —
covering this project's web (Rust/WASM), Android, and iOS targets from one
schema. Cap'n Proto's non-C++ language support, Swift especially, is
thinner.

`planus` over the official `flatc`: `flatc` is a C++ binary you'd need to
install or vendor separately. `planus` is a from-scratch, pure-Rust
reimplementation (its own schema parser, its own codegen) — so the whole
pipeline is just `cargo build`, no external tool to install. The Android
and iOS clients aren't Rust, so they're expected to bring their own
FlatBuffers tooling (likely the official `flatc`) when that work starts;
this crate only needs to serve the Rust targets (server, web).

## How codegen works

`build.rs` calls `planus-translation` (parse `fbs/protocol.fbs`) and
`planus-codegen` (emit Rust) directly as libraries — the same two calls
`planus-cli`'s own `planus rust` subcommand makes internally — and writes
the result to `$OUT_DIR/protocol.rs`, which `src/lib.rs` then `include!`s
under `lestobackupper_schema::protocol`. No `planus-cli` install required; a
plain `cargo build` regenerates the bindings whenever `fbs/protocol.fbs`
changes.

If you want richer diagnostics than a build failure gives you (e.g. after
a schema edit that doesn't parse), `cargo install planus-cli` and run
`planus check fbs/protocol.fbs` — same parser, much better error output.

## Shape of the generated API

Every table becomes a plain owned Rust struct (`CreateUserRequest { pub
username: String, pub password: String }`) — construct it with normal
struct literal syntax, no builder chains needed for simple cases. To
serialize: `planus::Builder::new().finish(&value, None) -> &[u8]`. To
deserialize: `SomeTypeRef::read_as_root(bytes)` (zero-copy), then
`SomeType::try_from(that_ref)` for an owned value. Unions
(`RequestBody`/`ResponseBody`) become Rust enums with one boxed variant per
member.
