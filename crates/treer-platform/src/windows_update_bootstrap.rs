//! Windows owner-side launch/connection primitives, not update authority.
//! No Controller caller or worker entry is wired yet. A future caller must be
//! the Controller daemon in the stable Host Job, never a CLI/Agent process.
//! This module does not transfer permits/locks, write journals or run installers.
use std::{
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    io::{self, Read},
    mem,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
    ptr,
    time::Duration,
};

use sha2::{Digest, Sha256};
use tokio::net::windows::named_pipe::NamedPipeServer;
use windows_sys::Win32::{
    Foundation::*,
    Security::{
        Authorization::*,
        Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG},
        *,
    },
    Storage::FileSystem::*,
    System::{
        JobObjects::*,
        Pipes::GetNamedPipeClientProcessId,
        SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE, MAXIMUM_ALLOWED},
        Threading::*,
        WindowsProgramming::DRIVE_FIXED,
    },
};

const IMAGE_LIMIT: u64 = 128 * 1024 * 1024;
const CONNECT_BUDGET: Duration = Duration::from_secs(5);
const CLEANUP_MS: u32 = 5_000;

mod parent_proof;

fn denied() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "update_bootstrap_refused")
}

fn random_nonce() -> io::Result<[u8; 16]> {
    let mut bytes = [0u8; 16];
    if unsafe {
        BCryptGenRandom(
            ptr::null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    } < 0
        || bytes == [0; 16]
    {
        return Err(denied());
    }
    Ok(bytes)
}

// Blocking native operations retain ownership in this task: unlike detaching a
// spawn_blocking launch, cancellation cannot leave a worker spawning a child
// after the caller has gone away. Tokio hands other runnable tasks/timers to a
// replacement worker. Single-thread runtimes are refused, never panicked.
fn blocking_phase<T>(work: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| denied())?;
    if runtime.runtime_flavor() != tokio::runtime::RuntimeFlavor::MultiThread {
        return Err(denied());
    }
    tokio::task::block_in_place(work)
}

// All errors are fixed internal codes, never paths, argv or OS exception text.
fn check(value: i32) -> io::Result<()> {
    if value == 0 {
        Err(denied())
    } else {
        Ok(())
    }
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn identity(file: &File) -> io::Result<(u32, u64, u32, u32)> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { mem::zeroed() };
    check(unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) })?;
    Ok((
        info.dwVolumeSerialNumber,
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        info.nNumberOfLinks,
        info.dwFileAttributes,
    ))
}

fn open_pinned(path: &Path, directory: bool) -> io::Result<File> {
    open_checked(
        path,
        directory,
        if directory {
            FILE_SHARE_READ | FILE_SHARE_WRITE
        } else {
            FILE_SHARE_READ
        },
    )
}

fn open_checked(path: &Path, directory: bool, sharing: u32) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(sharing)
        .custom_flags(
            FILE_FLAG_OPEN_REPARSE_POINT
                | if directory {
                    FILE_FLAG_BACKUP_SEMANTICS
                } else {
                    0
                },
        )
        .open(path)
        .map_err(|_| denied())?;
    let (_, _, links, flags) = identity(&file)?;
    if flags & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (flags & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
        || (!directory && links != 1)
    {
        return Err(denied());
    }
    Ok(file)
}

struct SecurityAllocation(*mut std::ffi::c_void);
impl Drop for SecurityAllocation {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

fn sid_text(sid: PSID) -> io::Result<String> {
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(denied());
    }
    let mut text = ptr::null_mut();
    check(unsafe { ConvertSidToStringSidW(sid, &mut text) })?;
    let _allocation = SecurityAllocation(text.cast());
    // A SID has at most 15 subauthorities; its textual representation is bounded.
    let length = (0..184)
        .find(|&index| unsafe { *text.add(index) } == 0)
        .ok_or_else(denied)?;
    String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) }).map_err(|_| denied())
}

