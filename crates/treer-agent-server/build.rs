use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':', '\0'])
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && !part.chars().any(char::is_control)
        })
}

fn regular_file(root: &Path, name: &str) -> Vec<u8> {
    assert!(safe_path(name), "unsafe launcher payload path");
    let mut path = root.to_path_buf();
    for part in name.split('/') {
        path.push(part);
        let metadata = fs::symlink_metadata(&path).expect("missing launcher payload component");
        assert!(
            !metadata.file_type().is_symlink(),
            "launcher payload contains a symlink"
        );
    }
    assert!(path.is_file(), "launcher payload is not a regular file");
    println!("cargo:rerun-if-changed={}", path.display());
    fs::read(path).expect("read launcher payload")
}

fn source_files(root: &Path, prefix: &str, files: &mut Vec<(String, bool)>) {
    let directory = root.join(prefix);
    println!("cargo:rerun-if-changed={}", directory.display());
    for item in fs::read_dir(directory).expect("read launcher sources") {
        let item = item.expect("read launcher entry");
        let name = item
            .file_name()
            .into_string()
            .expect("UTF-8 launcher filename");
        let name = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let kind = item.file_type().expect("launcher file type");
        assert!(!kind.is_symlink(), "launcher source contains symlink");
        if kind.is_dir() {
            source_files(root, &name, files);
        } else if kind.is_file() {
            files.push((name.clone(), name.ends_with(".sh")));
        }
    }
}

