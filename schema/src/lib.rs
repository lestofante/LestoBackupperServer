//! Generated FlatBuffers bindings for the client/server wire protocol.
//! `build.rs` runs `planus-translation` + `planus-codegen` against
//! `fbs/protocol.fbs` at build time — no external `flatc`/`planus-cli`
//! install required, just `cargo build`.

#![allow(clippy::all)] // generated code, not ours to fix

pub mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}
