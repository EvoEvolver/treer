//! `treer-launch` command line. Launch arguments become the runtime's
//! environment; `update` hands off to the local Controller binary, which owns
//! signed-release verification for every Treer executable.
use anyhow::{bail, Context, Result};

pub(crate) const USAGE: &str = "\
Usage: treer-launch [--harness codex|claude] [--session-id ID|--thread-id ID] [--cwd ABS] [--name NAME]
       treer-launch session --help   Claude export/import/prepare/launch/run (macOS/Linux)
       treer-launch update [--version vX.Y.Z | --channel NAME] [--release-base-url URL] [--public-key PEM]
       treer-launch --version

Starts the shared Agent UI for a native Codex or Claude session. Saved launch
profiles run `treer-launch --harness codex|claude`; `treer-agent` is a
deprecated alias of this command. `--runtime rust` is accepted and ignored.
`update` replaces only this launcher from the signed release (default: the
installed Controller's version); running Agents are not restarted.";

#[derive(Debug)]
pub(crate) enum Next {
    /// Continue into the runtime (or the `session` CLI) with the prepared environment.
    Run,
    Exit(i32),
}

fn set(name: &str, value: &str) {
    std::env::set_var(name, value);
}

/// A launch registers its own interface with the Controller, so a parent's
/// port observation file is dropped. An explicit `RUST_SPIKE_REGISTER`
/// (integration tests) leaves the runtime environment exactly as given.
fn register_by_default(drop_port_file: bool) {
    if std::env::var_os("RUST_SPIKE_REGISTER").is_none() {
        set("RUST_SPIKE_REGISTER", "1");
        if drop_port_file {
            std::env::remove_var("AGENT_UI_PORT_FILE");
        }
    }
}

/// Interpret argv before the runtime reads its environment.
pub(crate) fn prepare(args: &[String]) -> Result<Next> {
    match args.first().map(String::as_str) {
        // Bare invocation: the environment alone configures the runtime.
        None => return Ok(Next::Run),
        Some("--help" | "-h" | "help") if args.len() == 1 => {
            println!("{USAGE}");
            return Ok(Next::Exit(0));
        }
        Some(first) if first.starts_with("--") && args.iter().any(|arg| arg == "--help" || arg == "-h") => {
            println!("{USAGE}");
            return Ok(Next::Exit(0));
        }
        Some("--version" | "-V") if args.len() == 1 => {
            println!("treer-launch {}", treer_build_info::DISPLAY);
            return Ok(Next::Exit(0));
        }
        Some("update") => return update(&args[1..]).map(Next::Exit),
        Some("session") => {
            let sub = args.get(1).map(String::as_str);
            if matches!(sub, Some("launch" | "run")) {
                // A saved Host startup must never inherit a parent's observation file.
                register_by_default(sub == Some("run"));
            }
            return Ok(Next::Run);
        }
        // Runtime maintenance flags read directly by main.
        Some("--restore-node-backup" | "--fixture-create-private-state") => return Ok(Next::Run),
        _ => {}
    }
    // Only import flags: an environment-configured runtime start, as bare.
    if args.iter().all(|arg| matches!(arg.as_str(), "--import-node-state" | "--import-claude-state")) {
        return Ok(Next::Run);
    }
    let mut harness = std::env::var("AGENT_UI_HARNESS")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "codex".into());
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        if matches!(key, "--import-node-state" | "--import-claude-state") {
            index += 1;
            continue;
        }
        let Some(value) = args.get(index + 1).filter(|value| !value.is_empty()) else {
            bail!("incomplete launcher argument {key}; see treer-launch --help");
        };
        match key {
            "--runtime" if value == "rust" => {}
            "--runtime" => bail!("the Node runtime was removed; only --runtime rust is accepted"),
            "--harness" => harness = value.clone(),
            "--session-id" | "--thread-id" => set("AGENT_UI_SESSION_ID", value),
            "--cwd" => set("AGENT_UI_CWD", value),
            "--name" => set("AGENT_UI_NAME", value),
            _ => bail!("unsupported launcher argument {key}; see treer-launch --help"),
        }
        index += 2;
    }
    if !matches!(harness.as_str(), "codex" | "claude") {
        bail!("unsupported harness {harness}; select codex or claude");
    }
    set("AGENT_UI_HARNESS", &harness);
    if std::env::var_os("AGENT_UI_CWD").is_none() {
        if let Ok(cwd) = std::env::var("CODEX_UI_CWD") {
            set("AGENT_UI_CWD", &cwd);
        }
    }
    register_by_default(true);
    Ok(Next::Run)
}

