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

// Fixed stage plus numeric code only. Zero denotes a semantic refusal or an
// upstream verifier that intentionally erased the OS code; never GetLastError
// after unrelated checks, and never format the error or private record bytes.
fn failure(stage: &'static str, error: io::Error) -> io::Error {
    eprintln!(
        "record_fixture stage={stage} win32={}",
        error.raw_os_error().unwrap_or(0)
    );
    denied()
}

fn at<T>(stage: &'static str, result: io::Result<T>) -> io::Result<T> {
    result.map_err(|error| failure(stage, error))
}

fn read_record(mut file: &File) -> io::Result<Vec<u8>> {
    let (_, _, links, flags) = at("read_identity", identity(file))?;
    if links != 1 || flags & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return Err(failure("read_type_or_links", denied()));
    }
    at("read_seek", file.seek(SeekFrom::Start(0)))?;
    let mut bytes = Vec::new();
    at(
        "read_bytes",
        file.take((RECORD_LIMIT + 1) as u64).read_to_end(&mut bytes),
    )?;
    if bytes.is_empty() || bytes.len() > RECORD_LIMIT {
        return Err(failure("read_bound", denied()));
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
            return Err(failure("input_bound", denied()));
        }
        let owner = at("current_owner", crate::windows::current_user_sid())?;
        at(
            "directory_acl",
            protected_private_acl(&self.directory, &owner),
        )?;
        let current = at("directory_open", open_pinned(&self.path, true))?;
        if at("directory_current_identity", identity(&current))?
            != at("directory_retained_identity", identity(&self.directory))?
        {
            return Err(failure("directory_binding", denied()));
        }
        let directory = at("directory_path", final_path(&self.directory))?;
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
                    at("prior_current_acl", protected_private_acl(&actual, &owner))?;
                    at(
                        "prior_expected_acl",
                        protected_private_acl(expected, &owner),
                    )?;
                    if at("prior_current_identity", identity(&actual))?
                        != at("prior_expected_identity", identity(expected))?
                        || at("prior_current_bytes", read_record(&actual))?
                            != at("prior_expected_bytes", read_record(expected))?
                    {
                        return Err(failure("prior_mismatch", denied()));
                    }
                    Ok(())
                }
                (Err(error), _) => Err(failure("prior_open", error)),
                (Ok(_), None) => Err(failure("unexpected_prior", denied())),
            }
        };
        at("prior_before_stage", validate_previous())?;
        let security = at("stage_descriptor", descriptor(&owner))?;
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
            let error = io::Error::last_os_error();
            return Err(failure("stage_create", error));
        }
        let mut staged = unsafe { File::from_raw_handle(raw) };
        at("stage_write", staged.write_all(bytes))?;
        at("stage_sync", staged.sync_all())?;
        at("stage_acl", protected_private_acl(&staged, &owner))?;
        if at("stage_readback", read_record(&staged))? != bytes {
            return Err(failure("stage_bytes_mismatch", denied()));
        }
        let staged_identity = at("stage_identity", identity(&staged))?;
        if fault == RecordFault::BeforePublish {
            return Err(failure("injected_before_publish", denied()));
        }
        at("prior_before_publish", validate_previous())?;
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
            let error = io::Error::last_os_error();
            return Err(failure("rename_ex", error));
        }
        if fault == RecordFault::AfterPublish {
            return Err(failure("injected_after_publish", denied()));
        }
        if at("published_identity", identity(&staged))? != staged_identity
            || at("published_path", final_path(&staged))? != target
        {
            return Err(failure("published_binding", denied()));
        }
        at("published_acl", protected_private_acl(&staged, &owner))?;
        if at("published_readback", read_record(&staged))? != bytes {
            return Err(failure("published_bytes_mismatch", denied()));
        }
        // Close the write FILE_OBJECT before returning a fresh read pin. A
        // reduced DuplicateHandle would still retain its write-sharing record.
        drop(staged);
        let installed = at("installed_open", open())?;
        if at("installed_identity", identity(&installed))? != staged_identity {
            return Err(failure("installed_identity_mismatch", denied()));
        }
        at("installed_acl", protected_private_acl(&installed, &owner))?;
        if at("installed_readback", read_record(&installed))? != bytes {
            return Err(failure("installed_bytes_mismatch", denied()));
        }
        Ok(installed)
    }
}