fn private_owner_dacl(file: &File, owner_sid: &str) -> io::Result<()> {
    let mut owner = ptr::null_mut();
    let mut acl = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    let result = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut acl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    let _allocation = SecurityAllocation(descriptor);
    if result != ERROR_SUCCESS
        || sid_text(owner)? != owner_sid
        || acl.is_null()
        || unsafe { IsValidAcl(acl) } == 0
    {
        return Err(denied());
    }
    let mut info: ACL_SIZE_INFORMATION = unsafe { mem::zeroed() };
    check(unsafe {
        GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            mem::size_of_val(&info) as u32,
            AclSizeInformation,
        )
    })?;
    let mutation = GENERIC_ALL
        | GENERIC_WRITE
        | MAXIMUM_ALLOWED
        | WRITE_DAC
        | WRITE_OWNER
        | DELETE
        | FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_WRITE_EA
        | FILE_WRITE_ATTRIBUTES
        | FILE_DELETE_CHILD;
    for index in 0..info.AceCount {
        let mut raw = ptr::null_mut();
        check(unsafe { GetAce(acl, index, &mut raw) })?;
        let header = unsafe { ptr::read_unaligned(raw.cast::<ACE_HEADER>()) };
        // Only ordinary allow/deny ACEs are understood. Conditional, object and
        // callback ACEs are refused rather than approximating effective access.
        if !matches!(
            u32::from(header.AceType),
            ACCESS_ALLOWED_ACE_TYPE | ACCESS_DENIED_ACE_TYPE
        ) || usize::from(header.AceSize) < mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart) + 8
        {
            return Err(denied());
        }
        let ace = unsafe { ptr::read_unaligned(raw.cast::<ACCESS_ALLOWED_ACE>()) };
        let sid = unsafe {
            raw.cast::<u8>()
                .add(mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart))
        };
        let sid_size = 8 + 4 * usize::from(unsafe { *sid.add(1) });
        if sid_size + mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart) > usize::from(header.AceSize) {
            return Err(denied());
        }
        let trustee = sid_text(sid.cast())?;
        if u32::from(header.AceType) == ACCESS_ALLOWED_ACE_TYPE
            && ace.Mask & mutation != 0
            && trustee != owner_sid
            && trustee != "S-1-5-18"
        {
            // Reject even when another ACE denies the grant, and include
            // inherit-only grants: no future child may inherit foreign writers.
            return Err(denied());
        }
    }
    Ok(())
}

fn final_path(file: &File) -> io::Result<PathBuf> {
    let mut buffer = vec![0; 32_768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    };
    if length == 0 || length as usize >= buffer.len() {
        return Err(denied());
    }
    Ok(PathBuf::from(OsString::from_wide(
        &buffer[..length as usize],
    )))
}

fn digest(mut file: &File) -> io::Result<[u8; 32]> {
    // Reopened read-only handles get independent offsets; the pinned source is
    // never cloned/seeking concurrently. No writable/delete-sharing image handle.
    let size = file.metadata().map_err(|_| denied())?.len();
    if size == 0 || size > IMAGE_LIMIT {
        return Err(denied());
    }
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|_| denied())?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > IMAGE_LIMIT {
            return Err(denied());
        }
        hash.update(&buffer[..count]);
    }
    if total != size {
        return Err(denied());
    }
    Ok(hash.finalize().into())
}

/// Read-only pinned old-Controller copy in a validated private job directory.
/// Digest authenticity and installation/enrollment selection remain the future
/// coordinator's job.
/// Neither a path nor an expected digest alone is an execution permit.
pub struct PinnedImage {
    path: PathBuf,
    file: File,
    _ancestors: Vec<File>,
    id: (u32, u64, u32, u32),
    digest: [u8; 32],
}

impl PinnedImage {
    pub async fn open(path: &Path, expected_digest: [u8; 32]) -> io::Result<Self> {
        blocking_phase(|| Self::open_blocking(path, expected_digest))
    }

