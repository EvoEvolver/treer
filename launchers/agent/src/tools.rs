//! Run-time lookup of the Treer CLI and provider CLIs. Same-user processes are
//! trusted: explicit environment overrides win, then PATH.
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn executable_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// First regular file named `name` in an absolute PATH directory.
pub(crate) fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(executable_name(name)))
        .find(|candidate| candidate.is_file())
}

fn sibling(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let candidate = exe.with_file_name(executable_name(name));
    candidate.is_file().then_some(candidate)
}

/// The Treer CLI: the Controller-provided `TREER_BIN`, the legacy
/// `AGENT_UI_TREER_BIN`, PATH, then the launcher's own directory.
pub(crate) fn treer() -> String {
    env("TREER_BIN")
        .or_else(|| env("AGENT_UI_TREER_BIN"))
        .map(PathBuf::from)
        .or_else(|| which("treer"))
        .or_else(|| sibling("treer"))
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "treer".into())
}

/// The provider CLI for `harness`: `CODEX_BIN` / `CLAUDE_BIN`, else PATH.
pub(crate) fn provider(harness: &str) -> Result<String> {
    let (variable, name) = match harness {
        "claude" => ("CLAUDE_BIN", "claude"),
        _ => ("CODEX_BIN", "codex"),
    };
    if let Some(path) = env(variable) {
        if !Path::new(&path).is_absolute() {
            bail!("{variable} must be an absolute path");
        }
        return Ok(path);
    }
    match which(name) {
        Some(path) => Ok(path.display().to_string()),
        None => bail!("{name} was not found on PATH; install it or set {variable} to its absolute path"),
    }
}

/// A sibling of the running launcher, else PATH.
pub(crate) fn companion(name: &str) -> Option<PathBuf> {
    sibling(name).or_else(|| which(name))
}
