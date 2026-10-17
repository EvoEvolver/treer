//! Owned Unix subprocess groups for trusted, cooperating installers.
//!
//! This is lifecycle management, not executable authentication or a sandbox.
//! Descendants must retain their inherited process group and remain signalable
//! by the caller. Callers authenticate
//! the executable, set fixed argv/cwd and a clean environment, retain transaction
//! locks, and interpret output separately. No other waiter may reap this child
//! (including a process-wide SIGCHLD handler with waitpid(-1)).
use std::{
    io::{self, Read},
    os::unix::process::CommandExt,
    process::{Child, Command, Output, Stdio},
    time::Duration,
};

use rustix::{
    fs::{fcntl_getfl, fcntl_setfl, OFlags},
    process::{kill_process_group, waitid, Pid, Signal, WaitId, WaitIdOptions},
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;

fn quiescent(pid: Pid) -> io::Result<bool> {
    #[cfg(target_os = "macos")]
    return macos::quiescent(pid);
    #[cfg(target_os = "linux")]
    return linux::quiescent(pid);
}

/// Output whose owned group was observed with no executing members before the
/// leader was reaped. No authentication, receipt or rollback authority is
/// implied. The cooperating-child/no-external-reaper contract still applies.
/// Deliberately no Debug/Serialize: stdout can contain private installer data.
pub struct QuiescentOutput(Output);

impl QuiescentOutput {
    pub fn status(&self) -> std::process::ExitStatus {
        self.0.status
    }

    pub fn stdout(&self) -> &[u8] {
        &self.0.stdout
    }
}

struct Group {
    child: Option<Child>,
    pid: Pid,
}

impl Group {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        let mut child = command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        // Child::id is an OS child PID. A PID <= 1 could broaden kill(-pid)
        // semantics, so never signal it, even if a future platform is unusual.
        let Some(pid) =
            Pid::from_raw(child.id() as i32).filter(|pid| pid.as_raw_nonzero().get() > 1)
        else {
            // Never broaden this defensive error into group signalling.
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("invalid child identity"));
        };
        let group = Self {
            child: Some(child),
            pid,
        };
        let stdout = group.child.as_ref().unwrap().stdout.as_ref().unwrap();
        fcntl_setfl(stdout, fcntl_getfl(stdout)? | OFlags::NONBLOCK)?;
        Ok(group)
    }

    fn exited(&self) -> io::Result<bool> {
        // NOWAIT leaves the exited leader unreaped. Keeping its PID reserved
        // until AFTER group signalling prevents targeting a recycled PGID.
        match waitid(
            WaitId::Pid(self.pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(status) => Ok(status.is_some()),
            Err(rustix::io::Errno::INTR) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn stop(&mut self) -> io::Result<std::process::ExitStatus> {
        // Verify exclusive wait ownership before signalling, even on Drop. An
        // unexpected external reaper must never turn cleanup into kill by a
        // stale number. On an ownership error leave evidence to the caller.
        let exited = self.exited()?;
        match kill_process_group(self.pid, Signal::KILL) {
            Ok(()) => {}
            Err(rustix::io::Errno::SRCH) if exited => {}
            Err(rustix::io::Errno::SRCH) => {
                // A live leader without its original group violated the
                // cooperating-child contract. Kill/reap the directly owned
                // child, but do not claim its escaped descendants were cleaned.
                let child = self.child.as_mut().unwrap();
                child.kill()?;
                let _ = self.child.take().unwrap().wait();
                return Err(io::Error::other("child process group changed"));
            }
            // Darwin's killpg1 filters zombies before counting eligible group
            // members, so a zombie-only group returns EPERM. Do NOT generally
            // ignore EPERM: independently require an exited owned leader and
            // a complete OS group inventory containing ONLY that leader.
            #[cfg(target_os = "macos")]
            Err(rustix::io::Errno::PERM) if exited && macos::only_owned_leader(self.pid) => {}
            Err(error) => {
                // Cleanup is unproven, so propagate failure. Still reap an
                // already exited owned leader rather than leaking a zombie;
                // never signal by this number after that reap.
                if exited {
                    let _ = self.child.take().unwrap().wait();
                }
                return Err(error.into());
            }
        }
        let mut child = self.child.take().unwrap();
        // No signal by this PID/PGID is possible after this reap, even if wait
        // fails. Standard Child owns no background or implicit global reaper.
        child.wait()
    }

    async fn finish_quiescent(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> io::Result<std::process::ExitStatus> {
        if !self.exited()? {
            return Err(io::Error::other("child group quiescence unavailable"));
        }
        // Keep the leader unreaped through signalling AND the OS inventory.
        // Permission errors are not success. Preserve the narrow Darwin
        // singleton exception; an ambiguous multi-member EPERM still fails,
        // even if some members might be zombies. The full state check remains
        // mandatory before returning an output witness.
        match kill_process_group(self.pid, Signal::KILL) {
            Ok(()) => {}
            #[cfg(target_os = "macos")]
            Err(rustix::io::Errno::PERM) if macos::only_owned_leader(self.pid) => {}
            Err(error) => return Err(error.into()),
        }
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "child group quiescence deadline",
                ));
            }
            let quiet = quiescent(self.pid)?;
            // OS inventory reads are synchronous. Never return a successful
            // witness after a slow inventory consumed the remaining budget.
            if tokio::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "child group quiescence deadline",
                ));
            }
            if quiet && self.exited()? {
                // No further signal uses this number, including on Drop.
                return self.child.take().unwrap().wait();
            }
            tokio::time::sleep_until(
                deadline.min(tokio::time::Instant::now() + Duration::from_millis(10)),
            )
            .await;
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.stop();
        }
    }
}