    fn open_blocking(path: &Path, expected_digest: [u8; 32]) -> io::Result<Self> {
        // Only a local absolute disk path; no UNC, device namespace, aliases,
        // traversal, embedded NUL or alternate data streams.
        let components: Vec<_> = path.components().collect();
        if !matches!(components.first(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_)))
            || components.get(1) != Some(&Component::RootDir)
            || components.len() < 3
            || components[2..].iter().any(|c| !matches!(c, Component::Normal(name) if !name.encode_wide().any(|n| matches!(n, 0 | 58))))
        { return Err(denied()); }
        let mut ancestors = Vec::new();
        let mut current = PathBuf::new();
        for (index, component) in components.iter().enumerate() {
            current.push(component.as_os_str());
            if index >= 1 && index < components.len() - 1 {
                ancestors.push(open_pinned(&current, true)?);
            }
        }
        let file = open_pinned(path, false)?;
        let owner_sid = super::windows::current_user_sid().map_err(|_| denied())?;
        let parent = ancestors.last().ok_or_else(denied)?;
        private_owner_dacl(parent, &owner_sid)?;
        private_owner_dacl(&file, &owner_sid)?;
        Self::directory_contents(path)?;
        let canonical = final_path(&file)?;
        let drive = match canonical.components().next() {
            Some(Component::Prefix(p)) => match p.kind() {
                Prefix::VerbatimDisk(letter) => letter,
                _ => return Err(denied()),
            },
            _ => return Err(denied()),
        };
        let root = wide(OsStr::new(&format!("{}:\\", char::from(drive))));
        if unsafe { GetDriveTypeW(root.as_ptr()) } != DRIVE_FIXED {
            return Err(denied());
        }
        let mut filesystem = [0u16; 32];
        check(unsafe {
            GetVolumeInformationByHandleW(
                file.as_raw_handle(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            )
        })?;
        if !filesystem.starts_with(&[b'N' as u16, b'T' as u16, b'F' as u16, b'S' as u16, 0]) {
            return Err(denied());
        }
        let actual_digest = digest(&file)?;
        if actual_digest != expected_digest {
            return Err(denied());
        }
        Ok(Self {
            path: canonical,
            id: identity(&file)?,
            file,
            _ancestors: ancestors,
            digest: actual_digest,
        })
    }

    fn verify(&self) -> io::Result<()> {
        let owner_sid = super::windows::current_user_sid().map_err(|_| denied())?;
        private_owner_dacl(self._ancestors.last().ok_or_else(denied)?, &owner_sid)?;
        private_owner_dacl(&self.file, &owner_sid)?;
        Self::directory_contents(&self.path)?;
        let current = open_pinned(&self.path, false)?;
        if identity(&current)? != self.id
            || identity(&self.file)? != self.id
            || digest(&current)? != self.digest
        {
            return Err(denied());
        }
        Ok(())
    }

    fn directory_contents(image: &Path) -> io::Result<()> {
        let parent = image.parent().ok_or_else(denied)?;
        let expected = image.file_name().ok_or_else(denied)?;
        let mut entries = std::fs::read_dir(parent).map_err(|_| denied())?;
        let only = entries.next().ok_or_else(denied)?.map_err(|_| denied())?;
        if only.file_name() != expected || entries.next().is_some() {
            return Err(denied());
        }
        Ok(())
    }
}

fn creation(handle: HANDLE) -> io::Result<u64> {
    let mut created: FILETIME = unsafe { mem::zeroed() };
    let mut exit: FILETIME = unsafe { mem::zeroed() };
    let mut kernel: FILETIME = unsafe { mem::zeroed() };
    let mut user: FILETIME = unsafe { mem::zeroed() };
    check(unsafe { GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user) })?;
    Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

