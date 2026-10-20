//! W1a native fixture: private attempt creation and bounded, read-back checked
//! copies, W1b fixed-slot rename and opaque-record replacement fixtures.
//! No installed destination, process, production journal or public caller.
use super::*;
use std::io::{Seek, SeekFrom, Write};
use windows_sys::Win32::System::WindowsProgramming::FS_PERSISTENT_ACLS;

mod evidence_file;
mod head_record;
mod operation_outcome;
mod ownership;
mod record_file;
pub(super) use record_file::RecordFault;

pub(super) struct Attempt {
    path: PathBuf,
    directory: File,
    _parents: Vec<File>,
    evidence_used: std::cell::Cell<bool>,
}

#[derive(Clone, Copy)]
pub(super) enum Slot {
    Candidate,
    Active,
    Backup,
}

// Test-only snapshot of verified private objects, not a serializable receipt.
// It grants no authority and does not survive loss of directory/lock ownership.
pub(super) struct RecordedImage {
    identity: (u32, u64, u32, u32),
    size: u64,
    hash: [u8; 32],
}

fn protected_private_acl(file: &File, owner: &str) -> io::Result<()> {
    private_owner_dacl(file, owner)?;
    let mut acl = ptr::null_mut();
    let mut raw = ptr::null_mut();
    let result = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut acl,
            ptr::null_mut(),
            &mut raw,
        )
    };
    let _allocation = SecurityAllocation(raw);
    if result != ERROR_SUCCESS || acl.is_null() || unsafe { IsValidAcl(acl) } == 0 {
        return Err(denied());
    }
    let mut control = 0;
    let mut revision = 0;
    check(unsafe { GetSecurityDescriptorControl(raw, &mut control, &mut revision) })?;
    if control & SE_DACL_PROTECTED == 0 {
        return Err(denied());
    }
    // This stricter private-attempt policy also refuses foreign read grants.
    for index in 0..u32::from(unsafe { (*acl).AceCount }) {
        let mut ace = ptr::null_mut();
        check(unsafe { GetAce(acl, index, &mut ace) })?;
        let header = unsafe { ptr::read_unaligned(ace.cast::<ACE_HEADER>()) };
        let offset = mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
        if !matches!(
            u32::from(header.AceType),
            ACCESS_ALLOWED_ACE_TYPE | ACCESS_DENIED_ACE_TYPE
        ) || usize::from(header.AceSize) < offset + 8
        {
            return Err(denied());
        }
        let sid = unsafe {
            ace.cast::<u8>()
                .add(mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart))
        };
        let sid_size = 8 + 4 * usize::from(unsafe { *sid.add(1) });
        if offset + sid_size > usize::from(header.AceSize) {
            return Err(denied());
        }
        let trustee = sid_text(sid.cast())?;
        if trustee != owner && trustee != "S-1-5-18" {
            return Err(denied());
        }
    }
    Ok(())
}

impl RecordedImage {
    pub(super) fn capture(file: &File, hash: [u8; 32]) -> io::Result<Self> {
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        protected_private_acl(file, &owner)?;
        let before = identity(file)?;
        if before.2 != 1
            || before.3 & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        {
            return Err(denied());
        }
        let size = file.metadata()?.len();
        let mut reader = file;
        reader.seek(SeekFrom::Start(0))?;
        if digest(file)? != hash || identity(file)? != before {
            return Err(denied());
        }
        Ok(Self {
            identity: before,
            size,
            hash,
        })
    }
}

