use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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
    path::{Path, PathBuf},
    ptr,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::*,
    System::{JobObjects::*, SystemInformation::OSVERSIONINFOW, Threading::*},
};
#[cfg(not(test))]
use windows_sys::Win32::{
    System::Com::CoTaskMemFree,
    UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, KF_FLAG_DONT_VERIFY},
};

const CHILD_MS: u64 = 10_000;
const WAIT_MS: u32 = 5_000;
const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
const FLAGS: [u32; 2] = [
    CREATE_BREAKAWAY_FROM_JOB | CREATE_UNICODE_ENVIRONMENT,
    CREATE_BREAKAWAY_FROM_JOB
        | DETACHED_PROCESS
        | CREATE_NEW_PROCESS_GROUP
        | CREATE_UNICODE_ENVIRONMENT,
];

// RtlGetVersion reports the OS version without GetVersionEx's manifest filtering.
#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlGetVersion(info: *mut OSVERSIONINFOW) -> i32;
}

#[derive(Debug)]
struct Failure {
    stage: &'static str,
    win32: Option<i32>,
}
type Result<T> = std::result::Result<T, Failure>;

fn failed(stage: &'static str) -> Failure {
    Failure { stage, win32: None }
}
fn os_error(stage: &'static str) -> Failure {
    Failure {
        stage,
        win32: io::Error::last_os_error().raw_os_error(),
    }
}
fn checked(value: i32, stage: &'static str) -> Result<()> {
    if value == 0 {
        Err(os_error(stage))
    } else {
        Ok(())
    }
}
fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}
fn in_job(handle: HANDLE) -> Result<bool> {
    let mut member = 0;
    // SAFETY: a valid process handle and a writable BOOL; NULL means any Job.
    checked(
        unsafe { IsProcessInJob(handle, ptr::null_mut(), &mut member) },
        "job_query",
    )?;
    Ok(member != 0)
}
fn alive(handle: HANDLE) -> Result<()> {
    // SAFETY: retained process handle, zero timeout, no borrowed output memory.
    match unsafe { WaitForSingleObject(handle, 0) } {
        WAIT_TIMEOUT => Ok(()),
        WAIT_OBJECT_0 => Err(failed("child_exited_early")),
        _ => Err(os_error("child_wait")),
    }
}

struct Image {
    path: PathBuf,
    _file: File,
    _directories: Vec<File>,
    identity: (u32, u64),
    size: u64,
    digest: String,
}

fn checked_file(path: &Path, directory: bool) -> Result<(File, (u32, u64))> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(if directory {
            FILE_SHARE_READ | FILE_SHARE_WRITE
        } else {
            FILE_SHARE_READ
        })
        .custom_flags(
            FILE_FLAG_OPEN_REPARSE_POINT
                | if directory {
                    FILE_FLAG_BACKUP_SEMANTICS
                } else {
                    0
                },
        )
        .open(path)
        .map_err(|e| Failure {
            stage: "image_open",
            win32: e.raw_os_error(),
        })?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { mem::zeroed() };
    checked(
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) },
        "image_identity",
    )?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
        || (!directory && info.nNumberOfLinks != 1)
    {
        return Err(failed("image_reparse_or_alias"));
    }
    Ok((
        file,
        (
            info.dwVolumeSerialNumber,
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        ),
    ))
}

fn final_path(file: &File) -> Result<PathBuf> {
    let mut buffer = vec![0u16; 32_768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    };
    if length == 0 {
        return Err(os_error("image_final_path"));
    }
    if length as usize >= buffer.len() {
        return Err(failed("image_final_path"));
    }
    Ok(PathBuf::from(OsString::from_wide(
        &buffer[..length as usize],
    )))
}