fn current_job_contains(pid: u32) -> io::Result<()> {
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
    check(unsafe {
        QueryInformationJobObject(
            ptr::null_mut(),
            JobObjectExtendedLimitInformation,
            (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            mem::size_of_val(&limits) as u32,
            ptr::null_mut(),
        )
    })?;
    let flags = limits.BasicLimitInformation.LimitFlags;
    if flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE == 0
        || flags & (JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK) != 0
    {
        return Err(denied());
    }
    // Current Job enumeration proves membership in the caller's Job, not just
    // an arbitrary Job. Bounded storage; an incomplete query refuses launch.
    let mut storage = vec![0usize; 1026];
    check(unsafe {
        QueryInformationJobObject(
            ptr::null_mut(),
            JobObjectBasicProcessIdList,
            storage.as_mut_ptr().cast(),
            (storage.len() * mem::size_of::<usize>()) as u32,
            ptr::null_mut(),
        )
    })?;
    let list = unsafe { &*storage.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
    if list.NumberOfAssignedProcesses != list.NumberOfProcessIdsInList
        || list.NumberOfProcessIdsInList > 1024
    {
        return Err(denied());
    }
    // The variable-length member array starts at the offset of ProcessIdList.
    let members = unsafe {
        std::slice::from_raw_parts(
            list.ProcessIdList.as_ptr(),
            list.NumberOfProcessIdsInList as usize,
        )
    };
    if !members.contains(&(pid as usize)) {
        return Err(denied());
    }
    Ok(())
}

/// Actual CreateProcess handle, never adopted/reopened by PID. Drop attempts
/// bounded cleanup; callers must call abort to confirm it, never infer success
/// from Drop or from one process exit when installer writers could exist later.
pub struct OwnedBootstrap {
    process: OwnedHandle,
    image: PinnedImage,
    pid: u32,
    created: u64,
    cleanup_attempted: bool,
}

impl OwnedBootstrap {
    fn spawn(image: PinnedImage, endpoint: &Endpoint) -> io::Result<Self> {
        current_job_contains(unsafe { GetCurrentProcessId() })?;
        image.verify()?;
        let application = wide(image.path.as_os_str());
        let mut command = wide(&command_line(&image.path, &endpoint.selector));
        let environment = [0u16; 2];
        let cwd = wide(image.path.parent().ok_or_else(denied)?.as_os_str());
        let mut startup: STARTUPINFOW = unsafe { mem::zeroed() };
        startup.cb = mem::size_of_val(&startup) as u32;
        let mut info: PROCESS_INFORMATION = unsafe { mem::zeroed() };
        check(unsafe {
            CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr().cast(),
                cwd.as_ptr(),
                &startup,
                &mut info,
            )
        })?;
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
        let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
        let mut child = Self {
            process,
            image,
            pid: info.dwProcessId,
            created: 0,
            cleanup_attempted: false,
        };
        // From here any failure drops/terminates the owned suspended child.
        child.created = creation(child.process.as_raw_handle())?;
        child.verify()?;
        current_job_contains(child.pid)?;
        if unsafe { ResumeThread(thread.as_raw_handle()) } != 1 {
            return Err(denied());
        }
        Ok(child)
    }

    fn verify(&self) -> io::Result<()> {
        let handle = self.process.as_raw_handle();
        if unsafe { WaitForSingleObject(handle, 0) } != WAIT_TIMEOUT
            || creation(handle)? != self.created
        {
            return Err(denied());
        }
        self.image.verify()?;
        let mut buffer = vec![0; 32_768];
        let mut length = buffer.len() as u32;
        check(unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) })?;
        let path = PathBuf::from(OsString::from_wide(&buffer[..length as usize]));
        let actual = open_pinned(&path, false)?;
        if identity(&actual)? != self.image.id || digest(&actual)? != self.image.digest {
            return Err(denied());
        }
        Ok(())
    }

    pub fn abort(mut self) -> io::Result<()> {
        self.cleanup()
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if self.cleanup_attempted {
            return Err(denied());
        }
        self.cleanup_attempted = true;
        let handle = self.process.as_raw_handle();
        match unsafe { WaitForSingleObject(handle, 0) } {
            WAIT_TIMEOUT => check(unsafe { TerminateProcess(handle, 1) })?,
            WAIT_OBJECT_0 => return Ok(()),
            _ => return Err(denied()),
        }
        if unsafe { WaitForSingleObject(handle, CLEANUP_MS) } != WAIT_OBJECT_0 {
            return Err(denied());
        }
        Ok(())
    }
}

impl Drop for OwnedBootstrap {
    fn drop(&mut self) {
        if !self.cleanup_attempted {
            let _ = self.cleanup();
        }
    }
}

fn production_command_line(path: &Path, selector: &str) -> OsString {
    let mut value = OsString::from("\"");
    value.push(path);
    value.push("\" --self-update-supervisor ");
    value.push(selector);
    value
}

fn command_line(path: &Path, selector: &str) -> OsString {
    // Minimal libtest-only transport; production bytes are asserted below.
    let mut value = OsString::from("\"");
    value.push(path);
    value.push("\" --exact windows_update_bootstrap::tests::bootstrap_child --ignored --skip ");
    value.push(selector);
    value
}

/// One endpoint, one launch, one accepted connection. No raw-stream accessor or
/// reconnect API: a rogue connection, timeout, child death or cancellation aborts.
pub struct Endpoint {
    pipe: NamedPipeServer,
    selector: String,
}

