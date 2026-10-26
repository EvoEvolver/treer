//! Read-only process observation, NOT worker ownership or mapped-image
//! attestation. A reported executable pathname can outlive a file replacement.
//! No spawn, signal, Job escape, restart, resume or serialized-owner constructor.
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io,
    os::windows::{
        ffi::OsStringExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, OwnedHandle},
    },
    path::PathBuf,
};
use windows_sys::Win32::{
    Foundation::{WAIT_FAILED, WAIT_TIMEOUT},
    Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ},
    System::{
        JobObjects::IsProcessInJob,
        Threading::{
            QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE,
        },
    },
};

fn refused() -> io::Error {
    io::Error::other("windows_update_process_unconfirmed")
}

/// Read-only check of the ORIGINAL handle in a caller-owned Child. This never
/// opens a PID or creates lifecycle authority. A successful instantaneous check
/// is not a promise that another same-account actor cannot assign a Job later.
/// No breakaway flags or enclosing Job policy are changed here.
pub fn require_created_child_outside_jobs(child: &std::process::Child) -> io::Result<()> {
    let handle = child.as_raw_handle();
    // A process with application-selected exit code 259 is still exited.
    if unsafe { WaitForSingleObject(handle, 0) } != WAIT_TIMEOUT {
        return Err(refused());
    }
    let mut in_job = 0;
    if unsafe { IsProcessInJob(handle, std::ptr::null_mut(), &mut in_job) } == 0
        || in_job != 0
        || unsafe { WaitForSingleObject(handle, 0) } != WAIT_TIMEOUT
    {
        return Err(refused());
    }
    Ok(())
}

fn live(handle: &OwnedHandle) -> io::Result<()> {
    // Unlike STILL_ACTIVE alone this also refuses an exited process whose
    // application-selected exit code happens to be 259.
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
        WAIT_TIMEOUT => Ok(()),
        WAIT_FAILED => Err(io::Error::last_os_error()),
        _ => Err(refused()),
    }
}

fn reported_file(handle: &OwnedHandle) -> io::Result<File> {
    let mut path = vec![0u16; 32768];
    let mut length = path.len() as u32;
    if unsafe {
        QueryFullProcessImageNameW(handle.as_raw_handle(), 0, path.as_mut_ptr(), &mut length)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if length == 0 || length as usize >= path.len() {
        return Err(refused());
    }
    let path = PathBuf::from(OsString::from_wide(&path[..length as usize]));
    if !path.is_absolute() {
        return Err(refused());
    }
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(refused());
    }
    let (_, id, _, attributes) = crate::transfer_file_identity(&file)?;
    if id == 0
        || attributes & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(refused());
    }
    Ok(file)
}

fn file_id(file: &File) -> io::Result<(u64, u64)> {
    crate::transfer_file_identity(file).map(|(volume, id, _, _)| (volume, id))
}

/// Retains one query+synchronize process handle and a read-only image-path file
/// handle. No Debug/Serialize: paths and process data are not diagnostics. This
/// does NOT establish a parent/child relationship, authorize termination or
/// prove that the opened pathname is the process's originally mapped file.
pub struct ProcessObservation {
    process: OwnedHandle,
    file: File,
    pid: u32,
    birth: u64,
}

impl ProcessObservation {
    pub fn capture(pid: u32) -> io::Result<Self> {
        if pid == 0 {
            return Err(refused());
        }
        let process = crate::windows::open_process(
            pid,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
        )?
        .ok_or_else(refused)?;
        live(&process)?;
        let birth = crate::windows::creation_time(&process)?.ok_or_else(refused)?;
        if birth == 0 {
            return Err(refused());
        }
        let file = reported_file(&process)?;
        let observed = Self {
            process,
            file,
            pid,
            birth,
        };
        observed.verify_current()?;
        Ok(observed)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn creation_time(&self) -> u64 {
        self.birth
    }

    pub fn verify_current(&self) -> io::Result<()> {
        live(&self.process)?;
        if crate::windows::creation_time(&self.process)? != Some(self.birth)
            || file_id(&reported_file(&self.process)?)? != file_id(&self.file)?
        {
            return Err(refused());
        }
        live(&self.process)
    }

    pub fn reported_file_matches(&self, expected: &File) -> io::Result<bool> {
        self.verify_current()?;
        let matches = file_id(expected)? == file_id(&self.file)?;
        self.verify_current()?;
        Ok(matches)
    }

    /// Any-Job membership, queried on the SAME live process handle; not a Job
    /// identity/owner or proof of escape from a particular enclosing Job.
    pub fn in_any_job(&self) -> io::Result<bool> {
        self.verify_current()?;
        let mut result = 0;
        if unsafe {
            IsProcessInJob(
                self.process.as_raw_handle(),
                std::ptr::null_mut(),
                &mut result,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        self.verify_current()?;
        Ok(result != 0)
    }

    pub fn require_outside_jobs(&self) -> io::Result<()> {
        if self.in_any_job()? {
            return Err(refused());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        process::{Child, Command, Stdio},
    };

    struct OwnedFixture(Child);
    impl Drop for OwnedFixture {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn child() -> OwnedFixture {
        OwnedFixture(
            Command::new(std::env::current_exe().unwrap())
                .env_clear()
                .args([
                    "--exact",
                    "windows_update_process::tests::process_fixture_child",
                    "--ignored",
                    "--nocapture",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    #[test]
    #[ignore = "test-owned child; executed only through focused parent control"]
    fn process_fixture_child() {
        let mut input = [0u8];
        std::io::stdin().read_exact(&mut input).unwrap();
        // Deliberately test the STILL_ACTIVE collision on process exit.
        std::process::exit(259);
    }

    #[test]
    fn windows_update_process_current_identity_and_reported_file_are_read_only() {
        let observed = ProcessObservation::capture(std::process::id()).unwrap();
        assert_eq!(observed.pid(), std::process::id());
        assert!(observed.creation_time() > 0);
        observed.verify_current().unwrap();
        let executable = File::open(std::env::current_exe().unwrap()).unwrap();
        assert!(observed.reported_file_matches(&executable).unwrap());
        let path = std::env::temp_dir().join(format!(
            "treer-process-file-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let unrelated = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        assert!(!observed.reported_file_matches(&unrelated).unwrap());
        drop(unrelated);
        std::fs::remove_file(path).unwrap();
        assert!(ProcessObservation::capture(0).is_err());
        let in_job = observed.in_any_job().unwrap();
        assert_eq!(observed.require_outside_jobs().is_err(), in_job);
    }

    #[test]
    fn windows_update_process_held_child_handle_refuses_after_exit_259() {
        let mut child = child();
        let observed = ProcessObservation::capture(child.0.id()).unwrap();
        let mut stdin = child.0.stdin.take().unwrap();
        stdin.write_all(b"x").unwrap();
        drop(stdin);
        assert_eq!(child.0.wait().unwrap().code(), Some(259));
        assert!(observed.verify_current().is_err());
        assert!(observed.in_any_job().is_err());
    }

    #[test]
    fn windows_update_process_job_bound_owned_child_never_claims_escape() {
        let mut child = child();
        let _job = crate::ProcessTreeGuard::for_process(child.0.id())
            .unwrap()
            .unwrap();
        let observed = ProcessObservation::capture(child.0.id()).unwrap();
        assert!(observed.in_any_job().unwrap());
        assert!(observed.require_outside_jobs().is_err());
        child.0.stdin.take().unwrap().write_all(b"x").unwrap();
        assert_eq!(child.0.wait().unwrap().code(), Some(259));
    }
}
