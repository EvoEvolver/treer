//! Linux memory-pressure isolation for Host workloads.
//!
//! systemd-oomd kills whole cgroups. When every Agent shares the Host's cgroup
//! (for example a terminal's scope), one memory-hungry Agent takes down the
//! Host, the Controller and every other Agent. When the user systemd manager is
//! usable, each workload therefore runs in its own transient scope; otherwise
//! (and in addition) workloads raise their `oom_score_adj` so the kernel OOM
//! killer prefers them over the Host and Controller.
use std::{
    env,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};

/// Unprivileged processes may raise but not lower their OOM score.
pub const WORKLOAD_OOM_SCORE_ADJ: i32 = 500;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadIsolation {
    /// Each workload runs in its own transient user scope. `literal` is set
    /// when systemd-run accepts `--expand-environment=no`: since systemd 254
    /// it otherwise expands `$VAR` in the command line (older versions never
    /// expand and reject the flag).
    SystemdScope { literal: bool },
    /// Only the workload's OOM score is raised.
    OomScore,
}

/// `TREER_SYSTEMD_RUN` overrides the executable; setting it to `false`
/// disables systemd scopes.
pub fn systemd_run_executable() -> OsString {
    env::var_os("TREER_SYSTEMD_RUN").unwrap_or_else(|| "systemd-run".into())
}

const NO_EXPAND: &str = "--expand-environment=no";

/// The user manager can create scopes iff a trivial scoped command succeeds.
/// Returns whether `--expand-environment=no` is supported (and needed).
pub fn probe_systemd_user_scope(executable: &OsStr) -> Result<bool, String> {
    if run_scope_probe(executable, true).is_ok() {
        return Ok(true);
    }
    run_scope_probe(executable, false).map(|()| false)
}

fn run_scope_probe(executable: &OsStr, literal: bool) -> Result<(), String> {
    let mut command = Command::new(executable);
    command.args(["--user", "--scope", "--quiet", "--collect"]);
    if literal {
        command.arg(NO_EXPAND);
    }
    let mut child = command
        .arg("true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to run {}: {error}", Path::new(executable).display()))?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "systemd-run --user --scope did not finish within {} seconds",
                    PROBE_TIMEOUT.as_secs()
                ));
            }
            Err(error) => return Err(format!("failed to wait for systemd-run: {error}")),
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut detail = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = io::Read::read_to_string(&mut stderr, &mut detail);
    }
    let detail = detail.trim();
    if detail.is_empty() {
        Err(format!("systemd-run --user --scope exited with {status}"))
    } else {
        Err(format!("systemd-run --user --scope failed: {detail}"))
    }
}

/// Probe once per process and log the chosen mode.
pub fn workload_isolation() -> WorkloadIsolation {
    static MODE: OnceLock<WorkloadIsolation> = OnceLock::new();
    *MODE.get_or_init(
        || match probe_systemd_user_scope(&systemd_run_executable()) {
            Ok(literal) => {
                tracing::info!(
                    literal,
                    "workload isolation: one systemd user scope per workload"
                );
                WorkloadIsolation::SystemdScope { literal }
            }
            Err(reason) => {
                tracing::info!(
                    %reason,
                    "workload isolation: systemd user scopes unavailable; raising workload oom_score_adj only"
                );
                WorkloadIsolation::OomScore
            }
        },
    )
}