#[cfg(not(test))]
fn approved_directory() -> Result<PathBuf> {
    // Native Known Folder lookup, never trust workload LOCALAPPDATA overrides.
    let mut raw = ptr::null_mut();
    let status = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_LocalAppData,
            KF_FLAG_DONT_VERIFY as u32,
            ptr::null_mut(),
            &mut raw,
        )
    };
    if status < 0 || raw.is_null() {
        if !raw.is_null() {
            unsafe {
                CoTaskMemFree(raw.cast());
            }
        }
        return Err(failed("approved_directory"));
    }
    // SAFETY: successful API returns a CoTaskMem-owned NUL-terminated string.
    let mut length = 0;
    unsafe {
        while *raw.add(length) != 0 {
            length += 1;
        }
    }
    let path = PathBuf::from(OsString::from_wide(unsafe {
        std::slice::from_raw_parts(raw, length)
    }));
    unsafe {
        CoTaskMemFree(raw.cast());
    }
    Ok(path.join("Treer").join("self-update-probe-111"))
}

impl Image {
    fn open() -> Result<Self> {
        let path = std::env::current_exe().map_err(|_| failed("self_path"))?;
        if !path.is_absolute() || path.as_os_str().encode_wide().any(|c| c == 0 || c == 34) {
            return Err(failed("self_path"));
        }
        // Deny concurrent write/delete of our own image throughout both attempts.
        #[cfg(not(test))]
        let approved = approved_directory()?;
        #[cfg(test)]
        let approved = path
            .parent()
            .ok_or_else(|| failed("self_path"))?
            .to_path_buf();
        // Retain every physical ancestor without delete sharing. Reject junctions
        // in Treer or LocalAppData too, not just at the final directory entry.
        let mut directories = Vec::new();
        for ancestor in approved.ancestors().collect::<Vec<_>>().into_iter().rev() {
            directories.push(checked_file(ancestor, true)?.0);
        }
        let approved = final_path(
            directories
                .last()
                .ok_or_else(|| failed("approved_directory"))?,
        )?;
        let (mut file, identity) = checked_file(&path, false)?;
        let path = final_path(&file)?;
        if path.parent() != Some(approved.as_path()) {
            return Err(failed("unapproved_directory"));
        }
        let size = file.metadata().map_err(|_| failed("self_metadata"))?.len();
        if size == 0 || size > MAX_IMAGE_BYTES {
            return Err(failed("self_size"));
        }
        let mut hash = Sha256::new();
        let mut bytes = [0u8; 16 * 1024];
        let mut total = 0u64;
        loop {
            let n = file.read(&mut bytes).map_err(|_| failed("self_hash"))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > size {
                return Err(failed("self_changed"));
            }
            hash.update(&bytes[..n]);
        }
        if total != size {
            return Err(failed("self_changed"));
        }
        Ok(Self {
            path,
            _file: file,
            _directories: directories,
            identity,
            size,
            digest: format!("{:x}", hash.finalize()),
        })
    }
}

