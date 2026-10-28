//! Run-time lookup of the Treer CLI and provider CLIs. Same-user processes are
//! trusted: explicit environment overrides win, then PATH.
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// File names tried for `name` in each PATH directory, in order. Windows also
/// accepts the `.cmd`/`.bat` shims that npm, pnpm and yarn install.
fn candidates(name: &str) -> Vec<String> {
    if cfg!(windows) {
        ["exe", "cmd", "bat"].iter().map(|ext| format!("{name}.{ext}")).collect()
    } else {
        vec![name.to_string()]
    }
}

/// First regular file for `name` in an absolute directory of `path`.
fn which_in(path: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|directory| directory.is_absolute())
        .find_map(|directory| {
            candidates(name)
                .into_iter()
                .map(|file| directory.join(file))
                .find(|candidate| candidate.is_file())
        })
}

/// First regular file named `name` in an absolute PATH directory.
pub(crate) fn which(name: &str) -> Option<PathBuf> {
    which_in(&std::env::var_os("PATH")?, name)
}

fn sibling(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let candidate = exe.with_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX));
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

/// The provider CLI for `harness` (`CODEX_BIN` / `CLAUDE_BIN`, else PATH) as
/// the program to spawn and the arguments that precede the provider's own.
pub(crate) fn provider(harness: &str) -> Result<(String, Vec<String>)> {
    let (variable, name) = match harness {
        "claude" => ("CLAUDE_BIN", "claude"),
        _ => ("CODEX_BIN", "codex"),
    };
    let path = match env(variable) {
        Some(path) if !Path::new(&path).is_absolute() => bail!("{variable} must be an absolute path"),
        Some(path) => PathBuf::from(path),
        None => match which(name) {
            Some(path) => path,
            None => bail!("{name} was not found on PATH; install it or set {variable} to its absolute path"),
        },
    };
    Ok(launchable(path))
}

/// Windows batch shims cannot be spawned like an executable. An npm/pnpm/yarn
/// shim only runs `node <script>` (or a bundled `.exe`), so run that target
/// directly: no cmd.exe, no batch quoting, and the provider is the direct child
/// in its Job Object. Any other batch file is run by Rust's standard library,
/// which starts it through `cmd.exe /d /c` with its batch-safe argument
/// escaping (refusing arguments it cannot escape).
fn launchable(path: PathBuf) -> (String, Vec<String>) {
    let batch = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    if batch {
        if let Some(target) = shim_target(&path) {
            return target;
        }
    }
    (path.display().to_string(), Vec::new())
}

fn shim_target(shim: &Path) -> Option<(String, Vec<String>)> {
    let bytes = std::fs::read(shim).ok().filter(|bytes| bytes.len() <= 64 * 1024)?;
    let directory = shim.parent()?;
    let target = directory.join(shim_relative_target(&String::from_utf8_lossy(&bytes))?);
    if !target.is_file() {
        return None;
    }
    let extension = target.extension()?.to_str()?.to_ascii_lowercase();
    if extension == "exe" {
        return Some((target.display().to_string(), Vec::new()));
    }
    // The shims prefer a node.exe next to them, then node on PATH.
    let node = Some(directory.join("node.exe"))
        .filter(|node| node.is_file())
        .or_else(|| which("node"))?;
    Some((node.display().to_string(), vec![target.display().to_string()]))
}

/// The script (or bundled executable) a cmd-shim runs, relative to the
/// shim's directory: the last quoted `%dp0%\...` / `%~dp0\...` path ending in
/// .js/.cjs/.mjs/.exe (npm's `"%dp0%\node_modules\pkg\cli.js" %*`, pnpm's
/// `"%~dp0\..\pkg\cli.js" %*`). `node.exe` itself is not a target.
fn shim_relative_target(content: &str) -> Option<PathBuf> {
    content
        .split('"')
        .skip(1)
        .step_by(2)
        .filter_map(|quoted| {
            quoted
                .strip_prefix("%dp0%")
                .or_else(|| quoted.strip_prefix("%~dp0"))
        })
        .map(|relative| relative.trim_start_matches(['\\', '/']))
        .filter(|relative| {
            let lower = relative.to_ascii_lowercase();
            [".js", ".cjs", ".mjs", ".exe"].iter().any(|ext| lower.ends_with(ext))
                && lower != "node.exe"
                && !relative.contains('%')
        })
        .last()
        .map(|relative| relative.split(['\\', '/']).collect())
}

