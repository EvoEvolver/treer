//! Small platform adapters for machine-local transport and user state.
use std::{
    env, io,
    path::{Path, PathBuf},
};
use tokio::io::{AsyncRead, AsyncWrite};

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[allow(unsafe_code)]
pub mod birth_lock;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod bounded_child;
mod environment;
#[cfg(unix)]
pub mod installation_directory;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod local_filesystem;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod lock_transfer;
pub mod machine_file;
#[cfg(unix)]
pub mod private_directory;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod process_image;
#[cfg(unix)]
pub mod retained_executable;
pub use environment::{
    clean_process_environment_before_threads, inherited_environment, inherited_variable_allowed,
    machine_exec_environment, machine_exec_variable_allowed,
};

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
#[cfg(windows)]
pub use windows::{
    current_user_sid, process_created_at, terminate_process, LocalLock, ProcessTreeGuard,
};

pub trait LocalIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> LocalIo for T {}
pub type LocalStream = Box<dyn LocalIo>;

pub struct LocalListener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    inner: tokio::net::windows::named_pipe::NamedPipeServer,
    #[cfg(windows)]
    path: PathBuf,
}

impl LocalListener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        return Ok(Self {
            inner: tokio::net::UnixListener::bind(path)?,
        });
        #[cfg(windows)]
        return Ok(Self {
            inner: windows::create_pipe(path, true)?,
            path: path.into(),
        });
    }

    pub async fn accept(&mut self) -> io::Result<LocalStream> {
        #[cfg(unix)]
        return Ok(Box::new(self.inner.accept().await?.0));
        #[cfg(windows)]
        {
            loop {
                match self.inner.connect().await {
                    Ok(()) => break,
                    // Liveness probes can disconnect before accept.
                    Err(error) if error.raw_os_error() == Some(232) => {
                        self.inner = windows::create_pipe(&self.path, false)?;
                    }
                    Err(error) => return Err(error),
                }
            }
            let next = windows::create_pipe(&self.path, false)?;
            Ok(Box::new(std::mem::replace(&mut self.inner, next)))
        }
    }
}

pub async fn connect_local(path: &Path) -> io::Result<LocalStream> {
    #[cfg(unix)]
    return Ok(Box::new(tokio::net::UnixStream::connect(path).await?));
    #[cfg(windows)]
    {
        // ERROR_PIPE_BUSY is transient while a server prepares the next instance.
        for _ in 0..100 {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(path) {
                Ok(stream) => return Ok(Box::new(stream)),
                Err(error) if error.raw_os_error() == Some(231) => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await
                }
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "local Host pipe remained busy",
        ))
    }
}

pub fn local_endpoint_active(path: &Path) -> bool {
    #[cfg(unix)]
    return std::os::unix::net::UnixStream::connect(path).is_ok();
    #[cfg(windows)]
    {
        // Open is synchronous and does not require a Tokio runtime. Busy means
        // the endpoint exists; denial must also prevent replacing its owner.
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
        {
            Ok(_) => true,
            Err(error) => matches!(error.raw_os_error(), Some(231) | Some(5)),
        }
    }
}

pub fn config_directory() -> Option<PathBuf> {
    #[cfg(windows)]
    return env::var_os("LOCALAPPDATA").map(|root| PathBuf::from(root).join("Treer/agent-servers"));
    #[cfg(not(windows))]
    {
        let root = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|root| PathBuf::from(root).join(".config")))?;
        Some(root.join("treer/agent-servers"))
    }
}

pub fn secure_directory(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    windows::secure_path(path)?;
    Ok(())
}

pub fn secure_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    windows::secure_path(path)?;
    Ok(())
}

pub fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    return windows::replace_file(source, destination);
    #[cfg(not(windows))]
    std::fs::rename(source, destination)
}

pub fn endpoint_name(machine: &str, user: &str) -> PathBuf {
    fn hash(value: &str) -> u64 {
        value.bytes().fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        })
    }
    PathBuf::from(format!(
        r"\\.\pipe\treer-{:016x}-{:016x}",
        hash(user),
        hash(machine)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temporary() -> PathBuf {
        let root = env::temp_dir().join(format!(
            "treer-platform-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        secure_directory(&root).unwrap();
        root
    }

    #[test]
    fn endpoint_identity_separates_users_and_machines() {
        assert_eq!(
            endpoint_name("machine", "user"),
            endpoint_name("machine", "user")
        );
        assert_ne!(
            endpoint_name("machine", "user"),
            endpoint_name("machine2", "user")
        );
        assert_ne!(
            endpoint_name("machine", "user"),
            endpoint_name("machine", "user2")
        );
    }

    #[test]
    fn replaces_existing_private_state_without_a_delete_gap() {
        let root = temporary();
        let state = root.join("state.json");
        let next = root.join("next.json");
        std::fs::write(&state, "old").unwrap();
        std::fs::write(&next, "new").unwrap();
        secure_file(&next).unwrap();
        replace_file(&next, &state).unwrap();
        assert_eq!(std::fs::read_to_string(&state).unwrap(), "new");
        assert!(!next.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn local_transport_reconnects_and_rejects_duplicate_owner() {
        let root = temporary();
        #[cfg(unix)]
        let endpoint = root.join("host.sock");
        #[cfg(windows)]
        let endpoint = endpoint_name(&root.to_string_lossy(), &current_user_sid().unwrap());
        let mut listener = LocalListener::bind(&endpoint).unwrap();
        assert!(LocalListener::bind(&endpoint).is_err());
        assert!(local_endpoint_active(&endpoint));
        let server = tokio::spawn(async move {
            let mut replies = 0;
            while replies < 2 {
                let mut stream = listener.accept().await.unwrap();
                let mut bytes = [0; 7];
                match stream.read_exact(&mut bytes).await {
                    Ok(_) => {
                        assert_eq!(&bytes, b"request");
                        stream.write_all(b"reply").await.unwrap();
                        replies += 1;
                    }
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {}
                    Err(error) => panic!("unexpected connection error: {error}"),
                }
            }
        });
        for _ in 0..2 {
            let mut client = connect_local(&endpoint).await.unwrap();
            client.write_all(b"request").await.unwrap();
            let mut bytes = [0; 5];
            client.read_exact(&mut bytes).await.unwrap();
            assert_eq!(&bytes, b"reply");
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_process_identity_refuses_pid_reuse() {
        let mut child = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"])
            .spawn()
            .unwrap();
        let created = process_created_at(child.id()).unwrap().unwrap();
        assert!(terminate_process(child.id(), created + 1).is_err());
        assert_eq!(process_created_at(child.id()).unwrap(), Some(created));
        terminate_process(child.id(), created).unwrap();
        child.wait().unwrap();
        assert_eq!(process_created_at(child.id()).unwrap(), None);
    }
}