impl Endpoint {
    pub fn bind_random() -> io::Result<Self> {
        let bytes = random_nonce()?;
        let selector: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        Self::bind(&selector)
    }

    // Fixed selectors are confined to this module's adversarial fixtures.
    fn bind(selector: &str) -> io::Result<Self> {
        if selector.len() != 32
            || !selector
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(denied());
        }
        // Uses current-SID/SYSTEM-only DACL, FIRST_PIPE_INSTANCE, no remote
        // clients and no inheritable handle. Never impersonates the client.
        let pipe = super::windows::create_pipe(
            Path::new(&format!(r"\\.\pipe\Treer-update-{selector}")),
            true,
        )
        .map_err(|_| denied())?;
        Ok(Self {
            pipe,
            selector: selector.to_owned(),
        })
    }

    pub async fn launch(self, image: PinnedImage) -> io::Result<AuthenticatedConnection> {
        let deadline = tokio::time::Instant::now() + CONNECT_BUDGET;
        let child = blocking_phase(|| OwnedBootstrap::spawn(image, &self))?;
        self.accept_owned(child, deadline).await
    }

    async fn accept_owned(
        self,
        child: OwnedBootstrap,
        deadline: tokio::time::Instant,
    ) -> io::Result<AuthenticatedConnection> {
        if tokio::time::Instant::now() >= deadline {
            return Err(denied());
        }
        // Fixed deadline, not renewed after spawn. A future
        // dropped at await owns child, so cancellation triggers handle cleanup.
        // Synchronous native file/hash calls cannot be preempted; expiry is
        // rechecked before returning proof, never upgraded to late success.
        tokio::time::timeout_at(deadline, self.pipe.connect())
            .await
            .map_err(|_| denied())?
            .map_err(|_| denied())?;
        let mut pid = 0;
        check(unsafe { GetNamedPipeClientProcessId(self.pipe.as_raw_handle(), &mut pid) })?;
        if pid != child.pid {
            return Err(denied());
        }
        blocking_phase(|| {
            child.verify()?;
            current_job_contains(child.pid)
        })?;
        if tokio::time::Instant::now() >= deadline {
            return Err(denied());
        }
        Ok(AuthenticatedConnection {
            _pipe: self.pipe,
            child,
        })
    }
}

/// Owner checked the connected client, NOT a mutual handoff proof. There is no
/// permit send/ACK/dispatch API. The future child-side server/parent attestation,
/// locks and durable decision must be reviewed before any authority is added.
pub struct AuthenticatedConnection {
    _pipe: NamedPipeServer,
    child: OwnedBootstrap,
}

impl AuthenticatedConnection {
    pub fn abort(self) -> io::Result<()> {
        self.child.abort()
    }
}

#[cfg(test)]
mod tests {
    use super::parent_proof::{OwnerIdentity, ParentProof};
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::ClientOptions;

