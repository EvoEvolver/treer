//! Win32 ownership and ACL adapters. Every owned handle/allocation has one RAII owner.
use std::{
    ffi::OsStr,
    io, mem,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    System::{JobObjects::*, Threading::*},
};

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
fn checked(success: i32) -> io::Result<()> {
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Do not trust a caller/registry override of the SystemRoot variable when
/// configuring module discovery for Windows PowerShell.
pub fn system_windows_directory() -> io::Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
    let mut buffer = vec![0u16; 32_768];
    let length = unsafe { GetSystemWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize >= buffer.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Windows directory length",
        ));
    }
    let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize]));
    if !matches!(path.components().next(), Some(std::path::Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_)))
        || !path.is_absolute()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Windows installation directory",
        ));
    }
    Ok(path)
}

/// Metadata from an already-open, non-reparse-following handle.
pub fn transfer_file_identity(file: &std::fs::File) -> io::Result<(u64, u64, u32, u32)> {
    use windows_sys::Win32::Storage::FileSystem::*;
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = mem::zeroed();
        checked(GetFileInformationByHandle(file.as_raw_handle(), &mut info))?;
        Ok((
            u64::from(info.dwVolumeSerialNumber),
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            info.nNumberOfLinks,
            info.dwFileAttributes,
        ))
    }
}

/// Atomic same-directory publication, with no delete-then-rename interval.
pub fn publish_transfer_file(source: &Path, target: &Path, overwrite: bool) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::*;
    let source = wide(source.as_os_str());
    let target = wide(target.as_os_str());
    let flags = MOVEFILE_WRITE_THROUGH
        | if overwrite {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    unsafe { checked(MoveFileExW(source.as_ptr(), target.as_ptr(), flags)) }
}

pub fn transfer_available_bytes(path: &Path) -> io::Result<u64> {
    let path = wide(path.as_os_str());
    let mut bytes = 0;
    unsafe {
        checked(
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                path.as_ptr(),
                &mut bytes,
                ptr::null_mut(),
                ptr::null_mut(),
            ),
        )?;
    }
    Ok(bytes)
}
struct Allocation(*mut std::ffi::c_void);
impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

pub fn current_user_sid() -> io::Result<String> {
    unsafe {
        let mut token = ptr::null_mut();
        checked(OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY,
            &mut token,
        ))?;
        let token = OwnedHandle::from_raw_handle(token);
        let mut size = 0;
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut size,
        );
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        // usize storage provides TOKEN_USER pointer alignment as well as the requested bytes.
        let mut storage = vec![0usize; (size as usize).div_ceil(mem::size_of::<usize>())];
        checked(GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            storage.as_mut_ptr().cast(),
            size,
            &mut size,
        ))?;
        let user = &*storage.as_ptr().cast::<TOKEN_USER>();
        let mut sid = ptr::null_mut();
        checked(ConvertSidToStringSidW(user.User.Sid, &mut sid))?;
        let allocation = Allocation(sid.cast());
        let mut length = 0;
        while *sid.add(length) != 0 {
            length += 1;
        }
        let result = String::from_utf16(std::slice::from_raw_parts(sid, length))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        drop(allocation);
        result
    }
}

fn descriptor(inherit: bool) -> io::Result<Allocation> {
    let sid = current_user_sid()?;
    let flags = if inherit { "OICI" } else { "" };
    let sddl = wide(OsStr::new(&format!(
        "D:P(A;{flags};GA;;;SY)(A;{flags};GA;;;{sid})"
    )));
    unsafe {
        let mut descriptor = ptr::null_mut();
        checked(ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        ))?;
        Ok(Allocation(descriptor))
    }
}

pub fn secure_path(path: &Path) -> io::Result<()> {
    let descriptor = descriptor(path.is_dir())?;
    let name = wide(path.as_os_str());
    unsafe {
        checked(SetFileSecurityW(
            name.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor.0,
        ))
    }
}

/// Owner and discretionary ACL of a file object as the system reports them.
struct SecurityReport {
    owner: String,
    control: u16,
    /// (type, flags, access mask, SID) per ACE, in ACL order.
    aces: Vec<(u8, u8, u32, String)>,
}

