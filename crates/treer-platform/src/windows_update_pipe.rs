//! One private, local pipe instance; peer checking borrows an original Child.
//! Not a reconnecting listener, worker protocol, lifecycle or readiness proof.
use std::{
    io,
    os::windows::io::AsRawHandle,
    path::{Path, PathBuf},
    process::Child,
};
use tokio::net::windows::named_pipe::NamedPipeServer;
use windows_sys::Win32::{
    Foundation::WAIT_TIMEOUT,
    System::{
        Pipes::GetNamedPipeClientProcessId,
        Threading::{GetProcessId, WaitForSingleObject},
    },
};

pub struct CreatedChildPipe {
    server: NamedPipeServer,
    path: PathBuf,
}

fn refused() -> io::Error {
    io::Error::other("windows_update_pipe_unconfirmed")
}

impl CreatedChildPipe {
    /// A fresh caller-generated UUID's simple lowercase hex spelling. The
    /// namespace is fixed; no arbitrary path, ambient socket or repair/adoption.
    /// Existing name refuses via FIRST_PIPE_INSTANCE. The existing platform
    /// constructor supplies current-user+SYSTEM DACL and rejects remote clients.
    pub fn bind(selector: &str) -> io::Result<Self> {
        if selector.len() != 32
            || !selector
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(refused());
        }
        let path = PathBuf::from(format!(r"\\.\pipe\treer-update-child-{selector}"));
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
