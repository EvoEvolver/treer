//! One private, local pipe instance; peer checking borrows an original Child.
//! Not a reconnecting listener, worker protocol, lifecycle or readiness proof.
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    process::Child,
};
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer};
use windows_sys::Win32::{
    Foundation::{DuplicateHandle, INVALID_HANDLE_VALUE, WAIT_TIMEOUT},
    System::{
        Console::{GetStdHandle, STD_INPUT_HANDLE},
        Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId},
        Threading::{
            GetCurrentProcess, GetProcessId, WaitForSingleObject,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        },
    },
};

pub struct CreatedChildPipe {
    server: NamedPipeServer,
    path: PathBuf,
}

fn refused() -> io::Error {
    io::Error::other("windows_update_pipe_unconfirmed")
}

fn endpoint(selector: &str) -> io::Result<PathBuf> {
    if selector.len() != 32
        || !selector
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(refused());
    }
    Ok(PathBuf::from(format!(
        r"\\.\pipe\treer-update-child-{selector}"
    )))
}

/// Explicit bootstrap-only stdin: a reduced-rights handle to THIS creator
/// process. No PID lookup, token/secret inheritance or termination rights.
/// Command owns the Stdio and arranges inheritance of this exact input handle;
/// the child must not interpret it as byte input or normal Controller stdin.
pub fn creator_process_stdio() -> io::Result<std::process::Stdio> {
    let mut handle = std::ptr::null_mut();
    let success = unsafe {
        let current = GetCurrentProcess();
        DuplicateHandle(
            current,
            current,
            current,
            &mut handle,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            0,
        )
    };
    if success == 0 || handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(refused());
    }
    let owned = unsafe { OwnedHandle::from_raw_handle(handle) };
    Ok(std::process::Stdio::from(owned))
}

/// Child-side transport check using its explicitly inherited creator-process
/// input handle, never a caller PID/JSON claim. It opens one fixed local pipe
/// once, with no busy/retry loop, and refuses any other server or exited parent.
/// This does NOT prove the creator is an authorized Controller/runtime or grant
/// update authority: that still requires the admitted one-shot worker protocol.
pub fn connect_creation_parent_pipe(selector: &str) -> io::Result<NamedPipeClient> {
    let path = endpoint(selector)?;
    // Borrow this process's standard handle; do not close or take ownership of
    // a std handle potentially also used by the Rust runtime.
    let parent = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if parent.is_null()
        || parent == INVALID_HANDLE_VALUE
        || unsafe { WaitForSingleObject(parent, 0) } != WAIT_TIMEOUT
    {
        return Err(refused());
    }
    let expected = unsafe { GetProcessId(parent) };
    if expected == 0 {
        return Err(refused());
    }
    let client = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(path)
        .map_err(|_| refused())?;
    let mut actual = 0;
    if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle(), &mut actual) } == 0
        || actual != expected
        || unsafe { WaitForSingleObject(parent, 0) } != WAIT_TIMEOUT
    {
        return Err(refused());
    }
    Ok(client)
}

impl CreatedChildPipe {
    /// A fresh caller-generated UUID's simple lowercase hex spelling. The
    /// namespace is fixed; no arbitrary path, ambient socket or repair/adoption.
    /// Existing name refuses via FIRST_PIPE_INSTANCE. The existing platform
    /// constructor supplies current-user+SYSTEM DACL and rejects remote clients.
    pub fn bind(selector: &str) -> io::Result<Self> {
        let path = endpoint(selector)?;
        let server = crate::windows::create_pipe(&path, true)?;
        Ok(Self { server, path })
    }

    /// Nonsecret endpoint only, not a bearer credential or peer identity.
    pub fn endpoint(&self) -> &Path {
        &self.path
    }

