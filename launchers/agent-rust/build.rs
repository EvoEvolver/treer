use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-env-changed=TREER_SPIKE_BUILD_CODEX");
    for (name, key) in [("codex", "SPIKE_CODEX_PATH"), ("treer", "SPIKE_TREER_PATH")] {
        let selected = if name == "codex" {
            env::var_os("TREER_SPIKE_BUILD_CODEX").map(PathBuf::from)
        } else {
            None
        };
        if let Some(path) = &selected {
            assert!(path.is_absolute(), "build-time tool path must be absolute");
        }
        let path = selected
            .or_else(|| {
                env::split_paths(&env::var_os("PATH").expect("build PATH required"))
                    .map(|dir| dir.join(name))
                    .find(|path| path.is_file())
            })
            .and_then(|path| path.canonicalize().ok())
            .expect(
                "build requires installed codex and treer; record resolved binary hashes before P2",
            );
        assert!(PathBuf::from(&path).is_absolute());
        if env::var_os("CARGO_FEATURE_FIXTURE_HARNESS").is_none() {
            use std::io::Read;
            let mut prefix = [0; 2];
            std::fs::File::open(&path)
                .expect("tool path must exist")
                .read_exact(&mut prefix)
                .expect("tool header");
            assert_ne!(&prefix, b"#!", "default builds require native tools, not PATH-dependent script interpreters; set TREER_SPIKE_BUILD_CODEX to the absolute native Codex binary at build time");
        }
        println!("cargo:rustc-env={key}={}", path.display());
    }
}