struct Child {
    process: OwnedHandle,
    pid: u32,
    cleaned: bool,
    cleanup_attempted: bool,
}
impl Child {
    fn spawn(image: &Image, flags: u32) -> Result<Self> {
        let application = wide(image.path.as_os_str());
        let mut args = OsString::from("\"");
        args.push(&image.path);
        #[cfg(not(test))]
        args.push("\" --probe-child");
        #[cfg(test)]
        args.push("\" --ignored --exact windows::tests::sleeper");
        let mut command = wide(&args);
        // Explicit empty Unicode block, never copy workload environment values.
        let environment = [0u16; 2];
        let cwd = wide(
            image
                .path
                .parent()
                .ok_or_else(|| failed("self_path"))?
                .as_os_str(),
        );
        // SAFETY: initialized structs and live NUL-terminated input buffers. The
        // mutable command buffer is private; FALSE forbids handle inheritance.
        let mut startup: STARTUPINFOW = unsafe { mem::zeroed() };
        startup.cb = mem::size_of::<STARTUPINFOW>() as u32;
        let mut info: PROCESS_INFORMATION = unsafe { mem::zeroed() };
        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                flags,
                environment.as_ptr().cast(),
                cwd.as_ptr(),
                &startup,
                &mut info,
            )
        };
        if created == 0 {
            let error = os_error("child_create");
            return Err(if error.win32 == Some(ERROR_ACCESS_DENIED as i32) {
                Failure {
                    stage: "breakaway_denied",
                    win32: error.win32,
                }
            } else {
                error
            });
        }
        // SAFETY: successful CreateProcess transfers these two valid owned handles.
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
        let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
        drop(thread);
        Ok(Self {
            process,
            pid: info.dwProcessId,
            cleaned: false,
            cleanup_attempted: false,
        })
    }
    fn handle(&self) -> HANDLE {
        self.process.as_raw_handle()
    }
    fn inspect(&self, image: &Image) -> Result<(u64, bool)> {
        alive(self.handle())?;
        let mut path = vec![0u16; 32_768];
        let mut length = path.len() as u32;
        // SAFETY: owned process handle and a writable bounded UTF-16 buffer.
        checked(
            unsafe { QueryFullProcessImageNameW(self.handle(), 0, path.as_mut_ptr(), &mut length) },
            "child_image_query",
        )?;
        let path = PathBuf::from(OsString::from_wide(&path[..length as usize]));
        // Win32 may report the same DOS path with or without the verbatim prefix.
        let expected: Vec<_> = image.path.as_os_str().encode_wide().collect();
        let actual: Vec<_> = path.as_os_str().encode_wide().collect();
        let prefix = [92, 92, 63, 92];
        let expected = expected.strip_prefix(&prefix).unwrap_or(&expected);
        let actual = actual.strip_prefix(&prefix).unwrap_or(&actual);
        if actual != expected {
            return Err(failed("child_image_mismatch"));
        }
        let (child_file, identity) = checked_file(&path, false)?;
        if identity != image.identity || final_path(&child_file)? != image.path {
            return Err(failed("child_image_mismatch"));
        }
        // Only our pinned self-image is read, never a path supplied by the child.
        let again = Image::open()?;
        if again.size != image.size || again.digest != image.digest || again.path != image.path {
            return Err(failed("self_changed"));
        }
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: owned process handle and four writable FILETIME values.
        checked(
            unsafe {
                GetProcessTimes(
                    self.handle(),
                    &mut created,
                    &mut exited,
                    &mut kernel,
                    &mut user,
                )
            },
            "child_creation_query",
        )?;
        let membership = in_job(self.handle())?;
        alive(self.handle())?;
        Ok((
            (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
            membership,
        ))
    }
    fn cleanup(&mut self) -> Result<()> {
        self.cleanup_attempted = true;
        // SAFETY: only the process handle returned by our successful CreateProcess.
        let wait = unsafe { WaitForSingleObject(self.handle(), 0) };
        if wait == WAIT_OBJECT_0 {
            self.cleaned = true;
            return Ok(());
        }
        if wait != WAIT_TIMEOUT {
            return Err(Failure {
                stage: "cleanup_unconfirmed",
                win32: io::Error::last_os_error().raw_os_error(),
            });
        }
        let terminated = unsafe { TerminateProcess(self.handle(), 1) };
        let error = if terminated == 0 {
            Some(Failure {
                stage: "cleanup_unconfirmed",
                win32: io::Error::last_os_error().raw_os_error(),
            })
        } else {
            None
        };
        // TerminateProcess is asynchronous. An exit race is safe only if waited.
        if unsafe { WaitForSingleObject(self.handle(), WAIT_MS) } != WAIT_OBJECT_0 {
            return Err(error.unwrap_or_else(|| failed("cleanup_unconfirmed")));
        }
        self.cleaned = true;
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(())
        }
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        if !self.cleanup_attempted {
            // Last-resort exact-handle termination only; reporting still fails.
            unsafe {
                TerminateProcess(self.handle(), 1);
            }
        }
    }
}

