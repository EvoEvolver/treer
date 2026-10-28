//! Embed the committed UI build (`public/`) so the installed launcher is a
//! single self-contained executable. Provider and Treer CLIs are resolved at
//! run time (see src/tools.rs), never pinned at build time.
use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("public");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut names = Vec::new();
    for entry in fs::read_dir(&root).expect("launchers/agent/public must exist") {
        let entry = entry.expect("read public entry");
        let kind = entry.file_type().expect("public entry type");
        assert!(kind.is_file(), "public/ must contain only regular files");
        let name = entry.file_name().into_string().expect("UTF-8 asset name");
        println!("cargo:rerun-if-changed={}", entry.path().display());
        names.push(name);
    }
    names.sort();
    let mut generated = String::from("pub(crate) const ASSETS: &[(&str, &[u8])] = &[\n");
    for name in names {
        let path = fs::canonicalize(root.join(&name)).unwrap();
        generated.push_str(&format!("    ({name:?}, include_bytes!({path:?})),\n"));
    }
    generated.push_str("];\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("ui_assets.rs"),
        generated,
    )
    .unwrap();
}