/// `treer-launch update`: the local Controller binary downloads the signed
/// manifest, verifies the launcher artifact and replaces this executable.
fn update(args: &[String]) -> Result<i32> {
    if matches!(args, [flag] if flag == "--help" || flag == "-h") {
        println!("{USAGE}");
        return Ok(0);
    }
    let exe = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .context("cannot resolve the installed treer-launch executable")?;
    let directory = exe.parent().context("treer-launch has no parent directory")?;
    let controller = crate::tools::companion("treer-agent-server").context(
        "treer-agent-server was not found next to treer-launch or on PATH; install Treer first",
    )?;
    let status = std::process::Command::new(controller)
        .args(["launcher", "update", "--dir"])
        .arg(directory)
        .args(args)
        .status()
        .context("failed to run treer-agent-server launcher update")?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    // One test owns these process-wide variables.
    #[test]
    fn launch_arguments_become_runtime_environment() {
        for name in ["AGENT_UI_HARNESS", "AGENT_UI_SESSION_ID", "AGENT_UI_CWD", "RUST_SPIKE_REGISTER", "CODEX_UI_CWD"] {
            std::env::remove_var(name);
        }
        // An explicit registration choice keeps the given environment.
        std::env::set_var("RUST_SPIKE_REGISTER", "0");
        std::env::set_var("AGENT_UI_PORT_FILE", "/test/port");
        assert!(matches!(prepare(&args(&["--harness", "codex"])), Ok(Next::Run)));
        assert_eq!(std::env::var("RUST_SPIKE_REGISTER").unwrap(), "0");
        assert_eq!(std::env::var("AGENT_UI_PORT_FILE").unwrap(), "/test/port");
        std::env::remove_var("RUST_SPIKE_REGISTER");
        std::env::set_var("AGENT_UI_PORT_FILE", "/parent/port");
        let prepared = prepare(&args(&["--harness", "claude", "--runtime", "rust", "--session-id", "s1", "--cwd", "/work"]));
        assert!(matches!(prepared, Ok(Next::Run)));
        assert_eq!(std::env::var("AGENT_UI_HARNESS").unwrap(), "claude");
        assert_eq!(std::env::var("AGENT_UI_SESSION_ID").unwrap(), "s1");
        assert_eq!(std::env::var("AGENT_UI_CWD").unwrap(), "/work");
        assert_eq!(std::env::var("RUST_SPIKE_REGISTER").unwrap(), "1");
        assert!(std::env::var_os("AGENT_UI_PORT_FILE").is_none());

        std::env::remove_var("AGENT_UI_HARNESS");
        std::env::remove_var("AGENT_UI_CWD");
        std::env::set_var("CODEX_UI_CWD", "/legacy");
        assert!(matches!(prepare(&args(&["--thread-id", "t1"])), Ok(Next::Run)));
        assert_eq!(std::env::var("AGENT_UI_HARNESS").unwrap(), "codex");
        assert_eq!(std::env::var("AGENT_UI_SESSION_ID").unwrap(), "t1");
        assert_eq!(std::env::var("AGENT_UI_CWD").unwrap(), "/legacy");

        assert!(prepare(&args(&["--runtime", "node"])).unwrap_err().to_string().contains("Node runtime was removed"));
        assert!(prepare(&args(&["--harness", "acp"])).unwrap_err().to_string().contains("unsupported harness"));
        assert!(prepare(&args(&["--acp-command", "x"])).is_err());
        assert!(prepare(&args(&["--harness"])).is_err());
        assert!(matches!(prepare(&[]), Ok(Next::Run)));
        assert!(matches!(prepare(&args(&["--version"])), Ok(Next::Exit(0))));
        assert!(matches!(prepare(&args(&["--harness", "codex", "--help"])), Ok(Next::Exit(0))));
        assert!(matches!(prepare(&args(&["session", "--help"])), Ok(Next::Run)));
        for name in ["AGENT_UI_HARNESS", "AGENT_UI_SESSION_ID", "AGENT_UI_CWD", "RUST_SPIKE_REGISTER", "CODEX_UI_CWD"] {
            std::env::remove_var(name);
        }
    }
}
