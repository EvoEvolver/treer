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

/// Classify the PROCESS token for the Windows administrator requirement.
/// Elevated: full-elevation token, or an unsplit token that is a member of
/// BUILTIN\Administrators (UAC off, built-in Administrator, elevated service).
/// AdminCapable: the UAC-limited token of an Administrators member.
/// Standard: an unsplit token that is not a member. Every query uses the
/// process token (membership through an identification-level duplicate of
/// it), so thread impersonation never influences the result.
pub fn account_class() -> io::Result<super::WindowsAccountClass> {
    unsafe {
        let mut token = ptr::null_mut();
        // TOKEN_DUPLICATE is required for the identification-level duplicate
        // that the membership check below needs.
        checked(OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &mut token,
        ))?;
        let token = OwnedHandle::from_raw_handle(token);
        let mut elevation: TOKEN_ELEVATION_TYPE = 0;
        let mut size = 0u32;
        checked(GetTokenInformation(
            token.as_raw_handle(),
            TokenElevationType,
            (&mut elevation as *mut TOKEN_ELEVATION_TYPE).cast(),
            mem::size_of::<TOKEN_ELEVATION_TYPE>() as u32,
            &mut size,
        ))?;
        if elevation == TokenElevationTypeFull {
            return Ok(super::WindowsAccountClass::Elevated);
        }
        if elevation == TokenElevationTypeLimited {
            return Ok(super::WindowsAccountClass::AdminCapable);
        }
        // Unsplit token (UAC off, built-in Administrator, service, or a
        // standard user): membership decides.
        Ok(if administrators_member(&token)? {
            super::WindowsAccountClass::Elevated
        } else {
            super::WindowsAccountClass::Standard
        })
    }
}

/// Whether BUILTIN\Administrators is an enabled group of `token` (a process
/// token opened with TOKEN_QUERY | TOKEN_DUPLICATE). CheckTokenMembership needs
/// an impersonation token, so an identification-level duplicate of the given
/// token is used: the verdict is bound to that token, never to a thread's
/// impersonation state. For a UAC-limited administrator token the group is
/// deny-only and this returns false.
fn administrators_member(token: &OwnedHandle) -> io::Result<bool> {
    unsafe {
        let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut sid_size = SECURITY_MAX_SID_SIZE;
        checked(CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            ptr::null_mut(),
            sid.as_mut_ptr().cast(),
            &mut sid_size,
        ))?;
        let mut impersonation = ptr::null_mut();
        checked(DuplicateToken(
            token.as_raw_handle(),
            SecurityIdentification,
            &mut impersonation,
        ))?;
        let impersonation = OwnedHandle::from_raw_handle(impersonation);
        let mut member = 0;
        checked(CheckTokenMembership(
            impersonation.as_raw_handle(),
            sid.as_mut_ptr().cast(),
            &mut member,
        ))?;
        Ok(member != 0)
    }
}

/// Exercise the membership path (DuplicateToken + CheckTokenMembership) on the
/// current process token regardless of its elevation type. Diagnostic/test
/// support: returns the raw membership verdict.
pub fn process_token_administrators_member() -> io::Result<bool> {
    unsafe {
        let mut token = ptr::null_mut();
        checked(OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &mut token,
        ))?;
        administrators_member(&OwnedHandle::from_raw_handle(token))
    }
}

pub fn current_user_sid() -> io::Result<String> {
    token_sid(TokenUser)
}

/// The SID the process token assigns as default owner of new objects. For an
/// elevated administrator this is the Administrators group, not the user.
pub fn current_token_owner_sid() -> io::Result<String> {
    token_sid(TokenOwner)
}