/// A unit-name component limited to characters systemd accepts unescaped.
pub fn systemd_unit_component(value: &str) -> String {
    value
        .chars()
        .take(96)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// `systemd-run --scope` registers itself in a new scope and then execs the
/// command, so the PID, session and process group stay those of the caller.
pub fn systemd_scope_prefix(unit: &str, literal: bool) -> Vec<OsString> {
    let mut prefix: Vec<OsString> = vec![
        systemd_run_executable(),
        "--user".into(),
        "--scope".into(),
        "--quiet".into(),
        "--collect".into(),
    ];
    if literal {
        prefix.push(NO_EXPAND.into());
    }
    prefix.push(format!("--unit={unit}").into());
    prefix.push("--".into());
    prefix
}

/// Raise (never lower) the shell's OOM score, then exec the command in place.
/// Failures are ignored: isolation must never prevent a workload start.
fn workload_wrapper() -> String {
    let score = WORKLOAD_OOM_SCORE_ADJ;
    format!(
        r#"{{ read current </proc/self/oom_score_adj && [ "$current" -lt {score} ] && echo {score} >/proc/self/oom_score_adj; }} 2>/dev/null; exec "$@""#
    )
}

/// Program and arguments that start `program` isolated. Every step execs in
/// place, so the spawned PID is the workload's PID.
pub fn isolated_workload_command(
    isolation: WorkloadIsolation,
    unit: &str,
    program: &Path,
    args: &[String],
) -> (PathBuf, Vec<OsString>) {
    let mut wrapped: Vec<OsString> = vec![
        "-c".into(),
        workload_wrapper().into(),
        "treer-workload".into(),
    ];
    if let WorkloadIsolation::SystemdScope { literal } = isolation {
        wrapped.extend(systemd_scope_prefix(unit, literal));
    }
    wrapped.push(program.into());
    wrapped.extend(args.iter().map(OsString::from));
    (PathBuf::from("/bin/sh"), wrapped)
}

/// Resolve a workload command the way a direct spawn would, so a missing
/// program is still reported at spawn time rather than by the wrapper shell.
pub fn resolve_workload_program(
    command: &str,
    cwd: &Path,
    path: Option<&OsStr>,
) -> Result<PathBuf, String> {
    let executable = |candidate: &Path| {
        candidate.is_file() && rustix::fs::access(candidate, rustix::fs::Access::EXEC_OK).is_ok()
    };
    if command.contains('/') {
        let candidate = cwd.join(command);
        return if executable(&candidate) {
            Ok(candidate)
        } else {
            Err(format!(
                "Unable to spawn {} because it does not exist or is not executable",
                candidate.display()
            ))
        };
    }
    let Some(path) = path else {
        return Err(format!(
            "Unable to spawn {command} because the PATH is not set"
        ));
    };
    env::split_paths(path)
        .map(|directory| cwd.join(directory).join(command))
        .find(|candidate| executable(candidate))
        .ok_or_else(|| {
            format!(
                "Unable to spawn {command} because no viable candidate was found in PATH {path:?}"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temporary(label: &str) -> PathBuf {
        let directory =
            env::temp_dir().join(format!("treer-isolation-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create temporary directory");
        directory
    }

    fn script(path: &Path, body: &str) {
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("make script executable");
    }

    #[test]
    fn probe_reports_success_failure_and_missing_executable() {
        let directory = temporary("probe");
        let ok = directory.join("ok");
        script(
            &ok,
            r#"[ "$*" = "--user --scope --quiet --collect --expand-environment=no true" ] || exit 9"#,
        );
        assert_eq!(probe_systemd_user_scope(ok.as_os_str()), Ok(true));
        // systemd < 254 rejects the flag but never expands arguments.
        let old = directory.join("old");
        script(
            &old,
            r#"case "$*" in *--expand-environment*) echo "unrecognized option" >&2; exit 1;; esac"#,
        );
        assert_eq!(probe_systemd_user_scope(old.as_os_str()), Ok(false));

        let failing = directory.join("failing");
        script(
            &failing,
            "echo 'Failed to connect to bus: No medium found' >&2; exit 1",
        );
        let error = probe_systemd_user_scope(failing.as_os_str()).expect_err("no user bus");
        assert!(error.contains("No medium found"), "{error}");

        let error = probe_systemd_user_scope(directory.join("missing").as_os_str())
            .expect_err("missing systemd-run");
        assert!(error.contains("failed to run"), "{error}");
        std::fs::remove_dir_all(directory).expect("remove probe directory");
    }

    #[test]
    fn command_wraps_in_scope_only_when_systemd_is_usable() {
        let args = vec!["--flag".to_string(), "two words".to_string()];
        let program = Path::new("/usr/bin/codex");
        let (shell, fallback) =
            isolated_workload_command(WorkloadIsolation::OomScore, "u", program, &args);
        assert_eq!(shell, Path::new("/bin/sh"));
        assert_eq!(
            fallback[2..],
            ["treer-workload", "/usr/bin/codex", "--flag", "two words"].map(OsString::from)
        );
        let (_, scoped) = isolated_workload_command(
            WorkloadIsolation::SystemdScope { literal: true },
            "treer-workload-a_1-x",
            program,
            &args,
        );
        let scoped: Vec<_> = scoped[3..]
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            scoped,
            [
                systemd_run_executable().to_string_lossy().as_ref(),
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "--expand-environment=no",
                "--unit=treer-workload-a_1-x",
                "--",
                "/usr/bin/codex",
                "--flag",
                "two words",
            ]
        );
        assert_eq!(systemd_unit_component("agent:a/b c-1.x"), "agent_a_b_c-1.x");
    }

    #[test]
    fn wrapper_execs_in_place_and_never_lowers_the_score() {
        let directory = temporary("wrapper");
        let probe = directory.join("probe");
        // Report this process's PID and OOM score after the wrapper exec'd it.
        script(
            &probe,
            "echo \"$$ $(cat /proc/self/oom_score_adj 2>/dev/null || echo none) $1\"",
        );
        let (shell, args) = isolated_workload_command(
            WorkloadIsolation::OomScore,
            "unused",
            &probe,
            &["arg".to_string()],
        );
        let child = Command::new(&shell)
            .args(&args)
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn wrapper");
        let pid = child.id();
        let output = child.wait_with_output().expect("wait wrapper");
        let line = String::from_utf8(output.stdout).expect("UTF-8 output");
        let fields: Vec<_> = line.split_whitespace().collect();
        assert_eq!(fields[0], pid.to_string(), "wrapper must exec in place");
        assert_eq!(fields[2], "arg");
        if cfg!(target_os = "linux") {
            assert_eq!(fields[1], "500");
            // An already higher score is kept.
            let higher = Command::new("/bin/sh")
                .arg("-c")
                .arg("echo 800 >/proc/self/oom_score_adj && exec \"$@\"")
                .arg("sh")
                .arg(&shell)
                .args(&args)
                .output()
                .expect("spawn higher-score wrapper");
            let line = String::from_utf8(higher.stdout).expect("UTF-8 output");
            assert_eq!(line.split_whitespace().nth(1), Some("800"));
        }
        std::fs::remove_dir_all(directory).expect("remove wrapper directory");
    }

    #[test]
    fn resolves_programs_like_a_direct_spawn() {
        let directory = temporary("resolve");
        let bin = directory.join("bin");
        std::fs::create_dir_all(&bin).expect("create bin");
        script(&bin.join("tool"), "true");
        std::fs::write(bin.join("plain"), "data").expect("write non-executable");
        let path = OsString::from("/nonexistent:bin");
        assert_eq!(
            resolve_workload_program("tool", &directory, Some(&path)),
            Ok(bin.join("tool"))
        );
        assert_eq!(
            resolve_workload_program("./bin/tool", &directory, None),
            Ok(directory.join("./bin/tool"))
        );
        assert!(resolve_workload_program("plain", &directory, Some(&path)).is_err());
        assert!(resolve_workload_program("missing", &directory, Some(&path)).is_err());
        assert!(resolve_workload_program("tool", &directory, None).is_err());
        std::fs::remove_dir_all(directory).expect("remove resolve directory");
    }
}