    /// Consumes the sole instance on every outcome, including cancellation.
    /// No second accept/disconnect/rebind loop. A live retained Child makes the
    /// OS PID comparison unambiguous without opening/adopting that PID.
    pub async fn connect(self, child: &Child) -> io::Result<NamedPipeServer> {
        if unsafe { WaitForSingleObject(child.as_raw_handle(), 0) } != WAIT_TIMEOUT {
            return Err(refused());
        }
        tokio::time::timeout(std::time::Duration::from_secs(3), self.server.connect())
            .await
            .map_err(|_| refused())?
            .map_err(|_| refused())?;
        let handle = child.as_raw_handle();
        let expected = unsafe { GetProcessId(handle) };
        let mut actual = 0;
        if expected == 0
            || unsafe { GetNamedPipeClientProcessId(self.server.as_raw_handle(), &mut actual) } == 0
            || actual != expected
            || unsafe { WaitForSingleObject(handle, 0) } != WAIT_TIMEOUT
        {
            return Err(refused());
        }
        Ok(self.server)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct OwnedFixture(Child);
    impl Drop for OwnedFixture {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn selector() -> String {
        // Wall-clock reads need not be distinct across parallel tests. Names
        // belong to this test process and are never reused within its lifetime.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        format!(
            "{:08x}{:024x}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }
    fn fixture(selector: &str, role: &str, input: Stdio, ready: Option<&Path>) -> OwnedFixture {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .env("TREER_TEST_PIPE_SELECTOR", selector)
            .env("TREER_TEST_PIPE_ROLE", role)
            .args([
                "--exact",
                "windows_update_pipe::tests::creation_parent_pipe_fixture_child",
                "--ignored",
                "--nocapture",
            ])
            .stdin(input)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(ready) = ready {
            command.env("TREER_TEST_PIPE_READY", ready);
        }
        OwnedFixture(command.spawn().unwrap())
    }
    async fn exited(child: &mut Child) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "owned fixture exit deadline"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    #[ignore = "owned private bootstrap helper; only invoked by focused parents"]
    async fn creation_parent_pipe_fixture_child() {
        let selector = std::env::var("TREER_TEST_PIPE_SELECTOR").unwrap();
        if std::env::var("TREER_TEST_PIPE_ROLE").unwrap() == "server" {
            // Fixed fixture-only codes distinguish the pre-ready boundaries;
            // never serialize raw OS errors, paths or child stderr.
            let Ok(path) = endpoint(&selector) else {
                std::process::exit(81);
            };
            let Ok(server) = crate::windows::create_pipe(&path, true) else {
                std::process::exit(82);
            };
            let Some(ready) = std::env::var_os("TREER_TEST_PIPE_READY") else {
                std::process::exit(83);
            };
            if std::fs::File::create_new(ready).is_err() {
                std::process::exit(84);
            }
            if server.connect().await.is_err() {
                std::process::exit(85);
            }
            // Parent owns this fixed fixture and explicitly reaps it. Holding
            // the connection does not give this sibling lifecycle authority.
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            return;
        }
        let Ok(mut client) = connect_creation_parent_pipe(&selector) else {
            std::process::exit(77);
        };
        client.write_all(&[0x52]).await.unwrap();
        let mut byte = [0];
        client.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte, [0x53]);
    }

    #[tokio::test]
    async fn windows_update_parent_pipe_original_creator_handle_matches_live_server() {
        let selector = selector();
        let pipe = CreatedChildPipe::bind(&selector).unwrap();
        let mut child = fixture(&selector, "client", creator_process_stdio().unwrap(), None);
        let mut server = pipe.connect(&child.0).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let mut byte = [0];
            server.read_exact(&mut byte).await.unwrap();
            assert_eq!(byte, [0x52]);
            server.write_all(&[0x53]).await.unwrap();
        })
        .await
        .unwrap();
        assert!(exited(&mut child.0).await.success());
    }

    #[tokio::test]
    async fn windows_update_parent_pipe_foreign_server_and_invalid_input_are_refused() {
        let selector = selector();
        let root = std::env::temp_dir().join(format!("treer-parent-pipe-{selector}"));
        std::fs::create_dir(&root).unwrap();
        crate::windows::secure_path(&root).unwrap();
        let ready = root.join("ready");
        let mut sibling = fixture(&selector, "server", Stdio::null(), Some(&ready));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !ready.exists() {
            if let Some(status) = sibling.0.try_wait().unwrap() {
                panic!(
                    "owned sibling failed before ready; fixture exit={:?}",
                    status.code()
                );
            }
            assert!(
                std::time::Instant::now() < deadline,
                "owned sibling ready deadline"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // The client inherits OUR live process handle, while the actual pipe
        // server is our sibling Child. No claimed PID can make them match.
        let mut client = fixture(&selector, "client", creator_process_stdio().unwrap(), None);
        assert_eq!(exited(&mut client.0).await.code(), Some(77));
        drop(client);
        drop(sibling);
        std::fs::remove_file(ready).unwrap();
        std::fs::remove_dir(root).unwrap();

        let selector = self::selector();
        let _server = CreatedChildPipe::bind(&selector).unwrap();
        let mut invalid = fixture(&selector, "client", Stdio::null(), None);
        assert_eq!(exited(&mut invalid.0).await.code(), Some(77));
    }
}