fn token_sid(class: TOKEN_INFORMATION_CLASS) -> io::Result<String> {
    unsafe {
        let mut token = ptr::null_mut();
        checked(OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY,
            &mut token,
        ))?;
        let token = OwnedHandle::from_raw_handle(token);
        let mut size = 0;
        GetTokenInformation(token.as_raw_handle(), class, ptr::null_mut(), 0, &mut size);
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        // usize storage provides pointer alignment as well as the requested bytes.
        let mut storage = vec![0usize; (size as usize).div_ceil(mem::size_of::<usize>())];
        checked(GetTokenInformation(
            token.as_raw_handle(),
            class,
            storage.as_mut_ptr().cast(),
            size,
            &mut size,
        ))?;
        // TOKEN_USER starts with SID_AND_ATTRIBUTES { Sid, Attributes } and
        // TOKEN_OWNER with { Owner }: both begin with the PSID.
        let sid: PSID = if class == TokenOwner {
            (*storage.as_ptr().cast::<TOKEN_OWNER>()).Owner
        } else {
            (*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid
        };
        sid_string(sid)
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

/// Exclusive, non-inherited handle and private DACL from the first instant.
/// The caller retains checked physical parent handles for the complete write.
pub(super) fn create_private_update_file(path: &Path) -> io::Result<std::fs::File> {
    create_private_update_file_access(path, GENERIC_READ | GENERIC_WRITE)
}

pub(super) fn create_private_update_candidate(path: &Path) -> io::Result<std::fs::File> {
    create_private_update_file_access(
        path,
        GENERIC_READ | GENERIC_WRITE | windows_sys::Win32::Storage::FileSystem::DELETE,
    )
}

fn create_private_update_file_access(path: &Path, access: u32) -> io::Result<std::fs::File> {
    use windows_sys::Win32::Storage::FileSystem::*;
    let descriptor = descriptor(false)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let path = wide(path.as_os_str());
    unsafe {
        let handle = CreateFileW(
            path.as_ptr(),
            access,
            0,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(std::fs::File::from_raw_handle(handle))
    }
}

/// Rename the held candidate itself, without reopening its source pathname.
/// No POSIX/delete-sharing bypass flags: locked destination images must refuse.
pub(super) fn rename_update_candidate(file: &std::fs::File, target: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::*;
    let name = wide(target.as_os_str());
    let offset = mem::offset_of!(FILE_RENAME_INFO, FileName);
    let size = offset
        .checked_add(name.len() * 2)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| io::Error::other("update rename name too large"))?;
    // u64 storage provides alignment for FILE_RENAME_INFO on supported Windows.
    let mut storage = vec![0_u64; (size as usize).div_ceil(8)];
    unsafe {
        let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        (*info).Anonymous.ReplaceIfExists = true;
        (*info).RootDirectory = ptr::null_mut();
        (*info).FileNameLength = ((name.len() - 1) * 2) as u32;
        ptr::copy_nonoverlapping(
            name.as_ptr(),
            storage.as_mut_ptr().cast::<u8>().add(offset).cast::<u16>(),
            name.len(),
        );
        checked(SetFileInformationByHandle(
            file.as_raw_handle(),
            FileRenameInfo,
            info.cast(),
            size,
        ))
    }
}

/// Reduce access on the same file object without a path reopen or sharing gap.
/// The exclusive sharing mode survives in the duplicate; callers cannot write
/// through the returned handle. Never inherit it into a child process.
pub(super) fn readonly_update_file(file: std::fs::File) -> io::Result<std::fs::File> {
    let mut handle = ptr::null_mut();
    unsafe {
        checked(DuplicateHandle(
            GetCurrentProcess(),
            file.as_raw_handle(),
            GetCurrentProcess(),
            &mut handle,
            windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ,
            0,
            0,
        ))?;
        Ok(std::fs::File::from_raw_handle(handle))
    }
}

/// This record backend is deliberately local NTFS-only. Network/FAT/ReFS are
/// not admitted merely because an individual file write happened to succeed.
pub(super) fn require_update_ntfs(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::*;
    use windows_sys::Win32::System::{
        SystemServices::FILE_PERSISTENT_ACLS, WindowsProgramming::DRIVE_FIXED,
    };
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let mut filesystem = [0_u16; 32];
    let mut flags = 0;
    unsafe {
        checked(GetVolumeInformationByHandleW(
            directory.as_raw_handle(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut flags,
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        ))?;
    }
    let root = path.components().take(2).collect::<std::path::PathBuf>();
    if unsafe { GetDriveTypeW(wide(root.as_os_str()).as_ptr()) } != DRIVE_FIXED
        || filesystem[..5] != [b'N' as u16, b'T' as u16, b'F' as u16, b'S' as u16, 0]
        || flags & FILE_PERSISTENT_ACLS == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows update store requires local NTFS",
        ));
    }
    Ok(())
}

/// One discretionary ACE. Mask and principal are read only for the two plain
/// ACE types whose layout is `{ Header, Mask, SidStart }`; every other type is
/// recorded by header only and never parsed as that layout.
struct AceReport {
    ace_type: u8,
    flags: u8,
    /// `Some` only for ACCESS_ALLOWED / ACCESS_DENIED ACEs of a sane size.
    plain: Option<(u32, String)>,
}