fn drain(child: &mut Child, bytes: &mut Vec<u8>, limit: usize) -> io::Result<()> {
    let mut buffer = [0; 8192];
    // Bound work per poll as well as total bytes: a continuous writer cannot
    // starve cancellation or the deadline. Never allocate from an untrusted
    // declared length, and retain at most limit bytes.
    for _ in 0..8 {
        match child.stdout.as_mut().unwrap().read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if count > limit.saturating_sub(bytes.len()) {
                    return Err(io::Error::other("child output limit exceeded"));
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Capture bounded stdout; stdin/stderr are always real null handles. On normal
/// exit, error, timeout or future cancellation, SIGKILL the dedicated process
/// group BEFORE reaping its leader. The timeout bounds execution/polling, not an
/// OS-stalled spawn or uninterruptible kernel wait during cleanup. Drop performs
/// synchronous kill/reap; callers must not treat cancellation as rollback proof.
/// This does not survive the supervising process dying or contain descendants
/// that deliberately leave their process group. Group signalling is not proof
/// that every descendant has completed kernel teardown; quiescence/recovery
/// evidence and transaction-lock lifetime remain the caller's responsibility.
pub async fn output(command: &mut Command, timeout: Duration, limit: usize) -> io::Result<Output> {
    capture(command, timeout, limit, false).await
}

/// Strict capture for future transaction callers. The SAME timeout includes
/// post-signal quiescence observation; it is never extended. Only a successful
/// result carries QuiescentOutput. On error/cancellation the old best-effort
/// Drop cleanup still runs, but there is NO quiescence proof: the caller must
/// retain transaction ownership for recovery, not release locks or roll back.
/// This observes stopped user execution, not full kernel teardown: zombies are
/// inert and may await their own parent/init reaper. It cannot contain deliberate
/// group escape or another same-account process joining the group later. OS
/// inventory reads are synchronous and may stall in the kernel; exceeding the
/// budget returns an error, never a late successful witness.
pub async fn output_quiescent(
    command: &mut Command,
    timeout: Duration,
    limit: usize,
) -> io::Result<QuiescentOutput> {
    capture(command, timeout, limit, true)
        .await
        .map(QuiescentOutput)
}

async fn capture(
    command: &mut Command,
    timeout: Duration,
    limit: usize,
    require_quiescent: bool,
) -> io::Result<Output> {
    if timeout.is_zero() || limit == 0 {
        return Err(io::Error::other("invalid child budget"));
    }
    let deadline = tokio::time::Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::other("invalid child budget"))?;
    let mut group = Group::spawn(command)?;
    let mut bytes = Vec::new();
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "child deadline"));
        }
        drain(group.child.as_mut().unwrap(), &mut bytes, limit)?;
        if group.exited()? {
            // Retain the pipe independently while stop consumes/reaps Child.
            let mut stdout = group.child.as_mut().unwrap().stdout.take().unwrap();
            let status = if require_quiescent {
                group.finish_quiescent(deadline).await?
            } else {
                group.stop()?
            };
            // The exited leader cannot produce more bytes. Read its remaining
            // buffered bytes, without waiting for descendant-held pipe handles.
            let mut buffer = [0; 8192];
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        if count > limit.saturating_sub(bytes.len()) {
                            return Err(io::Error::other("child output limit exceeded"));
                        }
                        bytes.extend_from_slice(&buffer[..count]);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
            return Ok(Output {
                status,
                stdout: bytes,
                stderr: Vec::new(),
            });
        }
        tokio::time::sleep_until(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(10)),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "treer-child-group-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn command(&self, exit_parent: bool) -> Command {
            // Only isolated fixture processes. The descendant retains the
            // inherited group and stdout, and would mutate after cleanup if
            // only the leader were killed/reaped. No production shell usage.
            let mut command = Command::new("/bin/sh");
            command.env_clear().arg("-c").arg(if exit_parent {
                "/bin/sh -c '/bin/sleep 0.4; printf leaked > \"$1/late\"' fixture \"$1\" & printf ready > \"$1/ready\"; printf result; exit 7"
            } else {
                "/bin/sh -c '/bin/sleep 0.4; printf leaked > \"$1/late\"' fixture \"$1\" & printf ready > \"$1/ready\"; wait"
            }).arg("fixture").arg(&self.0);
            command
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    async fn ready(path: &Path) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !path.join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fixture did not become ready");
    }

    async fn no_late_write(fixture: &Fixture) {
        tokio::time::sleep(Duration::from_millis(550)).await;
        assert!(!fixture.0.join("late").exists());
    }

    #[tokio::test]
    async fn normal_exit_preserves_status_output_and_cleans_descendants() {
        let fixture = Fixture::new();
        let result = output(&mut fixture.command(true), Duration::from_secs(3), 64)
            .await
            .unwrap();
        assert_eq!(result.status.code(), Some(7));
        assert_eq!(result.stdout, b"result");
        assert!(result.stderr.is_empty());
        assert!(fixture.0.join("ready").exists());
        no_late_write(&fixture).await;
    }

    #[tokio::test]
    async fn deadline_cleans_the_child_group() {
        let fixture = Fixture::new();
        let error = output(&mut fixture.command(false), Duration::from_millis(150), 64)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(fixture.0.join("ready").exists());
        no_late_write(&fixture).await;
    }

    #[tokio::test]
    async fn cancelling_the_future_cleans_the_child_group() {
        let fixture = Fixture::new();
        let mut command = fixture.command(false);
        let task =
            tokio::spawn(async move { output(&mut command, Duration::from_secs(3), 64).await });
        ready(&fixture.0).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        no_late_write(&fixture).await;
    }

    #[tokio::test]
    async fn excess_stdout_is_rejected_and_never_returned() {
        let mut command = Command::new("/bin/sh");
        command
            .env_clear()
            .args(["-c", "while :; do printf 0123456789; done"]);
        let error = output(&mut command, Duration::from_secs(3), 64)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "child output limit exceeded");
    }

    #[tokio::test]
    async fn null_input_and_stderr_do_not_wait_for_a_reader() {
        let mut command = Command::new("/bin/sh");
        command.env_clear().args([
            "-c",
            "if read line; then exit 9; fi; printf private >&2; printf public",
        ]);
        let result = output(&mut command, Duration::from_secs(3), 64)
            .await
            .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"public");
        assert!(result.stderr.is_empty());
    }

    #[tokio::test]
    async fn leader_stays_reserved_until_group_cleanup_then_is_reaped() {
        let mut command = Command::new("/bin/sh");
        command.env_clear().args(["-c", "exit 0"]);
        let mut group = Group::spawn(&mut command).unwrap();
        assert_eq!(
            rustix::process::getpgid(Some(group.pid)).unwrap(),
            group.pid
        );
        assert_ne!(rustix::process::getpgid(None).unwrap(), group.pid);
        tokio::time::timeout(Duration::from_secs(3), async {
            while !group.exited().unwrap() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(group.exited().unwrap()); // WNOWAIT did not consume status.
        #[cfg(target_os = "macos")]
        assert!(macos::only_owned_leader(group.pid));
        assert!(group.stop().unwrap().success());
        assert!(group.child.is_none());
        assert_eq!(
            waitid(WaitId::Pid(group.pid), WaitIdOptions::EXITED)
                .err()
                .unwrap(),
            rustix::io::Errno::CHILD
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn darwin_singleton_exception_rejects_a_group_with_descendants() {
        let fixture = Fixture::new();
        let mut group = Group::spawn(&mut fixture.command(false)).unwrap();
        ready(&fixture.0).await;
        assert!(!macos::only_owned_leader(group.pid));
        assert!(!group.stop().unwrap().success());
        no_late_write(&fixture).await;
    }

    #[tokio::test]
    async fn invalid_budgets_do_not_spawn() {
        let fixture = Fixture::new();
        for (timeout, limit) in [
            (Duration::ZERO, 1),
            (Duration::from_secs(1), 0),
            (Duration::MAX, 1),
        ] {
            assert!(output(&mut fixture.command(true), timeout, limit)
                .await
                .is_err());
            assert!(!fixture.0.join("ready").exists());
        }
    }

    #[tokio::test]
    async fn strict_result_requires_observed_quiescence_before_reaping() {
        let fixture = Fixture::new();
        let result = output_quiescent(&mut fixture.command(true), Duration::from_secs(3), 64)
            .await
            .unwrap();
        assert_eq!(result.status().code(), Some(7));
        assert_eq!(result.stdout(), b"result");
        no_late_write(&fixture).await;
    }

    #[tokio::test]
    async fn live_members_are_not_quiescent_and_killed_members_are() {
        let fixture = Fixture::new();
        let mut group = Group::spawn(&mut fixture.command(false)).unwrap();
        ready(&fixture.0).await;
        assert!(!quiescent(group.pid).unwrap());
        kill_process_group(group.pid, Signal::KILL).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !group.exited().unwrap() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !quiescent(group.pid).unwrap() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        // Test the state observation after the test's own signal, without
        // sending another kill to an already zombie-only Darwin group (that
        // case intentionally retains the conservative EPERM failure rule).
        let status = group.child.take().unwrap().wait().unwrap();
        assert!(!status.success());
        assert!(group.child.is_none());
        no_late_write(&fixture).await;
    }

    #[tokio::test]
    async fn expired_quiescence_budget_keeps_the_unreaped_owner_and_grants_no_witness() {
        let mut command = Command::new("/bin/sh");
        command.env_clear().args(["-c", "exit 0"]);
        let mut group = Group::spawn(&mut command).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !group.exited().unwrap() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let error = group
            .finish_quiescent(tokio::time::Instant::now())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(group.child.is_some());
        assert!(group.exited().unwrap());
        assert!(group.stop().unwrap().success());
    }

    #[tokio::test]
    async fn strict_deadline_or_cancellation_grants_no_result() {
        let fixture = Fixture::new();
        assert!(
            output_quiescent(&mut fixture.command(false), Duration::from_millis(150), 64)
                .await
                .is_err()
        );
        no_late_write(&fixture).await;

        let fixture = Fixture::new();
        let mut command = fixture.command(false);
        let task = tokio::spawn(async move {
            output_quiescent(&mut command, Duration::from_secs(3), 64).await
        });
        ready(&fixture.0).await;
        task.abort();
        assert!(task.await.err().unwrap().is_cancelled());
        no_late_write(&fixture).await;
    }
}
