use std::{env, path::PathBuf};

fn validate_native(path: &std::path::Path) {
    use std::io::Read;
    let mut prefix = [0; 2];
    std::fs::File::open(path)
        .expect("native tool path must exist")
        .read_exact(&mut prefix)
        .expect("native tool header");
    assert_ne!(
        &prefix, b"#!",
        "default builds require pinned native tools, not script interpreters"
    );
    #[cfg(windows)]
    assert_eq!(
        &prefix, b"MZ",
        "Windows default builds require native PE tools"
    );
}

// A native build host may validate one image while the deployment uses a
// different absolute path. This is build-time binding only: runtime overrides
// remain forbidden, and the artifact manifest must hash both images.
fn deployment_path(key: &str, image: &std::path::Path) -> PathBuf {
    println!("cargo:rerun-if-env-changed={key}");
    let mut path = env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| image.to_owned());
    // Windows canonicalize returns a verbatim drive prefix. Bind its ordinary
    // drive spelling, while still rejecting UNC and device namespace paths.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        if let Some(value) = path
            .to_str()
            .and_then(|value| value.strip_prefix("\\\\?\\"))
        {
            path = PathBuf::from(value);
        }
    }
    let text = path.to_str().expect("deployment tool path must be UTF-8");
    assert!(
        !text.chars().any(char::is_control),
        "deployment tool path contains control characters"
    );
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let bytes = text.as_bytes();
        assert!(
            bytes.len() > 3 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == b":\\",
            "Windows deployment tool path must be an absolute drive path"
        );
        assert!(
            text[3..].split('\\').all(|part| !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with([' ', '.'])
                && !part.contains([':', '/'])),
            "Windows deployment tool path must be normalized"
        );
        assert!(
            text.to_ascii_lowercase().ends_with(".exe"),
            "Windows tools must be native .exe images"
        );
    } else {
        assert!(path.is_absolute(), "deployment tool path must be absolute");
        assert!(
            path.components().all(|c| !matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )),
            "deployment tool path must be normalized"
        );
    }
    path
}

fn main() {
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-env-changed=TREER_SPIKE_BUILD_CODEX");
    println!("cargo:rerun-if-env-changed=TREER_SPIKE_BUILD_CLAUDE");
    println!("cargo:rerun-if-env-changed=TREER_SPIKE_BUILD_TREER");
    for (name, key) in [("codex", "SPIKE_CODEX_PATH"), ("treer", "SPIKE_TREER_PATH")] {
        let selected =
            env::var_os(format!("TREER_SPIKE_BUILD_{}", name.to_uppercase())).map(PathBuf::from);
        if let Some(path) = &selected {
            assert!(path.is_absolute(), "build-time tool path must be absolute");
        }
        let path = selected
            .or_else(|| {
                env::split_paths(&env::var_os("PATH").expect("build PATH required"))
                    .map(|dir| {
                        dir.join(if cfg!(windows) {
                            format!("{name}.exe")
                        } else {
                            name.to_owned()
                        })
                    })
                    .find(|path| path.is_file())
            })
            .and_then(|path| path.canonicalize().ok())
            .expect(
                "build requires installed codex and treer; record resolved binary hashes before P2",
            );
        assert!(PathBuf::from(&path).is_absolute());
        if env::var_os("CARGO_FEATURE_FIXTURE_HARNESS").is_none() {
            validate_native(&path);
        }
        let target = deployment_path(
            &format!("TREER_SPIKE_TARGET_{}", name.to_uppercase()),
            &path,
        );
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
            validate_native(&path);
        }
        let target = deployment_path("TREER_SPIKE_TARGET_CLAUDE", &path);
        println!("cargo:rustc-env=SPIKE_CLAUDE_PATH={}", target.display());
    } else {
        println!("cargo:rustc-env=SPIKE_CLAUDE_PATH=");
    }
}