/// Owner and discretionary ACL of a file object as the system reports them.
struct SecurityReport {
    owner: String,
    control: u16,
    aces: Vec<AceReport>,
}

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
/// Bound on ACEs inspected or reported; a longer DACL is not private anyway.
const MAX_REPORTED_ACES: u32 = 32;

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
            for index in 0..size.AceCount.min(MAX_REPORTED_ACES) {
                let mut ace = ptr::null_mut();
                checked(GetAce(dacl, index, &mut ace))?;
                // Only the header is common to every ACE type. The plain
                // allowed/denied layout is read solely for those two types and
                // only when the ACE is large enough to hold it; anything else
                // (object, callback, audit, unknown) stays header-only.
                let header = *ace.cast::<ACE_HEADER>();
                let plain = if matches!(
                    header.AceType,
                    ACCESS_ALLOWED_ACE_TYPE | ACCESS_DENIED_ACE_TYPE
                ) && usize::from(header.AceSize)
                    >= mem::size_of::<ACCESS_ALLOWED_ACE>()
                {
                    let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
                    let sid = sid_string(ptr::addr_of!(allowed.SidStart).cast_mut().cast())?;
                    Some((allowed.Mask, sid))
                } else {
                    None
                };
                aces.push(AceReport {
                    ace_type: header.AceType,
                    flags: header.AceFlags,
                    plain,
                });
            }
            if size.AceCount > MAX_REPORTED_ACES {
                aces.push(AceReport {
                    ace_type: u8::MAX,
                    flags: 0,
                    plain: None,
                });
            }
        }
        Ok(SecurityReport {
            owner,
            control,
            aces,
        })
    }
}

const SYSTEM_SID: &str = "S-1-5-18";
const ADMINISTRATORS_SID: &str = "S-1-5-32-544";

/// Whether `path` carries the private descriptor `secure_path` writes: owned by
/// this process's token user or token owner (the SID the system assigns as
/// default owner of objects this process creates), a present and protected
/// DACL whose entries are all non-inherited plain ACCESS_ALLOWED entries for
/// the token user or LocalSystem. Any other ACE type, principal, deny entry or
/// inherited entry fails closed. Reads the live descriptor, so later
/// loosening is seen.
pub fn private_path_is_secure(path: &Path) -> io::Result<bool> {
    let user = current_user_sid()?;
    let token_owner = current_token_owner_sid()?;
    let report = read_security(path)?;
    if report.owner != user && report.owner != token_owner {
        return Ok(false);
    }
    if report.control & SE_DACL_PRESENT == 0 || report.control & SE_DACL_PROTECTED == 0 {
        return Ok(false);
    }
    if report.aces.is_empty() {
        return Ok(false);
    }
    for ace in &report.aces {
        if ace.ace_type != ACCESS_ALLOWED_ACE_TYPE || ace.flags & INHERITED_ACE as u8 != 0 {
            return Ok(false);
        }
        match &ace.plain {
            Some((_mask, sid)) if sid == &user || sid == SYSTEM_SID => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// Categorical, bounded owner/control/ACE listing for diagnostics. Principals
/// are classified relative to this process's token (current_user,
/// token_owner, system, administrators, everyone, users, authenticated_users,
/// other); raw SIDs, paths, host names and environment values never appear.
pub fn private_path_security_report(path: &Path) -> io::Result<String> {
    fn classify(sid: &str, user: &str, token_owner: &str) -> &'static str {
        match sid {
            _ if sid == user => "current_user",
            _ if sid == token_owner => "token_owner",
            SYSTEM_SID => "system",
            ADMINISTRATORS_SID => "administrators",
            "S-1-1-0" => "everyone",
            "S-1-5-32-545" => "users",
            "S-1-5-11" => "authenticated_users",
            "<none>" => "none",
            _ => "other",
        }
    }
    let user = current_user_sid()?;
    let token_owner = current_token_owner_sid()?;
    let report = read_security(path)?;
    let mut text = format!(
        "owner={} token_owner_is_user={} control=0x{:04x} dacl_present={} dacl_protected={} aces={}",
        classify(&report.owner, &user, &token_owner),
        token_owner == user,
        report.control,
        report.control & SE_DACL_PRESENT != 0,
        report.control & SE_DACL_PROTECTED != 0,
        report.aces.len()
    );
    for ace in &report.aces {
        let inherited = ace.flags & INHERITED_ACE as u8 != 0;
        match &ace.plain {
            Some((mask, sid)) => text.push_str(&format!(
                " [type={} inherited={inherited} flags=0x{:02x} mask=0x{mask:08x} principal={}]",
                ace.ace_type,
                ace.flags,
                classify(sid, &user, &token_owner)
            )),
            None => text.push_str(&format!(
                " [type={} inherited={inherited} flags=0x{:02x} unsupported]",
                ace.ace_type, ace.flags
            )),
        }
    }
    Ok(text)
}

/// Apply an SDDL security descriptor (DACL part) to a path. Test support for
/// native regressions that need unusual ACE types; production code uses
/// `secure_path`.
pub fn apply_sddl_dacl(path: &Path, sddl: &str) -> io::Result<()> {
    let sddl = wide(OsStr::new(sddl));
    let name = wide(path.as_os_str());
    unsafe {
        let mut descriptor = ptr::null_mut();
        checked(ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        ))?;
        let descriptor = Allocation(descriptor);
        checked(SetFileSecurityW(
            name.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor.0,
        ))
    }
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
