use std::{env, path::PathBuf};

// A native build host may validate one image while the deployment uses a
// different absolute path. This is build-time binding only: runtime overrides
// remain forbidden, and the artifact manifest must hash both images.
fn deployment_path(key: &str, image: &std::path::Path) -> PathBuf {
    println!("cargo:rerun-if-env-changed={key}");
    let path = env::var_os(key).map(PathBuf::from).unwrap_or_else(|| image.to_owned());
    assert!(path.is_absolute(), "deployment tool path must be absolute");
    let text = path.to_str().expect("deployment tool path must be UTF-8");
    assert!(!text.chars().any(char::is_control), "deployment tool path contains control characters");
    assert!(path.components().all(|c| !matches!(c, std::path::Component::ParentDir | std::path::Component::CurDir)), "deployment tool path must be normalized");
    path
}

fn main() {
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-env-changed=TREER_SPIKE_BUILD_CODEX");
    println!("cargo:rerun-if-env-changed=TREER_SPIKE_BUILD_CLAUDE");
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
        let target = deployment_path(&format!("TREER_SPIKE_TARGET_{}", name.to_uppercase()), &path);
        println!("cargo:rustc-env={key}={}", target.display());
    }
    let claude = env::var_os("TREER_SPIKE_BUILD_CLAUDE")
        .map(PathBuf::from)
        .map(|p| {
            assert!(p.is_absolute(), "Claude build-time path must be absolute");
            p.canonicalize().expect("Claude build-time path must exist")
        });
    if let Some(path) = claude {
        assert!(path.is_absolute());
        if env::var_os("CARGO_FEATURE_FIXTURE_HARNESS").is_none() {
            use std::io::Read;
            let mut prefix = [0; 2];
            std::fs::File::open(&path)
                .unwrap()
                .read_exact(&mut prefix)
                .unwrap();
            assert_ne!(
                &prefix, b"#!",
                "Claude production path must be a pinned native executable"
            );
        }
        let target = deployment_path("TREER_SPIKE_TARGET_CLAUDE", &path);
        println!("cargo:rustc-env=SPIKE_CLAUDE_PATH={}", target.display());
    } else {
        println!("cargo:rustc-env=SPIKE_CLAUDE_PATH=");
    }
}