fn main() {
    for variable in [
        "TREER_LAUNCHER_PAYLOAD_DIR",
        "TREER_REQUIRE_LAUNCHER_BUNDLE",
        "TREER_BUILD_COMMIT",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    let repository = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    // Re-run when a checkout/worktree advances, even without changes to payload files.
    println!(
        "cargo:rerun-if-changed={}",
        repository.join(".git").display()
    );
    let mut refs = vec!["HEAD".to_owned(), "packed-refs".to_owned()];
    if let Ok(output) = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(&repository)
        .output()
    {
        if output.status.success() {
            refs.push(String::from_utf8(output.stdout).unwrap().trim().to_owned());
        }
    }
    for suffix in &refs {
        if let Ok(output) = Command::new("git")
            .args(["rev-parse", "--git-path", suffix])
            .current_dir(&repository)
            .output()
        {
            if output.status.success() {
                let path = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
                println!(
                    "cargo:rerun-if-changed={}",
                    if path.is_absolute() {
                        path
                    } else {
                        repository.join(path)
                    }
                    .display()
                );
            }
        }
    }
    let commit = env::var("TREER_BUILD_COMMIT").unwrap_or_else(|_| {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repository)
            .output()
            .expect("git build identity");
        assert!(
            output.status.success(),
            "cannot resolve launcher source commit"
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    });
    let prepared = env::var_os("TREER_LAUNCHER_PAYLOAD_DIR");
    let root = prepared
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| repository.join("launchers/agent"));
    assert!(
        !fs::symlink_metadata(&root)
            .expect("launcher payload root")
            .file_type()
            .is_symlink(),
        "payload root must not be a symlink"
    );
    let mut entries = Vec::new();
    let mut manifest_bytes = None;
    let ready = if prepared.is_some() {
        let bytes = regular_file(&root, "bundle-manifest.json");
        let manifest: serde_json::Value =
            serde_json::from_slice(&bytes).expect("launcher payload manifest");
        assert_eq!(
            manifest["schema"], "treer.launcher-payload/v1",
            "unknown payload schema"
        );
        assert_eq!(
            manifest["source_commit"].as_str(),
            Some(commit.as_str()),
            "launcher payload belongs to another commit"
        );
        assert_eq!(
            manifest["runtime_ready"], true,
            "release runtime is not prepared"
        );
        assert_eq!(
            manifest["node_min"], 22,
            "unsupported payload Node requirement"
        );
        assert_eq!(
            manifest["entry"], "runtime.mjs",
            "unsupported runtime entry"
        );
        for file in manifest["files"].as_array().expect("payload files") {
            let name = file["path"].as_str().expect("payload filename");
            assert_ne!(name, "bundle-manifest.json", "self-referential manifest");
            let bytes = regular_file(&root, name);
            assert_eq!(
                file["size"].as_u64(),
                Some(bytes.len() as u64),
                "payload length mismatch: {name}"
            );
            assert_eq!(
                file["sha256"].as_str(),
                Some(format!("{:x}", Sha256::digest(&bytes)).as_str()),
                "payload hash mismatch: {name}"
            );
            entries.push((
                name.to_owned(),
                file["executable"]
                    .as_bool()
                    .expect("payload executable flag"),
            ));
        }
        for required in [
            "runtime.mjs",
            "interface-contract.json",
            "profiles.json",
            "scripts/install.mjs",
            "scripts/installed-runner.mjs",
            "scripts/treer-agent.mjs",
        ] {
            assert!(
                entries.iter().any(|(name, _)| name == required),
                "missing required bundled file: {required}"
            );
        }
        manifest_bytes = Some(bytes);
        true
    } else {
        assert_ne!(env::var("PROFILE").ok().as_deref(), Some("release"),
            "release builds require a verified TREER_LAUNCHER_PAYLOAD_DIR; source-only builds are development-only");
        assert_ne!(
            env::var("TREER_REQUIRE_LAUNCHER_BUNDLE").ok().as_deref(),
            Some("1"),
            "official builds require a prepared TREER_LAUNCHER_PAYLOAD_DIR"
        );
        for directory in ["public", "scripts"] {
            source_files(&root, directory, &mut entries);
        }
        for file in ["package.json", "package-lock.json", "profiles.json"] {
            entries.push((file.to_owned(), false));
        }
        // Transitional development checkouts can precede the launcher-owner commit.
        // Release payloads above always require the shared contract.
        if root.join("interface-contract.json").exists() {
            entries.push(("interface-contract.json".to_owned(), false));
        }
        false
    };
    entries.sort();
    assert!(
        entries.windows(2).all(|pair| pair[0].0 != pair[1].0),
        "duplicate launcher payload file"
    );
    assert!(entries.len() <= 1024, "too many launcher payload files");
    let mut manifest_files = Vec::new();
    let mut generated = format!("const RUNTIME_READY: bool = {ready};\nconst SOURCE_COMMIT: &str = {commit:?};\nconst FILES: &[EmbeddedFile] = &[\n");
    let mut size = 0usize;
    for (name, executable) in entries {
        let bytes = regular_file(&root, &name);
        size += bytes.len();
        assert!(size <= 32 * 1024 * 1024, "launcher payload exceeds 32 MiB");
        let digest = format!("{:x}", Sha256::digest(&bytes));
        manifest_files.push(serde_json::json!({"path":name,"size":bytes.len(),"sha256":digest,"executable":executable}));
        generated.push_str(&format!("EmbeddedFile {{ path: {name:?}, bytes: include_bytes!({:?}), sha256: {digest:?}, executable: {executable} }},\n", fs::canonicalize(root.join(&name)).unwrap()));
    }
    let bytes = manifest_bytes.unwrap_or_else(|| {
        serde_json::to_vec(&serde_json::json!({
            "schema":"treer.launcher-payload/v1", "source_commit":commit, "runtime_ready":ready,
            "node_min":22,"entry":"runtime.mjs", "files":manifest_files
        }))
        .unwrap()
    });
    generated.push_str(&format!(
        "];\nconst PAYLOAD_DIGEST: &str = \"{:x}\";\nconst MANIFEST_JSON: &str = {:?};\n",
        Sha256::digest(&bytes),
        String::from_utf8(bytes).unwrap()
    ));
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("launcher_payload.rs"),
        generated,
    )
    .unwrap();
}
