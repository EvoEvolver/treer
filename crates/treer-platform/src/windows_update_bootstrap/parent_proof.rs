//! Inert child-side kernel identity only. Not a mutual session or permit.
use super::*;
use tokio::net::windows::named_pipe::NamedPipeClient;
use windows_sys::Win32::System::{Diagnostics::ToolHelp::*, Pipes::GetNamedPipeServerProcessId};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct OwnerIdentity {
    pid: u32,
    created: u64,
    volume: u32,
    file: u64,
    digest: [u8; 32],
}

fn image_for_process(handle: HANDLE) -> io::Result<File> {
    let mut buffer = vec![0; 32_768];
    let mut length = buffer.len() as u32;
    check(unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) })?;
    if length == 0 || length as usize >= buffer.len() {
        return Err(denied());
    }
    open_pinned(
        &PathBuf::from(OsString::from_wide(&buffer[..length as usize])),
        false,
    )
}

impl OwnerIdentity {
    fn observe(handle: HANDLE, pid: u32) -> io::Result<(Self, File)> {
        if unsafe { WaitForSingleObject(handle, 0) } != WAIT_TIMEOUT {
            return Err(denied());
        }
        let image = image_for_process(handle)?;
        let (volume, file, _, _) = identity(&image)?;
        let observed = Self {
            pid,
            created: creation(handle)?,
            volume,
            file,
            digest: digest(&image)?,
        };
        if unsafe { WaitForSingleObject(handle, 0) } != WAIT_TIMEOUT {
            return Err(denied());
        }
        Ok((observed, image))
    }

    pub(super) fn current() -> io::Result<Self> {
        Self::observe(unsafe { GetCurrentProcess() }, unsafe {
            GetCurrentProcessId()
        })
        .map(|(identity, _)| identity)
    }

    // Fixed-size fixture transport only. The production registration and mutual
    // challenge protocol are intentionally absent from this sub-increment.
    pub(super) fn encode(self) -> [u8; 56] {
        let mut bytes = [0; 56];
        bytes[..4].copy_from_slice(&self.pid.to_le_bytes());
        bytes[4..12].copy_from_slice(&self.created.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.volume.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.file.to_le_bytes());
        bytes[24..].copy_from_slice(&self.digest);
        bytes
    }
}

fn os_parent_pid() -> io::Result<u32> {
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE || raw.is_null() {
        return Err(denied());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry: PROCESSENTRY32W = unsafe { mem::zeroed() };
    entry.dwSize = mem::size_of_val(&entry) as u32;
    check(unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) })?;
    let own = unsafe { GetCurrentProcessId() };
    for _ in 0..65_536 {
        if entry.th32ProcessID == own {
            return if entry.th32ParentProcessID != 0 && entry.th32ParentProcessID != own {
                Ok(entry.th32ParentProcessID)
            } else {
                Err(denied())
            };
        }
        if unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) } == 0 {
            return Err(denied());
        }
    }
    Err(denied())
}

/// Retains the one opened OS parent and its pinned image for the entire proof
/// lifetime. Never opens a PID supplied by a frame and never reopens on failure.
pub(super) struct ParentProof {
    process: OwnedHandle,
    image: File,
    identity: OwnerIdentity,
}

impl ParentProof {
    pub(super) fn attest(pipe: &NamedPipeClient) -> io::Result<Self> {
        let mut server = 0;
        check(unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut server) })?;
        let parent = os_parent_pid()?;
        if server != parent {
            return Err(denied());
        }
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                parent,
            )
        };
        if raw.is_null() {
            return Err(denied());
        }
        let process = unsafe { OwnedHandle::from_raw_handle(raw) };
        let (identity, image) = OwnerIdentity::observe(raw, parent)?;
        // Parent-PID reuse after the actual parent exits cannot pass: any new
        // process with that PID was created after this child, not before it.
        if identity.created >= creation(unsafe { GetCurrentProcess() })? {
            return Err(denied());
        }
        Ok(Self {
            process,
            image,
            identity,
        })
    }

    pub(super) fn verify_registration(&self, registration: &[u8; 56]) -> io::Result<()> {
        if *registration != self.identity.encode() {
            return Err(denied());
        }
        let (fresh, _image) =
            OwnerIdentity::observe(self.process.as_raw_handle(), self.identity.pid)?;
        let (volume, file, _, _) = identity(&self.image)?;
        if fresh != self.identity || volume != fresh.volume || file != fresh.file {
            return Err(denied());
        }
        Ok(())
    }
}
