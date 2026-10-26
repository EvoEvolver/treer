//! One private, local pipe instance; peer checking borrows an original Child.
//! Not a reconnecting listener, worker protocol, lifecycle or readiness proof.
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    pin::Pin,
    process::Child,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer};
use windows_sys::Win32::{
    Foundation::{DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT},
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

/// Child-side stream plus retained original creator handle. Neither a PID nor
/// later changes to stdin can replace this query/synchronize-only custody.
/// Creator origin/exit observation does not grant selected-runtime authority.
pub struct CreationParentConnection {
    stream: NamedPipeClient,
    creator: OwnedHandle,
}

fn retain_creator(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(refused());
    }
    let mut duplicate = std::ptr::null_mut();
    let result = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            handle,
            GetCurrentProcess(),
            &mut duplicate,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            0,
        )
    };
    if result == 0 || duplicate.is_null() || duplicate == INVALID_HANDLE_VALUE {
        return Err(refused());
    }
    let owned = unsafe { OwnedHandle::from_raw_handle(duplicate) };
    // Reject non-process handles and unknown query outcomes before admission.
    if unsafe { GetProcessId(owned.as_raw_handle()) } == 0 {
        return Err(refused());
    }
    creator_live(&owned)?;
    Ok(owned)
}

fn creator_live(creator: &OwnedHandle) -> io::Result<bool> {
    match unsafe { WaitForSingleObject(creator.as_raw_handle(), 0) } {
        WAIT_TIMEOUT => Ok(true),
        WAIT_OBJECT_0 => Ok(false),
        _ => Err(refused()),
    }
}

async fn wait_creator_exit(creator: OwnedHandle) -> io::Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(refused());
        }
        let live = creator_live(&creator)?;
        // Do not accept an observation arriving after the finite budget.
        if tokio::time::Instant::now() >= deadline {
            return Err(refused());
        }
        if !live {
            return Ok(());
        }
        tokio::time::sleep_until(std::cmp::min(
            deadline,
            tokio::time::Instant::now() + std::time::Duration::from_millis(10),
        ))
        .await;
    }
}

impl CreationParentConnection {
    pub fn creator_is_live(&self) -> io::Result<bool> {
        creator_live(&self.creator)
    }

    /// Consume the transport, closing it before a bounded read-only wait on the
    /// original creator. Success is exit observation, never readiness, authority
    /// or permission to resume a transaction. Cancellation drops the handle;
    /// this object cannot reconnect or initiate a second wait.
    pub async fn wait_for_creator_exit(self) -> io::Result<()> {
        let Self { stream, creator } = self;
        drop(stream);
        wait_creator_exit(creator).await
    }
}

impl AsyncRead for CreationParentConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for CreationParentConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
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
pub fn connect_creation_parent_pipe(selector: &str) -> io::Result<CreationParentConnection> {
    let path = endpoint(selector)?;
    // Borrow this process's standard handle; do not close or take ownership of
    // a std handle potentially also used by the Rust runtime.
    let parent = retain_creator(unsafe { GetStdHandle(STD_INPUT_HANDLE) })?;
    if !creator_live(&parent)? {
        return Err(refused());
    }
    let expected = unsafe { GetProcessId(parent.as_raw_handle()) };
    if expected == 0 {
        return Err(refused());
    }
    let client = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(path)
        .map_err(|_| refused())?;
    let mut actual = 0;
    if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle(), &mut actual) } == 0
        || actual != expected
        || !creator_live(&parent)?
    {
        return Err(refused());
    }
    Ok(CreationParentConnection {
        stream: client,
        creator: parent,
    })
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
        let role = std::env::var("TREER_TEST_PIPE_ROLE").unwrap();
        if role == "exit259" {
            use std::io::Read;
            std::io::stdin().read_exact(&mut [0]).unwrap();
            std::process::exit(259);
        }
        if role == "server" {
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
        if role == "custody" {
            assert!(client.creator_is_live().unwrap());
            // This helper alone replaces its stdin slot. The original std
            // handle is borrowed, never closed by the connection constructor.
            assert_ne!(
                unsafe {
                    windows_sys::Win32::System::Console::SetStdHandle(
                        STD_INPUT_HANDLE,
                        INVALID_HANDLE_VALUE,
                    )
                },
                0
            );
            assert!(connect_creation_parent_pipe(&selector).is_err());
            assert!(client.creator_is_live().unwrap());
        }
        client.write_all(&[0x52]).await.unwrap();
        let mut byte = [0];
        client.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte, [0x53]);
        if role == "custody" {
            assert!(client.creator_is_live().unwrap());
            assert_eq!(
                client
                    .wait_for_creator_exit()
                    .await
                    .unwrap_err()
                    .to_string(),
                "windows_update_pipe_unconfirmed"
            );
        }
    }

    #[tokio::test]
    async fn windows_update_creator_custody_stdin_rebinding_keeps_original_handle_and_stream() {
        let selector = selector();
        let pipe = CreatedChildPipe::bind(&selector).unwrap();
        let mut child = fixture(&selector, "custody", creator_process_stdio().unwrap(), None);
        let mut server = pipe.connect(&child.0).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let mut byte = [0];
            server.read_exact(&mut byte).await.unwrap();
            assert_eq!(byte, [0x52]);
            server.write_all(&[0x53]).await.unwrap();
            // The consuming wait closes transport before waiting on the live
            // original creator. EOF does not masquerade as creator exit.
            assert!(server.read_exact(&mut byte).await.is_err());
        })
        .await
        .unwrap();
        assert!(exited(&mut child.0).await.success());
    }

    #[tokio::test]
    async fn windows_update_creator_custody_original_handle_exit259_and_deadline_are_not_liveness()
    {
        use std::io::Write;
        assert!(retain_creator(INVALID_HANDLE_VALUE).is_err());
        let mut child = fixture(&selector(), "exit259", Stdio::piped(), None);
        let retained = retain_creator(child.0.as_raw_handle()).unwrap();
        assert!(creator_live(&retained).unwrap());
        // A live original handle reaches a finite refusal, never a successful
        // readiness/exit result. Only the task-owned original Child is reaped.
        assert_eq!(
            wait_creator_exit(retained).await.unwrap_err().to_string(),
            "windows_update_pipe_unconfirmed"
        );
        let retained = retain_creator(child.0.as_raw_handle()).unwrap();
        child.0.stdin.take().unwrap().write_all(b"x").unwrap();
        wait_creator_exit(retained).await.unwrap();
        assert_eq!(exited(&mut child.0).await.code(), Some(259));
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