/// A sibling of the running launcher, else PATH.
pub(crate) fn companion(name: &str) -> Option<PathBuf> {
    sibling(name).or_else(|| which(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NPM: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n";
    const PNPM: &str = "@SETLOCAL\r\n@IF NOT DEFINED NODE_PATH (\r\n  @SET \"NODE_PATH=%~dp0\\..\\pnpm\\node_modules\"\r\n)\r\n@IF EXIST \"%~dp0\\node.exe\" (\r\n  \"%~dp0\\node.exe\"  \"%~dp0\\..\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n) ELSE (\r\n  @SET PATHEXT=%PATHEXT:;.JS;=;%\r\n  node  \"%~dp0\\..\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n)\r\n";

    #[test]
    fn npm_and_pnpm_shims_name_their_script() {
        let npm: PathBuf = ["node_modules", "@openai", "codex", "bin", "codex.js"].iter().collect();
        assert_eq!(shim_relative_target(NPM), Some(npm));
        let pnpm: PathBuf = ["..", "@anthropic-ai", "claude-code", "cli.js"].iter().collect();
        assert_eq!(shim_relative_target(PNPM), Some(pnpm));
        assert_eq!(shim_relative_target("@echo off\r\nC:\\tools\\real.exe %*\r\n"), None);
        assert_eq!(shim_relative_target("\"%dp0%\\node.exe\" %*"), None);
    }

    #[test]
    fn path_lookup_takes_directories_in_order() {
        let base = std::env::temp_dir().join(format!("rust-tools-{}", uuid::Uuid::new_v4()));
        let (first, second) = (base.join("first"), base.join("second"));
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let file = candidates("provider").pop().unwrap();
        std::fs::write(second.join(&file), "").unwrap();
        let path = std::env::join_paths([Path::new("relative"), &first, &second]).unwrap();
        assert_eq!(which_in(&path, "provider"), Some(second.join(&file)));
        std::fs::write(first.join(&file), "").unwrap();
        assert_eq!(which_in(&path, "provider"), Some(first.join(&file)));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn shim_resolves_to_node_and_script_next_to_it() {
        let base = std::env::temp_dir().join(format!("rust-shim-{}", uuid::Uuid::new_v4()));
        let script = base.join("node_modules/@openai/codex/bin/codex.js");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "").unwrap();
        std::fs::write(base.join("node.exe"), "").unwrap();
        std::fs::write(base.join("codex.cmd"), NPM).unwrap();
        let (program, args) = launchable(base.join("codex.cmd"));
        assert_eq!(PathBuf::from(program), base.join("node.exe"));
        assert_eq!(args.len(), 1);
        assert_eq!(std::fs::canonicalize(&args[0]).unwrap(), std::fs::canonicalize(&script).unwrap());
        // A batch file that is not a shim is run as itself.
        std::fs::write(base.join("other.cmd"), "@echo off\r\n").unwrap();
        assert_eq!(launchable(base.join("other.cmd")), (base.join("other.cmd").display().to_string(), Vec::new()));
        std::fs::remove_dir_all(base).unwrap();
    }

    /// The fallback relies on std running a batch file through cmd.exe with
    /// its argument escaping; provider arguments must arrive intact.
    #[cfg(windows)]
    #[test]
    fn windows_batch_provider_receives_arguments_intact() {
        let base = std::env::temp_dir().join(format!("rust-batch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let script = base.join("provider.cmd");
        std::fs::write(&script, "@echo off\r\necho [%1] [%2] [%3]\r\n").unwrap();
        let (program, args) = launchable(script);
        let output = std::process::Command::new(program)
            .args(args)
            .args(["app-server", "--listen", "stdio://"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "[app-server] [--listen] [stdio://]");
        std::fs::remove_dir_all(base).unwrap();
    }
}
