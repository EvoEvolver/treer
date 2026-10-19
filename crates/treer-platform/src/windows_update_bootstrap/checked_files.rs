//! W1a native fixture: private attempt creation and bounded, read-back checked
//! copies. No installed destination, rename, process, receipt or public caller.
use super::*;
use std::io::{Seek, SeekFrom, Write};
use windows_sys::Win32::System::WindowsProgramming::FS_PERSISTENT_ACLS;

pub(super) struct Attempt {
    path: PathBuf,
    directory: File,
    _parents: Vec<File>,
}

fn descriptor(owner: &str) -> io::Result<SecurityAllocation> {
    let text = wide(OsStr::new(&format!(
        "O:{owner}D:P(A;OICI;GA;;;SY)(A;OICI;GA;;;{owner})"
    )));
    let mut raw = ptr::null_mut();
    check(unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            1,
            &mut raw,
            ptr::null_mut(),
        )
    })?;
    Ok(SecurityAllocation(raw))
}

impl Attempt {
    pub(super) fn create(parent: &Path, name: &str) -> io::Result<Self> {
        if name.len() != 40
            || !name.starts_with("attempt-")
            || !name[8..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(denied());
        }
        // The same local absolute-path restrictions as the image fixture.
        let parts: Vec<_> = parent.components().collect();
        if !matches!(parts.first(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_)))
            || parts.get(1) != Some(&Component::RootDir)
            || parts.len() < 3
            || parts[2..].iter().any(|part| !matches!(part, Component::Normal(name) if !name.encode_wide().any(|c| matches!(c, 0 | 58))))
        {
            return Err(denied());
        }
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        let mut current = PathBuf::new();
        let mut parents = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            current.push(part.as_os_str());
            if index >= 1 {
                parents.push(open_pinned(&current, true)?);
            }
        }
        private_owner_dacl(parents.last().ok_or_else(denied)?, &owner)?;
        let parent_handle = parents.last().ok_or_else(denied)?;
        let canonical = final_path(parent_handle)?;
        let drive = match canonical.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
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
        let mut flags = 0;
        check(unsafe {
            GetVolumeInformationByHandleW(
                parent_handle.as_raw_handle(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut flags,
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            )
        })?;
        if flags & FS_PERSISTENT_ACLS == 0
            || !filesystem.starts_with(&[b'N' as u16, b'T' as u16, b'F' as u16, b'S' as u16, 0])
        {
            return Err(denied());
        }
        let path = parent.join(name);
        let security = descriptor(&owner)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.0,
            bInheritHandle: 0,
        };
        // ACL is present at creation, never create-then-chmod. An existing
        // directory is refused without opening, repairing or deleting it.
        check(unsafe { CreateDirectoryW(wide(path.as_os_str()).as_ptr(), &attributes) })?;
        let directory = open_pinned(&path, true)?;
        private_owner_dacl(&directory, &owner)?;
        Ok(Self {
            path,
            directory,
            _parents: parents,
        })
    }

    pub(super) fn copy(&self, source: &File, expected: [u8; 32]) -> io::Result<File> {
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        private_owner_dacl(&self.directory, &owner)?;
        if identity(&open_pinned(&self.path, true)?)? != identity(&self.directory)? {
            return Err(denied());
        }
        // Reopen a read-only, non-write/delete-sharing handle with its own
        // offset. The caller's handle is not sought or treated as a pathname.
        let mut input = open_pinned(&final_path(source)?, false)?;
        if identity(&input)? != identity(source)? {
            return Err(denied());
        }
        private_owner_dacl(&input, &owner)?;
        let size = input.metadata().map_err(|_| denied())?.len();
        if size == 0 || size > IMAGE_LIMIT {
            return Err(denied());
        }
        let security = descriptor(&owner)?;
        let path = self.path.join("candidate.exe");
        // CREATE_NEW, non-inherited, no write/delete sharing. This handle is
        // retained through verification; nothing is published or launched here.
        let attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.0,
            bInheritHandle: 0,
        };
        let raw = unsafe {
            CreateFileW(
                wide(path.as_os_str()).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ,
                &attributes,
                CREATE_NEW,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(denied());
        }
        let mut output = unsafe { File::from_raw_handle(raw) };
        let mut hash = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = input.read(&mut buffer).map_err(|_| denied())?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > IMAGE_LIMIT {
                return Err(denied());
            }
            hash.update(&buffer[..count]);
            output.write_all(&buffer[..count]).map_err(|_| denied())?;
        }
        if total != size || <[u8; 32]>::from(hash.finalize()) != expected {
            return Err(denied());
        }
        output.sync_all().map_err(|_| denied())?;
        output.seek(SeekFrom::Start(0)).map_err(|_| denied())?;
        if digest(&output)? != expected {
            return Err(denied());
        }
        private_owner_dacl(&output, &owner)?;
        let (_, _, links, flags) = identity(&output)?;
        if links != 1 || flags & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            return Err(denied());
        }
        // Reduce access on the very same kernel object, with no pathname
        // close/reopen gap. The caller cannot modify the verified copy.
        output.seek(SeekFrom::Start(0)).map_err(|_| denied())?;
        let mut readonly = ptr::null_mut();
        check(unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                output.as_raw_handle(),
                GetCurrentProcess(),
                &mut readonly,
                FILE_GENERIC_READ,
                0,
                0,
            )
        })?;
        let readonly = unsafe { File::from_raw_handle(readonly) };
        if identity(&readonly)? != identity(&output)? {
            return Err(denied());
        }
        Ok(readonly)
    }
}