impl Slot {
    fn name(self) -> &'static str {
        match self {
            Self::Candidate => "candidate.exe",
            Self::Active => "controller.exe",
            Self::Backup => "controller.prev.exe",
        }
    }
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
    // Only disposable fixture callers. Production observation still needs the
    // installation lock, attested quiescence and persisted record validation.
    // Returned labels are a momentary snapshot, never retained mutation pins.
    pub(super) fn observe(
        &self,
        old: &RecordedImage,
        new: &RecordedImage,
    ) -> [crate::windows_update_layout::Object; 3] {
        use crate::windows_update_layout::Object::*;
        let observe = || -> io::Result<_> {
            let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
            protected_private_acl(&self.directory, &owner)?;
            if identity(&open_pinned(&self.path, true)?)? != identity(&self.directory)? {
                return Err(denied());
            }
            if old.identity == new.identity {
                return Err(denied());
            }
            let directory = final_path(&self.directory)?;
            let mut slots = [Unreadable; 3];
            for (index, slot) in [Slot::Active, Slot::Candidate, Slot::Backup]
                .into_iter()
                .enumerate()
            {
                let path = directory.join(slot.name());
                // Preserve Win32 open errors: generic helpers deliberately
                // erase them, which would lose the absence/read-failure split.
                let file = match OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
                    .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(&path)
                {
                    Ok(file) => file,
                    Err(error) => {
                        slots[index] = if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32)
                        {
                            Absent
                        } else {
                            Unreadable
                        };
                        continue;
                    }
                };
                let verify = || -> io::Result<_> {
                    protected_private_acl(&file, &owner)?;
                    let id = identity(&file)?;
                    if id.2 != 1
                        || id.3 & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
                        || final_path(&file)? != path
                    {
                        return Ok(Unknown);
                    }
                    let size = file.metadata()?.len();
                    let hash = digest(&file)?;
                    if identity(&file)? != id {
                        return Ok(Unknown);
                    }
                    let matches = |record: &RecordedImage| {
                        id == record.identity && size == record.size && hash == record.hash
                    };
                    Ok(if matches(old) {
                        Old
                    } else if matches(new) {
                        New
                    } else {
                        Unknown
                    })
                };
                slots[index] = verify().unwrap_or(Unreadable);
            }
            Ok(slots)
        };
        observe().unwrap_or([Unreadable; 3])
    }

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
            evidence_used: std::cell::Cell::new(false),
        })
    }

    pub(super) fn copy(&self, source: &File, expected: [u8; 32]) -> io::Result<File> {
        self.copy_inner(source, expected, false)
    }

    // Only the disposable rename fixture gets DELETE access, on the same
    // checked object. The W1a read-only copy contract remains unchanged.
    pub(super) fn copy_for_rename(&self, source: &File, expected: [u8; 32]) -> io::Result<File> {
        self.copy_inner(source, expected, true)
    }

    fn copy_inner(&self, source: &File, expected: [u8; 32], rename: bool) -> io::Result<File> {
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
                GENERIC_READ | GENERIC_WRITE | if rename { DELETE } else { 0 },
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
                FILE_GENERIC_READ | if rename { DELETE } else { 0 },
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

    // Move only one of this attempt's fixed slots, by the verified handle.
    // Never replace an existing name. No installed path, process or receipt is
    // involved; a sequence of these calls is NOT a crash-atomic transaction.
    pub(super) fn rename_checked(
        &self,
        file: &File,
        from: Slot,
        to: Slot,
        expected: [u8; 32],
    ) -> io::Result<()> {
        self.rename_captured(file, from, to, expected, |_| Ok(()))
            .map_err(|error| match error {
                operation_outcome::RenameError::Before(error)
                | operation_outcome::RenameError::Uncertain(error) => error,
                operation_outcome::RenameError::Native(_) => {
                    io::Error::new(io::ErrorKind::PermissionDenied, "rename_native_call")
                }
            })
    }

    fn rename_captured(
        &self,
        file: &File,
        from: Slot,
        to: Slot,
        expected: [u8; 32],
        mut after_call: impl FnMut(bool) -> io::Result<()>,
    ) -> Result<(), operation_outcome::RenameError> {
        use operation_outcome::RenameError;
        // Native fixture diagnostics are fixed stage codes, never paths, bytes
        // or OS error strings. This remains test-only, not a public status API.
        let refused = |stage: &'static str| io::Error::new(io::ErrorKind::PermissionDenied, stage);
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        private_owner_dacl(&self.directory, &owner).map_err(|_| refused("rename_directory_acl"))?;
        private_owner_dacl(file, &owner).map_err(|_| refused("rename_file_acl"))?;
        let directory =
            final_path(&self.directory).map_err(|_| refused("rename_directory_path"))?;
        let before = identity(file).map_err(|_| refused("rename_source_identity"))?;
        if final_path(file).map_err(|_| refused("rename_source_path"))?
            != directory.join(from.name())
            || before.2 != 1
            || before.3 & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        {
            return Err(refused("rename_source_slot_or_attributes").into());
        }
        let mut reader = file;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|_| refused("rename_seek_before_hash"))?;
        let actual = digest(file).map_err(|_| refused("rename_source_digest"))?;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|_| refused("rename_seek_after_hash"))?;
        if actual != expected {
            return Err(refused("rename_digest_mismatch").into());
        }
        // Use the Win32 full-path form with a NULL RootDirectory. The relative
        // form was rejected with ERROR_INVALID_PARAMETER on hosted 2022/2025.
        // This path comes only from the retained directory's final path plus a
        // fixed slot. The directory and all ancestors remain pinned without
        // delete sharing, so their names cannot be redirected during this call.
        // No cwd/caller path or native-API fallback is involved. usize storage
        // supplies struct alignment.
        let name = wide(directory.join(to.name()).as_os_str());
        let bytes = mem::offset_of!(FILE_RENAME_INFO, FileName) + name.len() * 2;
        let mut storage = vec![0usize; bytes.div_ceil(mem::size_of::<usize>())];
        let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        unsafe {
            (*info).Anonymous.ReplaceIfExists = false;
            (*info).RootDirectory = ptr::null_mut();
            (*info).FileNameLength = ((name.len() - 1) * 2) as u32;
            ptr::copy_nonoverlapping(
                name.as_ptr(),
                ptr::addr_of_mut!((*info).FileName).cast(),
                name.len(),
            );
        }
        let renamed = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileRenameInfo,
                info.cast(),
                bytes as u32,
            )
        };
        if renamed == 0 {
            // Numeric Win32 status cannot contain private data. Capture it
            // immediately, before any other API call overwrites last-error.
            let code = unsafe { GetLastError() };
            eprintln!("rename_fixture stage=set_file_information win32={code}");
            after_call(false).map_err(RenameError::Uncertain)?;
            return Err(RenameError::Native(code));
        }
        after_call(true).map_err(RenameError::Uncertain)?;
        if identity(file).map_err(|_| RenameError::Uncertain(refused("rename_target_identity")))?
            != before
            || final_path(file)
                .map_err(|_| RenameError::Uncertain(refused("rename_target_path")))?
                != directory.join(to.name())
        {
            return Err(RenameError::Uncertain(refused("rename_postcheck")));
        }
        Ok(())
    }
}