fn read_security(path: &Path) -> io::Result<SecurityReport> {
    let name = wide(path.as_os_str());
    unsafe {
        let mut owner = ptr::null_mut();
        let mut dacl = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        let status = GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        );
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let descriptor = Allocation(descriptor.cast());
        let mut control = 0u16;
        let mut revision = 0u32;
        checked(GetSecurityDescriptorControl(
            descriptor.0,
            &mut control,
            &mut revision,
        ))?;
        let owner = if owner.is_null() {
            String::from("<none>")
        } else {
            sid_string(owner)?
        };
        let mut aces = Vec::new();
        if !dacl.is_null() {
            let mut size = ACL_SIZE_INFORMATION {
                AceCount: 0,
                AclBytesInUse: 0,
                AclBytesFree: 0,
            };
            checked(GetAclInformation(
                dacl,
                (&mut size as *mut ACL_SIZE_INFORMATION).cast(),
                mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            ))?;
            for index in 0..size.AceCount {
                let mut ace = ptr::null_mut();
                checked(GetAce(dacl, index, &mut ace))?;
                let header = &*ace.cast::<ACE_HEADER>();
                // Every ACE type shares the header; the mask/SID layout read
                // here is that of ACCESS_ALLOWED/DENIED ACEs, which is all a
                // private directory may legitimately carry.
                let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
                let sid = sid_string(ptr::addr_of!(allowed.SidStart).cast_mut().cast())?;
                aces.push((header.AceType, header.AceFlags, allowed.Mask, sid));
            }
        }
        Ok(SecurityReport {
            owner,
            control,
            aces,
        })
    }
}

/// Well-known principals a private Treer directory may name besides the
/// current user: LocalSystem and the Administrators group (the default owner
/// of objects created by an elevated administrator; administrators can take
/// ownership of any object regardless, so this grants nothing new).
const SYSTEM_SID: &str = "S-1-5-18";
const ADMINISTRATORS_SID: &str = "S-1-5-32-544";

/// Whether `path` carries the private descriptor `secure_path` writes: owned by
/// the current user (or SYSTEM / the Administrators group), a present and
/// protected DACL whose entries all allow only the current user or SYSTEM (no
/// inherited or foreign principals, no deny entries). Reads the live security
/// descriptor, so later loosening is seen.
pub fn private_path_is_secure(path: &Path) -> io::Result<bool> {
    // ACE type values are fixed by the Win32 ABI (ACCESS_ALLOWED_ACE_TYPE = 0).
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    let current = current_user_sid()?;
    let report = read_security(path)?;
    if report.owner != current && report.owner != SYSTEM_SID && report.owner != ADMINISTRATORS_SID {
        return Ok(false);
    }
    if report.control & SE_DACL_PRESENT == 0 || report.control & SE_DACL_PROTECTED == 0 {
        return Ok(false);
    }
    if report.aces.is_empty() {
        return Ok(false);
    }
    for (ace_type, flags, _mask, sid) in &report.aces {
        if *ace_type != ACCESS_ALLOWED_ACE_TYPE || flags & INHERITED_ACE as u8 != 0 {
            return Ok(false);
        }
        if sid != &current && sid != SYSTEM_SID {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Categorical owner/control/ACE listing for diagnostics: principals are
/// classified (current_user, system, administrators, everyone, other), never
/// printed as raw SIDs; no path content or credentials.
pub fn private_path_security_report(path: &Path) -> io::Result<String> {
    fn classify(sid: &str, current: &str) -> &'static str {
        match sid {
            _ if sid == current => "current_user",
            SYSTEM_SID => "system",
            ADMINISTRATORS_SID => "administrators",
            "S-1-1-0" => "everyone",
            "S-1-5-32-545" => "users",
            "S-1-5-11" => "authenticated_users",
            "<none>" => "none",
            _ => "other",
        }
    }
    let current = current_user_sid()?;
    let report = read_security(path)?;
    let mut text = format!(
        "owner={} control=0x{:04x} dacl_present={} dacl_protected={} aces={}",
        classify(&report.owner, &current),
        report.control,
        report.control & SE_DACL_PRESENT != 0,
        report.control & SE_DACL_PROTECTED != 0,
        report.aces.len()
    );
    for (ace_type, flags, mask, sid) in &report.aces {
        text.push_str(&format!(
            " [type={ace_type} inherited={} flags=0x{flags:02x} mask=0x{mask:08x} principal={}]",
            flags & INHERITED_ACE as u8 != 0,
            classify(sid, &current)
        ));
    }
    Ok(text)
}

unsafe fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = ptr::null_mut();
    checked(ConvertSidToStringSidW(sid, &mut text))?;
    let allocation = Allocation(text.cast());
    let mut length = 0;
    while *text.add(length) != 0 {
        length += 1;
    }
    let result = String::from_utf16(std::slice::from_raw_parts(text, length))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    drop(allocation);
    result
}

pub fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source = wide(source.as_os_str());
    let destination = wide(destination.as_os_str());
    unsafe {
        checked(MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        ))
    }
}

