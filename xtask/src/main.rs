// cargo xtask <task>: project tasks, written in Rust so building needs only
// cargo (the alias is in .cargo/config.toml).
//
//   web   builds the web client into web/pkg: the wasm for the browser, then
//         the JavaScript glue that loads it (wasm-bindgen, used as a library).
//         lestobackupper-server then serves web/ on its WebSocket port.

use std::path::{Path, PathBuf};
use std::process::{exit, Command};

fn main() {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("web") => web(),
        _ => {
            eprintln!("usage: cargo xtask web    build the web client into web/pkg");
            exit(2);
        }
    }
}

fn root() -> PathBuf {
    // xtask/ is one level below the repository root.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives inside the repository").to_path_buf()
}

fn web() {
    let web = root().join("web");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = Command::new(cargo)
        .args(["build", "--release", "--target", "wasm32-unknown-unknown", "--manifest-path"])
        .arg(web.join("Cargo.toml"))
        .status()
        .expect("cannot run cargo");
    if !status.success() {
        eprintln!("building the web client failed (is the target installed? rustup target add wasm32-unknown-unknown)");
        exit(1);
    }

    let release = web.join("target/wasm32-unknown-unknown/release");
    let wasm = ["lestobackupper-web.wasm", "lestobackupper_web.wasm"]
        .iter()
        .map(|name| release.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| {
            eprintln!("no .wasm in {}", release.display());
            exit(1)
        });

    let out = web.join("pkg");
    let mut bindgen = wasm_bindgen_cli_support::Bindgen::new();
    // As the wasm-bindgen program does: init() with no argument finds the
    // .wasm next to the .js (the library leaves that out unless asked).
    bindgen.input_path(&wasm).out_name("lestobackupper_web").typescript(false).omit_default_module_path(false);
    if let Err(e) = bindgen.web(true).and_then(|b| b.generate(&out)) {
        eprintln!("wasm-bindgen failed: {e:#}");
        exit(1);
    }
    println!("web client built in {}", out.display());
}