fn parent_job_details() -> Result<(u32, Vec<usize>)> {
    // NULL queries our current Job; it does not enumerate every ancestor Job.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
    checked(
        unsafe {
            QueryInformationJobObject(
                ptr::null_mut(),
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                mem::size_of_val(&limits) as u32,
                ptr::null_mut(),
            )
        },
        "parent_job_limits_unavailable",
    )?;
    #[repr(C)]
    struct Members {
        assigned: u32,
        count: u32,
        ids: [usize; 1024],
    }
    let mut members = Members {
        assigned: 0,
        count: 0,
        ids: [0; 1024],
    };
    checked(
        unsafe {
            QueryInformationJobObject(
                ptr::null_mut(),
                JobObjectBasicProcessIdList,
                (&mut members as *mut Members).cast(),
                mem::size_of_val(&members) as u32,
                ptr::null_mut(),
            )
        },
        "parent_job_members",
    )?;
    if members.count > 1024 || members.assigned != members.count {
        return Err(failed("parent_job_members_incomplete"));
    }
    Ok((
        limits.BasicLimitInformation.LimitFlags,
        members.ids[..members.count as usize].to_vec(),
    ))
}

fn probe(report: &mut Value) -> Result<()> {
    let mut version: OSVERSIONINFOW = unsafe { mem::zeroed() };
    version.dwOSVersionInfoSize = mem::size_of_val(&version) as u32;
    // SAFETY: initialized version structure with its required size.
    if unsafe { RtlGetVersion(&mut version) } != 0 {
        return Err(failed("windows_version"));
    }
    report["windows_build"] = json!(version.dwBuildNumber);
    let image = Image::open()?;
    report["helper_sha256"] = json!(image.digest);
    report["helper_size"] = json!(image.size);
    let parent_in_job = in_job(unsafe { GetCurrentProcess() })?;
    report["parent_in_job"] = json!(parent_in_job);
    if !parent_in_job {
        return Err(failed("not_exec_job_evidence"));
    }
    let (limits, _) = parent_job_details()?;
    report["parent_job_limit_flags"] = json!(limits);
    for flags in FLAGS {
        let mut attempt = json!({"flags":flags,"child_pid":null,"child_creation_time":null,
            "child_in_job":null,"child_in_parent_job":null,"cleanup_confirmed":false,
            "result":"failed","error_stage":null,"win32_error":null});
        let result = (|| -> Result<()> {
            let mut child = Child::spawn(&image, flags)?;
            attempt["child_pid"] = json!(child.pid);
            let observed = (|| -> Result<()> {
                let (creation, membership) = child.inspect(&image)?;
                attempt["child_creation_time"] = json!(creation);
                attempt["child_in_job"] = json!(membership);
                let (_, members) = parent_job_details()?;
                let listed = members.contains(&(child.pid as usize));
                attempt["child_in_parent_job"] = json!(listed);
                if membership || listed {
                    return Err(failed("child_still_in_job"));
                }
                Ok(())
            })();
            let cleanup = child.cleanup();
            attempt["cleanup_confirmed"] = json!(child.cleaned);
            cleanup?;
            observed
        })();
        match &result {
            Ok(()) => attempt["result"] = json!("breakaway_observed"),
            Err(e) => {
                attempt["error_stage"] = json!(e.stage);
                attempt["win32_error"] = json!(e.win32);
            }
        }
        report["attempts"]
            .as_array_mut()
            .expect("fixed report array")
            .push(attempt);
        // Fixed diagnostic sequence, never retry/change policy after a failure.
        result?;
    }
    Ok(())
}

