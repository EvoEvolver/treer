//! Disposable native storage fixture. Opaque bounded bytes only: no receipt
//! schema, marker protocol, enrollment state, recovery entry or public caller.
use super::*;
use windows_sys::Win32::System::WindowsProgramming::{
    FILE_RENAME_FLAG_POSIX_SEMANTICS, FILE_RENAME_FLAG_REPLACE_IF_EXISTS,
};

const RECORD_LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordFault {
    None,
    BeforePublish,
    AfterPublish,
}

fn read_record(mut file: &File) -> io::Result<Vec<u8>> {
    let (_, _, links, flags) = identity(file)?;
    if links != 1 || flags & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return Err(denied());
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take((RECORD_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > RECORD_LIMIT {
        return Err(denied());
    }
    Ok(bytes)
}

impl Attempt {
    // expected_previous is an already retained record handle, never a path.
    // This check is not an atomic CAS against another writer; production needs
    // the installation lock and marker protocol. Failure never deletes files.
    pub(crate) fn replace_record(
        &self,
        bytes: &[u8],
        expected_previous: Option<&File>,
        fault: RecordFault,
    ) -> io::Result<File> {
        if bytes.is_empty() || bytes.len() > RECORD_LIMIT {
            return Err(denied());
        }
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        protected_private_acl(&self.directory, &owner)?;
        if identity(&open_pinned(&self.path, true)?)? != identity(&self.directory)? {
            return Err(denied());
        }
        let directory = final_path(&self.directory)?;
        let target = directory.join("receipt.json");
        let temporary = directory.join("receipt.next");
        let open = || {
            OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&target)
        };
        let validate_previous = || -> io::Result<()> {
            match (open(), expected_previous) {
                (Err(error), None) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                    Ok(())
                }
                (Ok(actual), Some(expected)) => {
                    protected_private_acl(&actual, &owner)?;
                    protected_private_acl(expected, &owner)?;
                    if identity(&actual)? != identity(expected)?
                        || read_record(&actual)? != read_record(expected)?
                    {
                        return Err(denied());
                    }
                    Ok(())
                }
                _ => Err(denied()),
            }
        };
        validate_previous()?;
        let security = descriptor(&owner)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.0,
            bInheritHandle: 0,
        };
        let raw = unsafe {
            CreateFileW(
                wide(temporary.as_os_str()).as_ptr(),
                GENERIC_READ | GENERIC_WRITE | DELETE,
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
        let mut staged = unsafe { File::from_raw_handle(raw) };
        staged.write_all(bytes)?;
        staged.sync_all()?;
        protected_private_acl(&staged, &owner)?;
        if read_record(&staged)? != bytes {
            return Err(denied());
        }
        let staged_identity = identity(&staged)?;
        if fault == RecordFault::BeforePublish {
            return Err(denied());
        }
        validate_previous()?;
        // Extended rename supplies the documented open-reader replacement
        // semantics. Ordinary FileRenameInfo failed with retained old readers
        // on hosted Server 2022/2025. No delete-first or legacy fallback.
        // A retained old reader must allow delete sharing; otherwise refusal
        // preserves the original target and complete staged evidence.
        let name = wide(target.as_os_str());
        let length = mem::offset_of!(FILE_RENAME_INFO, FileName) + name.len() * 2;
        let mut storage = vec![0usize; length.div_ceil(mem::size_of::<usize>())];
        let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        unsafe {
            (*info).Anonymous.Flags = if expected_previous.is_some() {
                FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS
            } else {
                0
            };
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
                staged.as_raw_handle(),
                FileRenameInfoEx,
                info.cast(),
                length as u32,
            )
        };
        if renamed == 0 {
            let code = unsafe { GetLastError() };
            eprintln!("record_fixture stage=rename_ex win32={code}");
            return Err(denied());
        }
        if fault == RecordFault::AfterPublish {
            return Err(denied());
        }
        if identity(&staged)? != staged_identity || final_path(&staged)? != target {
            return Err(denied());
        }
        protected_private_acl(&staged, &owner)?;
        if read_record(&staged)? != bytes {
            return Err(denied());
        }
        // Close the write FILE_OBJECT before returning a fresh read pin. A
        // reduced DuplicateHandle would still retain its write-sharing record.
        drop(staged);
        let installed = open()?;
        if identity(&installed)? != staged_identity {
            return Err(denied());
        }
        protected_private_acl(&installed, &owner)?;
        if read_record(&installed)? != bytes {
            return Err(denied());
        }
        Ok(installed)
    }
}
