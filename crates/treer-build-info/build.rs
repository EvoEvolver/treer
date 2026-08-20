use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=TREER_BUILD_COMMIT");
    watch_git_head();

    let commit = env::var("TREER_BUILD_COMMIT")
        .ok()
        .filter(|value| valid_commit(value))
        .or_else(git_commit)
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=TREER_BUILD_COMMIT={commit}");
}

fn git_commit() -> Option<String> {
    let root = workspace_root();
    let output = Command::new("git")
        .args(["-C", root.to_str()?, "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8(output.stdout).ok()?.trim().to_string();
    valid_commit(&commit).then_some(commit)
}

fn watch_git_head() {
    let root = workspace_root();
    let git = root.join(".git");
    let head = git.join("HEAD");
    println!("cargo:rerun-if-changed={}", head.display());

    let Ok(reference) = std::fs::read_to_string(head) else {
        return;
    };
    let Some(reference) = reference.trim().strip_prefix("ref: ") else {
        return;
    };
    println!("cargo:rerun-if-changed={}", git.join(reference).display());
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"))
        .join("../..")
}

fn valid_commit(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
