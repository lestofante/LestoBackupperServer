use std::path::Path;

fn main() {
    let schema_path = "fbs/protocol.fbs";
    println!("cargo:rerun-if-changed={schema_path}");

    let options = planus_translation::ConverterOptions { ignore_docstring_errors: false };

    let declarations = planus_translation::translate_files_with_options(&[Path::new(schema_path)], options)
        .expect("failed to parse fbs/protocol.fbs — run `planus check fbs/protocol.fbs` (cargo install planus-cli) for full diagnostics");

    // Not formatted (false): that would need rustfmt installed to build, for
    // a file in target/ nobody reads.
    let code = planus_codegen::generate_rust(&declarations, false).expect("failed to generate rust bindings from fbs/protocol.fbs");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    std::fs::write(format!("{out_dir}/protocol.rs"), code).unwrap();
}