pub fn create_pipe(
    path: &Path,
    first: bool,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let descriptor = descriptor(false)?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    // CreateNamedPipe copies the security descriptor during this call. It cannot
    // outlive descriptor; no handle is inherited or remotely accessible.
    unsafe {
        tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                path,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}

fn open_process(pid: u32, access: u32) -> io::Result<Option<OwnedHandle>> {
    unsafe {
        let handle = OpenProcess(access, 0, pid);
        if handle.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                return Ok(None);
            }
            return Err(error);
        }
        Ok(Some(OwnedHandle::from_raw_handle(handle)))
    }
}

fn creation_time(handle: &OwnedHandle) -> io::Result<Option<u64>> {
    unsafe {
        let mut exit_code = 0;
        checked(GetExitCodeProcess(handle.as_raw_handle(), &mut exit_code))?;
        if exit_code != STILL_ACTIVE as u32 {
            return Ok(None);
        }
        let mut created: FILETIME = mem::zeroed();
        let mut exit: FILETIME = mem::zeroed();
        let mut kernel: FILETIME = mem::zeroed();
        let mut user: FILETIME = mem::zeroed();
        checked(GetProcessTimes(
            handle.as_raw_handle(),
            &mut created,
            &mut exit,
            &mut kernel,
            &mut user,
        ))?;
        Ok(Some(
            (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
        ))
    }
}

pub fn process_created_at(pid: u32) -> io::Result<Option<u64>> {
    open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?
        .map(|handle| creation_time(&handle))
        .transpose()
        .map(Option::flatten)
}

pub fn terminate_process(pid: u32, expected_creation: u64) -> io::Result<()> {
    let Some(handle) = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE)?
    else {
        return Ok(());
    };
    if creation_time(&handle)? != Some(expected_creation) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "process identity changed; refusing to terminate",
        ));
    }
    // Inspect and terminate the same open process handle, preventing PID reuse races.
    unsafe { checked(TerminateProcess(handle.as_raw_handle(), 0)) }
}

/// Owned by the stable Host. Child processes inherit the job; Controller
/// replacement never closes it. Host exit cleans up its complete process tree.
pub struct ProcessTreeGuard {
    _handle: OwnedHandle,
}
impl ProcessTreeGuard {
    pub fn new() -> io::Result<Self> {
        unsafe { Self::assign(GetCurrentProcess()) }
    }

    /// Own one workload and its descendants in a nested job.
    pub fn for_process(pid: u32) -> io::Result<Option<Self>> {
        let Some(process) = open_process(
            pid,
            PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
        )?
        else {
            return Ok(None);
        };
        if creation_time(&process)?.is_none() {
            return Ok(None);
        }
        match unsafe { Self::assign(process.as_raw_handle()) } {
            Ok(job) => Ok(Some(job)),
            Err(_) if creation_time(&process)?.is_none() => Ok(None),
            Err(error) => Err(error),
        }
    }

    unsafe fn assign(process: HANDLE) -> io::Result<Self> {
        unsafe {
            let handle = CreateJobObjectW(ptr::null(), ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let handle = OwnedHandle::from_raw_handle(handle);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            checked(SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                mem::size_of_val(&limits) as u32,
            ))?;
            checked(AssignProcessToJobObject(handle.as_raw_handle(), process))?;
            Ok(Self { _handle: handle })
        }
    }
}

/// Thread-bound startup lock. Windows releases an abandoned mutex on owner exit.
pub struct LocalLock {
    handle: OwnedHandle,
    _thread_bound: std::marker::PhantomData<*mut ()>,
}
impl LocalLock {
    pub fn acquire(key: &str) -> io::Result<Self> {
        let endpoint = super::endpoint_name(key, &current_user_sid()?);
        let name = wide(OsStr::new(&format!(
            "Global\\{}",
            endpoint
                .file_name()
                .unwrap_or(endpoint.as_os_str())
                .to_string_lossy()
        )));
        let descriptor = descriptor(false)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        unsafe {
            let handle = CreateMutexW(&attributes, 0, name.as_ptr());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let handle = OwnedHandle::from_raw_handle(handle);
            match WaitForSingleObject(handle.as_raw_handle(), 30_000) {
                WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self {
                    handle,
                    _thread_bound: std::marker::PhantomData,
                }),
                WAIT_TIMEOUT => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Host startup lock is occupied",
                )),
                _ => Err(io::Error::last_os_error()),
            }
        }
    }
}
impl Drop for LocalLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.handle.as_raw_handle());
        }
    }
}