    async fn fixture_binding_exchange<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
        pipe: &mut S,
        selector: &str,
        registration: &[u8; 56],
        owner: bool,
        verify: impl Fn() -> io::Result<()>,
    ) -> io::Result<()> {
        use crate::update_binding::{Binding, Exchange, FRAME_SIZE};
        let deadline = tokio::time::Instant::now() + CONNECT_BUDGET;
        // These are synthetic fixture inputs, not durable preparation records.
        let attempt_hash = Sha256::digest(selector.as_bytes());
        let binding = Binding {
            attempt: attempt_hash[..16].try_into().map_err(|_| denied())?,
            intent: Sha256::digest(b"inert-parent-proof-fixture-v1").into(),
            registration: Sha256::digest(registration).into(),
        };
        let mut exchange = Exchange::new(binding, random_nonce()?, owner, deadline.into_std());
        if owner {
            let hello = exchange
                .start(tokio::time::Instant::now().into_std())
                .map_err(|_| denied())?;
            tokio::time::timeout_at(deadline, pipe.write_all(&hello))
                .await
                .map_err(|_| denied())?
                .map_err(|_| denied())?;
        }
        for _ in 0..2 {
            let mut bytes = [0; FRAME_SIZE];
            tokio::time::timeout_at(deadline, pipe.read_exact(&mut bytes))
                .await
                .map_err(|_| denied())?
                .map_err(|_| denied())?;
            blocking_phase(&verify)?;
            let response = exchange
                .receive(&bytes, tokio::time::Instant::now().into_std())
                .map_err(|_| denied())?;
            if let Some(response) = response {
                tokio::time::timeout_at(deadline, pipe.write_all(&response))
                    .await
                    .map_err(|_| denied())?
                    .map_err(|_| denied())?;
            }
        }
        if !exchange.complete() || tokio::time::Instant::now() >= deadline {
            return Err(denied());
        }
        Ok(())
    }

    struct PrivateImage {
        directory: PathBuf,
        path: PathBuf,
    }

    fn set_fixture_security(path: &Path, extra: &str) {
        let sid = super::super::windows::current_user_sid().unwrap();
        let sddl = wide(OsStr::new(&format!(
            "O:{sid}D:P(A;OICI;GA;;;SY)(A;OICI;GA;;;{sid}){extra}"
        )));
        let mut descriptor = ptr::null_mut();
        check(unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            )
        })
        .unwrap();
        let _allocation = SecurityAllocation(descriptor);
        let path = wide(path.as_os_str());
        check(unsafe {
            SetFileSecurityW(
                path.as_ptr(),
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor,
            )
        })
        .unwrap();
    }

    impl PrivateImage {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!("treer-bootstrap-{}", selector()));
            std::fs::create_dir(&directory).unwrap();
            set_fixture_security(&directory, "");
            let path = directory.join("controller.exe");
            std::fs::copy(std::env::current_exe().unwrap(), &path).unwrap();
            set_fixture_security(&path, "");
            Self { directory, path }
        }
    }

    impl Drop for PrivateImage {
        fn drop(&mut self) {
            // Exact fixture files only, no recursive deletion or glob cleanup.
            let _ = std::fs::remove_file(self.directory.join("unexpected.dll"));
            let _ = std::fs::remove_file(self.directory.join("controller.prev.exe"));
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_dir(&self.directory);
        }
    }

    #[tokio::test]
    async fn blocking_phase_refuses_single_thread_runtime_without_running_work() {
        assert!(blocking_phase(|| -> io::Result<()> {
            panic!("must refuse before running blocking work")
        })
        .is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn blocking_phase_yields_worker_to_other_tasks() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let task = tokio::spawn(async move {
            tokio::task::yield_now().await;
            sender.send(()).unwrap();
        });
        blocking_phase(|| {
            receiver
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| denied())
        })
        .unwrap();
        task.await.unwrap();
    }

    fn selector() -> String {
        // Test-only uniqueness; production uses the coordinator's job identity.
        format!(
            "{:032x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[tokio::test]
    async fn endpoint_refuses_invalid_selectors_and_an_existing_instance() {
        let random = Endpoint::bind_random().unwrap();
        assert_eq!(random.selector.len(), 32);
        assert!(Endpoint::bind(&random.selector).is_err());
        for value in ["", "..", "0000000000000000000000000000000G", "../other"] {
            assert!(Endpoint::bind(value).is_err());
        }
        let id = selector();
        let first = Endpoint::bind(&id).unwrap();
        assert!(
            Endpoint::bind(&id).is_err(),
            "must not attach to an existing pipe name"
        );
        drop(first);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn child_refuses_a_pipe_server_that_is_not_its_os_parent() {
        let endpoint = Endpoint::bind_random().unwrap();
        let client = ClientOptions::new()
            .open(format!(r"\\.\pipe\Treer-update-{}", endpoint.selector))
            .unwrap();
        endpoint.pipe.connect().await.unwrap();
        // Correct account and private endpoint, but the server is this process,
        // not the process listed by the OS as its parent.
        assert!(blocking_phase(|| ParentProof::attest(&client)).is_err());
    }

    #[test]
    fn production_argv_has_only_the_fixed_entry_and_selector() {
        let path = Path::new(r"C:\Treer state\job\controller.exe");
        let id = "0123456789abcdef0123456789abcdef";
        let actual: Vec<u16> = production_command_line(path, id).encode_wide().collect();
        let expected: Vec<u16> = OsStr::new(
            r#""C:\Treer state\job\controller.exe" --self-update-supervisor 0123456789abcdef0123456789abcdef"#,
        )
        .encode_wide()
        .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn pinned_image_rejects_digest_conflict_and_keeps_the_file_unwritable() {
        let fixture = PrivateImage::new();
        let path = fixture.path.clone();
        let expected = digest(&File::open(&path).unwrap()).unwrap();
        let image = PinnedImage::open_blocking(&path, expected).unwrap();
        image.verify().unwrap();
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        let mut conflict = expected;
        conflict[0] ^= 1;
        assert!(PinnedImage::open_blocking(&path, conflict).is_err());
        assert!(PinnedImage::open_blocking(Path::new("relative.exe"), expected).is_err());
    }

    #[test]
    fn pinned_image_refuses_foreign_writers_and_extra_directory_entries() {
        let fixture = PrivateImage::new();
        let expected = digest(&File::open(&fixture.path).unwrap()).unwrap();
        let image = PinnedImage::open_blocking(&fixture.path, expected).unwrap();
        // Directory content is checked again before any launch/connection proof.
        std::fs::write(fixture.directory.join("unexpected.dll"), b"not executable").unwrap();
        assert!(image.verify().is_err());
        drop(image);
        assert!(PinnedImage::open_blocking(&fixture.path, expected).is_err());
        std::fs::remove_file(fixture.directory.join("unexpected.dll")).unwrap();

        // World write on the job directory is rejected even when the image
        // itself retains its protected, current-user/SYSTEM-only DACL.
        set_fixture_security(&fixture.directory, "(A;;GW;;;WD)");
        assert!(PinnedImage::open_blocking(&fixture.path, expected).is_err());
        set_fixture_security(&fixture.directory, "");
        set_fixture_security(&fixture.path, "(A;;GW;;;WD)");
        assert!(PinnedImage::open_blocking(&fixture.path, expected).is_err());
        set_fixture_security(&fixture.path, "");
        assert!(PinnedImage::open_blocking(&fixture.path, expected).is_ok());
    }

    #[test]
    fn parent_image_pin_allows_rename_but_denies_writes_to_the_retained_file() {
        let fixture = PrivateImage::new();
        let parent_image = parent_proof::open_parent_image(&fixture.path).unwrap();
        let before = identity(&parent_image).unwrap();
        let expected = digest(&parent_image).unwrap();
        let renamed = fixture.directory.join("controller.prev.exe");
        std::fs::rename(&fixture.path, &renamed).unwrap();
        assert_eq!(identity(&parent_image).unwrap(), before);
        assert!(OpenOptions::new().write(true).open(&renamed).is_err());
        let fresh = parent_proof::open_parent_image(&renamed).unwrap();
        assert_eq!(identity(&fresh).unwrap(), before);
        assert_eq!(digest(&fresh).unwrap(), expected);
        drop(fresh);
        drop(parent_image);
        std::fs::rename(renamed, &fixture.path).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "owned child of native_owner_round_trip only"]
    async fn bootstrap_child() {
        let args: Vec<_> = std::env::args().collect();
        let at = args.iter().position(|v| v == "--skip").unwrap();
        let id = &args[at + 1];
        // A fixed nonconnecting role lets the owner exercise a rogue peer and
        // cancellation without timing races. No production argv/env override.
        let mut pipe = if id.starts_with('0') {
            None
        } else {
            Some(
                ClientOptions::new()
                    .open(format!(r"\\.\pipe\Treer-update-{id}"))
                    .unwrap(),
            )
        };
        // This bounded fixture exchange exercises kernel-parent attestation and
        // registration comparison only; it is not a production handoff frame.
        let _parent = if let Some(pipe) = pipe.as_mut() {
            let deadline = tokio::time::Instant::now() + CONNECT_BUDGET;
            let parent = blocking_phase(|| ParentProof::attest(pipe)).unwrap();
            let mut registration = [0; 56];
            tokio::time::timeout_at(deadline, pipe.read_exact(&mut registration))
                .await
                .unwrap()
                .unwrap();
            if blocking_phase(|| parent.verify_registration(&registration)).is_err() {
                return;
            }
            tokio::time::timeout_at(deadline, pipe.write_all(&[1]))
                .await
                .unwrap()
                .unwrap();
            fixture_binding_exchange(pipe, id, &registration, false, || {
                parent.verify_registration(&registration)
            })
            .await
            .unwrap();
            Some(parent)
        } else {
            None
        };
        tokio::time::sleep(Duration::from_secs(10)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "disposable Windows worker outside ambient Jobs; never win_native"]
    async fn native_owner_round_trip() {
        let mut in_job = 0;
        check(unsafe { IsProcessInJob(GetCurrentProcess(), ptr::null_mut(), &mut in_job) })
            .unwrap();
        assert_eq!(in_job, 0, "native fixture must start outside ambient Jobs");
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        assert!(!raw.is_null());
        let job = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        check(unsafe {
            SetInformationJobObject(
                raw,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                mem::size_of_val(&limits) as u32,
            )
        })
        .unwrap();
        check(unsafe { AssignProcessToJobObject(raw, GetCurrentProcess()) }).unwrap();
        // This isolated test process is itself in the Job. Retain the root until
        // process exit, so dropping it does not kill the harness before reporting.
        mem::forget(job);
        let fixture = PrivateImage::new();
        let path = fixture.path.clone();
        let expected = digest(&File::open(&path).unwrap()).unwrap();
        let id = format!("f{}", &selector()[1..]);
        let endpoint = Endpoint::bind(&id).unwrap();
        let image = PinnedImage::open(&path, expected).await.unwrap();
        let mut connection = endpoint.launch(image).await.unwrap();
        connection.child.verify().unwrap();
        let registration = blocking_phase(OwnerIdentity::current).unwrap().encode();
        let deadline = tokio::time::Instant::now() + CONNECT_BUDGET;
        tokio::time::timeout_at(deadline, connection._pipe.write_all(&registration))
            .await
            .unwrap()
            .unwrap();
        let mut ack = [0];
        tokio::time::timeout_at(deadline, connection._pipe.read_exact(&mut ack))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ack, [1]);
        let child = &connection.child;
        fixture_binding_exchange(&mut connection._pipe, &id, &registration, true, || {
            child.verify()
        })
        .await
        .unwrap();
        connection.abort().unwrap();

        // A legitimate kernel parent cannot substitute a different registration
        // PID, creation time, volume/file identity or digest on this connection.
        for offset in [0, 4, 12, 16, 24] {
            let id = format!("e{}", &selector()[1..]);
            let endpoint = Endpoint::bind(&id).unwrap();
            let image = PinnedImage::open(&path, expected).await.unwrap();
            let mut connection = endpoint.launch(image).await.unwrap();
            let mut wrong = registration;
            wrong[offset] ^= 1;
            let deadline = tokio::time::Instant::now() + CONNECT_BUDGET;
            tokio::time::timeout_at(deadline, connection._pipe.write_all(&wrong))
                .await
                .unwrap()
                .unwrap();
            assert!(
                tokio::time::timeout_at(deadline, connection._pipe.read_exact(&mut ack))
                    .await
                    .unwrap()
                    .is_err()
            );
            connection.abort().unwrap();
        }

        for rogue in [true, false] {
            let id = format!("0{}", &selector()[1..]);
            let endpoint = Endpoint::bind(&id).unwrap();
            let image = PinnedImage::open(&path, expected).await.unwrap();
            let child = OwnedBootstrap::spawn(image, &endpoint).unwrap();
            let mut duplicate = ptr::null_mut();
            check(unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    child.process.as_raw_handle(),
                    GetCurrentProcess(),
                    &mut duplicate,
                    PROCESS_SYNCHRONIZE,
                    0,
                    0,
                )
            })
            .unwrap();
            let observed = unsafe { OwnedHandle::from_raw_handle(duplicate) };
            if rogue {
                // Current process has the correct SID and knows the pipe name,
                // but its kernel PID is not the owned child's PID.
                let _rogue = ClientOptions::new()
                    .open(format!(r"\\.\pipe\Treer-update-{id}"))
                    .unwrap();
                assert!(endpoint
                    .accept_owned(child, tokio::time::Instant::now() + CONNECT_BUDGET)
                    .await
                    .is_err());
            } else {
                let future =
                    endpoint.accept_owned(child, tokio::time::Instant::now() + CONNECT_BUDGET);
                assert!(tokio::time::timeout(Duration::from_millis(50), future)
                    .await
                    .is_err());
            }
            assert_eq!(
                unsafe { WaitForSingleObject(observed.as_raw_handle(), 0) },
                WAIT_OBJECT_0,
                "rogue client/cancellation must clean up the actual owned child"
            );
        }
    }
}