pub(super) fn run() -> i32 {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args == [OsStr::new("--probe-child")] {
        std::thread::sleep(Duration::from_millis(CHILD_MS));
        return 0;
    }
    let mut report = super::report::empty(std::process::id());
    let result = if args == [OsStr::new("--probe")] {
        probe(&mut report)
    } else {
        Err(failed("invalid_arguments"))
    };
    let exit = match result {
        Ok(()) => {
            report["result"] = json!("breakaway_observed");
            0
        }
        Err(e) => {
            report["error_stage"] = json!(e.stage);
            report["win32_error"] = json!(e.win32);
            1
        }
    };
    // A closed stdout is a diagnostic failure, not a panic with raw stderr text.
    use std::io::Write;
    if !super::report::valid(&report) {
        return 1;
    }
    match serde_json::to_vec(&report) {
        Ok(mut bytes) if bytes.len() < 8192 => {
            bytes.push(b'\n');
            if io::stdout().lock().write_all(&bytes).is_ok() {
                exit
            } else {
                1
            }
        }
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "owned fixture child only"]
    fn sleeper() {
        std::thread::sleep(Duration::from_millis(CHILD_MS));
    }

    #[test]
    fn exited_child_is_not_live_evidence_and_cleanup_uses_owned_handle() {
        let image = Image::open().unwrap();
        // No breakaway request: this test works inside an ambient CI Job too.
        let mut child =
            Child::spawn(&image, DETACHED_PROCESS | CREATE_UNICODE_ENVIRONMENT).unwrap();
        child.cleanup().unwrap();
        assert!(child.cleaned);
        assert_eq!(
            child.inspect(&image).unwrap_err().stage,
            "child_exited_early"
        );
        child.cleanup().unwrap();
    }

    #[test]
    fn failed_create_returns_no_process_to_adopt() {
        let mut image = Image::open().unwrap();
        image.path.set_extension("nonexistent-probe-image");
        assert!(matches!(Child::spawn(&image, FLAGS[0]), Err(e) if e.stage == "child_create"));
    }

    #[test]
    #[ignore = "isolated native Job fixture only"]
    fn job_fixture() {
        let case = std::env::var("TREER_TEST_PROBE_JOB").unwrap();
        assert!(
            !in_job(unsafe { GetCurrentProcess() }).unwrap(),
            "fixture needs an origin outside ambient Jobs"
        );
        let _job = if case != "none" {
            // Only this isolated test process is assigned; production never calls
            // either SetInformationJobObject or AssignProcessToJobObject.
            let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            assert!(!raw.is_null());
            let job = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
            if case == "allow" {
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK;
            }
            checked(
                unsafe {
                    SetInformationJobObject(
                        job.as_raw_handle(),
                        JobObjectExtendedLimitInformation,
                        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        mem::size_of_val(&limits) as u32,
                    )
                },
                "fixture_job_limits",
            )
            .unwrap();
            checked(
                unsafe { AssignProcessToJobObject(job.as_raw_handle(), GetCurrentProcess()) },
                "fixture_assign",
            )
            .unwrap();
            Some(job)
        } else {
            None
        };
        let mut report = json!({"attempts":[]});
        let outcome = probe(&mut report);
        match case.as_str() {
            "none" => {
                assert_eq!(outcome.unwrap_err().stage, "not_exec_job_evidence");
                assert!(report["attempts"].as_array().unwrap().is_empty());
            }
            "allow" => {
                outcome.unwrap();
                let attempts = report["attempts"].as_array().unwrap();
                assert_eq!(attempts.len(), 2);
                for attempt in attempts {
                    assert_eq!(attempt["result"], "breakaway_observed");
                    assert_eq!(attempt["child_in_job"], false);
                    assert_eq!(attempt["child_in_parent_job"], false);
                    assert_eq!(attempt["cleanup_confirmed"], true);
                }
            }
            "deny" => {
                let error = outcome.unwrap_err();
                assert_eq!(error.stage, "breakaway_denied");
                assert_eq!(error.win32, Some(ERROR_ACCESS_DENIED as i32));
                assert_eq!(report["attempts"].as_array().unwrap().len(), 1);
                assert_eq!(report["attempts"][0]["child_pid"], Value::Null);
            }
            _ => panic!("unknown test fixture"),
        }
    }

    #[test]
    #[ignore = "run explicitly on a native Windows worker outside any ambient Job"]
    fn native_job_matrix() {
        use std::process::{Command, Stdio};
        for case in ["none", "allow", "deny"] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "windows::tests::job_fixture"])
                .env_clear()
                .env("TREER_TEST_PROBE_JOB", case)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let until = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "native Job fixture failed: {case}");
                    break;
                }
                if std::time::Instant::now() >= until {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("native Job fixture timed out: {case}");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
