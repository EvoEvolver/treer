//! Disposable native fixture only. No updater entry point or transaction work.
#![cfg(windows)]
#![allow(unsafe_code)]

use std::{
    ffi::OsStr,
    fs, mem,
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    ptr, thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::FILE_GENERIC_READ,
    System::{JobObjects::*, Pipes::*, SystemServices::JOB_OBJECT_QUERY, Threading::*},
};

const DEADLINE: Duration = Duration::from_secs(30);

fn check(value: i32) {
    assert_ne!(value, 0, "{}", std::io::Error::last_os_error());
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn member(process: HANDLE, job: HANDLE) -> bool {
    let mut yes = 0;
    check(unsafe { IsProcessInJob(process, job, &mut yes) });
    yes != 0
}

fn job(name: &Path) -> OwnedHandle {
    let raw = unsafe { CreateJobObjectW(ptr::null(), wide(name.as_os_str()).as_ptr()) };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    check(unsafe {
        SetInformationJobObject(
            raw,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            mem::size_of_val(&limits) as u32,
        )
    });
    // Deliberately inheritable sentinel: CreateProcess(false) must override it.
    check(unsafe {
        SetHandleInformation(
            handle.as_raw_handle(),
            HANDLE_FLAG_INHERIT,
            HANDLE_FLAG_INHERIT,
        )
    });
    handle
}

struct Child {
    process: OwnedHandle,
    thread: OwnedHandle,
}

impl Child {
    fn spawn(dir: &Path, role: &str, suspended: bool) -> Self {
        let exe = std::env::current_exe().unwrap();
        let mut argv = wide(OsStr::new(&format!(
            "\"{}\" --ignored --exact fixture_role --nocapture",
            exe.display()
        )));
        // Fixture-only inputs; production launch must have its own reviewed argv.
        let mut env = Vec::new();
        for (key, value) in [
            ("TREER_LIFETIME_DIR", dir.display().to_string()),
            ("TREER_LIFETIME_ROLE", role.to_owned()),
            ("TREER_LIFETIME_OBSERVER", observer().to_string()),
        ] {
            env.extend(format!("{key}={value}").encode_utf16());
            env.push(0);
        }
        env.push(0);
        let mut startup: STARTUPINFOW = unsafe { mem::zeroed() };
        startup.cb = mem::size_of_val(&startup) as u32;
        let mut info: PROCESS_INFORMATION = unsafe { mem::zeroed() };
        // No inherited Job, pipe, stdio or other handle; no breakaway flags.
        check(unsafe {
            CreateProcessW(
                wide(exe.as_os_str()).as_ptr(),
                argv.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_UNICODE_ENVIRONMENT
                    | CREATE_NO_WINDOW
                    | if suspended { CREATE_SUSPENDED } else { 0 },
                env.as_ptr().cast(),
                ptr::null(),
                &startup,
                &mut info,
            )
        });
        Self {
            process: unsafe { OwnedHandle::from_raw_handle(info.hProcess) },
            thread: unsafe { OwnedHandle::from_raw_handle(info.hThread) },
        }
    }

    fn resume(&self) {
        assert_ne!(
            unsafe { ResumeThread(self.thread.as_raw_handle()) },
            u32::MAX
        );
    }

    fn stop(&self) {
        if self.live() {
            check(unsafe { TerminateProcess(self.process.as_raw_handle(), 77) });
        }
        assert_eq!(
            unsafe { WaitForSingleObject(self.process.as_raw_handle(), 5_000) },
            WAIT_OBJECT_0
        );
    }

    fn live(&self) -> bool {
        assert_live(self.process.as_raw_handle())
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // Also bound failure-path fixture cleanup using the original owned handle.
        if unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } == WAIT_TIMEOUT {
            unsafe {
                TerminateProcess(self.process.as_raw_handle(), 78);
                WaitForSingleObject(self.process.as_raw_handle(), 5_000);
            }
        }
    }
}

fn assert_live(handle: HANDLE) -> bool {
    match unsafe { WaitForSingleObject(handle, 0) } {
        WAIT_TIMEOUT => true,
        WAIT_OBJECT_0 => false,
        _ => panic!("process wait failed: {}", std::io::Error::last_os_error()),
    }
}

fn observer() -> u32 {
    std::env::var("TREER_LIFETIME_OBSERVER")
        .map(|value| value.parse().unwrap())
        .unwrap_or_else(|_| std::process::id())
}

fn export(dir: &Path, name: &str, process: HANDLE) {
    // Instrumentation only: open the observer, NEVER a cleanup target by PID.
    // Transfer the actual CreateProcess handle into the outer observer's table.
    // No Job is exported. The observer may stop only these exact child handles.
    let raw = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, observer()) };
    assert!(!raw.is_null());
    let target = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut remote = ptr::null_mut();
    check(unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            process,
            target.as_raw_handle(),
            &mut remote,
            if name == "pipe" {
                FILE_GENERIC_READ
            } else {
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE
            },
            0,
            0,
        )
    });
    fs::write(
        dir.join(format!("{name}.tmp")),
        (remote as usize).to_string(),
    )
    .unwrap();
    fs::rename(dir.join(format!("{name}.tmp")), dir.join(name)).unwrap();
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + DEADLINE;
    while !condition() {
        assert!(Instant::now() < until, "fixture deadline exceeded");
        thread::sleep(Duration::from_millis(10));
    }
}

fn receive(dir: &Path, name: &str) -> OwnedHandle {
    wait_for(|| dir.join(name).exists());
    let raw: usize = fs::read_to_string(dir.join(name)).unwrap().parse().unwrap();
    assert_ne!(raw, 0);
    // Only a handle duplicated directly into this process by our owned fixture.
    unsafe { OwnedHandle::from_raw_handle(raw as HANDLE) }
}

fn marker(dir: &Path, name: &str) {
    fs::write(dir.join(name), b"ready").unwrap();
}

fn query_membership(dir: &Path, name: &str, expected: bool) {
    let name = wide(OsStr::new(&format!(
        "Local\\Treer111-{}-{name}",
        dir.file_name().unwrap().to_string_lossy()
    )));
    let raw = unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, 0, name.as_ptr()) };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    assert_eq!(
        member(unsafe { GetCurrentProcess() }, handle.as_raw_handle()),
        expected
    );
    // Query handle closes here, before the Host-death phase.
}

fn named_job(dir: &Path, name: &str) -> OwnedHandle {
    job(Path::new(&format!(
        "Local\\Treer111-{}-{name}",
        dir.file_name().unwrap().to_string_lossy()
    )))
}

#[test]
#[ignore = "private child mode for native_host_job_lifetime only"]
fn fixture_role() {
    let dir = PathBuf::from(std::env::var_os("TREER_LIFETIME_DIR").unwrap());
    match std::env::var("TREER_LIFETIME_ROLE").unwrap().as_str() {
        "host" => {
            assert!(!member(unsafe { GetCurrentProcess() }, ptr::null_mut()));
            let root = named_job(&dir, "root");
            check(unsafe { AssignProcessToJobObject(root.as_raw_handle(), GetCurrentProcess()) });
            // Empty sibling PTY Job: supervisor must never be assigned to it.
            let _pty = named_job(&dir, "pty");
            let controller = Child::spawn(&dir, "controller", false);
            wait_for(|| dir.join("restart").exists());
            // Mirrors Host RestartController: direct child kill/wait, no tree kill.
            controller.stop();
            let _replacement = Child::spawn(&dir, "replacement", false);
            marker(&dir, "restarted");
            thread::sleep(DEADLINE);
        }
        "controller" => {
            let mut read = ptr::null_mut();
            let mut write = ptr::null_mut();
            let security = SECURITY_ATTRIBUTES {
                nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: ptr::null_mut(),
                bInheritHandle: 1,
            };
            check(unsafe { CreatePipe(&mut read, &mut write, &security, 0) });
            let read = unsafe { OwnedHandle::from_raw_handle(read) };
            let _write = unsafe { OwnedHandle::from_raw_handle(write) };
            export(&dir, "pipe", read.as_raw_handle());
            let stdio = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .share_mode(0)
                .open(dir.join("stdio-sentinel"))
                .unwrap();
            check(unsafe {
                SetHandleInformation(
                    stdio.as_raw_handle(),
                    HANDLE_FLAG_INHERIT,
                    HANDLE_FLAG_INHERIT,
                )
            });
            let _supervisor = Child::spawn(&dir, "supervisor", false);
            export(&dir, "supervisor", _supervisor.process.as_raw_handle());
            thread::sleep(DEADLINE);
        }
        "replacement" => {
            query_membership(&dir, "root", true);
            marker(&dir, "replacement-ready");
            thread::sleep(DEADLINE);
        }
        "supervisor" => {
            query_membership(&dir, "root", true);
            query_membership(&dir, "pty", false);
            let installer_job = named_job(&dir, "installer");
            let installer = Child::spawn(&dir, "installer", true);
            check(unsafe {
                AssignProcessToJobObject(
                    installer_job.as_raw_handle(),
                    installer.process.as_raw_handle(),
                )
            });
            assert!(member(
                installer.process.as_raw_handle(),
                installer_job.as_raw_handle()
            ));
            export(&dir, "installer", installer.process.as_raw_handle());
            installer.resume();
            marker(&dir, "supervisor-ready");
            thread::sleep(DEADLINE);
        }
        "installer" => {
            query_membership(&dir, "root", true);
            query_membership(&dir, "installer", true);
            let until = Instant::now() + DEADLINE;
            let mut sequence = 0u64;
            while Instant::now() < until {
                sequence += 1;
                fs::write(dir.join("writer"), sequence.to_string()).unwrap();
                thread::sleep(Duration::from_millis(10));
            }
        }
        _ => panic!("unknown fixture role"),
    }
}

#[test]
#[ignore = "disposable native Windows worker outside every ambient Job; never win_native"]
fn native_host_job_lifetime() {
    assert!(
        !member(unsafe { GetCurrentProcess() }, ptr::null_mut()),
        "observer must be outside ambient Jobs"
    );
    for end in ["host", "supervisor"] {
        lifetime_case(end);
    }
}

fn lifetime_case(end: &str) {
    let dir = std::env::temp_dir().join(format!(
        "treer111-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&dir).unwrap();
    let host = Child::spawn(&dir, "host", false);
    let supervisor = receive(&dir, "supervisor");
    let installer = receive(&dir, "installer");
    let pipe = receive(&dir, "pipe");
    wait_for(|| dir.join("supervisor-ready").exists() && dir.join("writer").exists());
    marker(&dir, "restart");
    wait_for(|| dir.join("restarted").exists() && dir.join("replacement-ready").exists());
    assert!(assert_live(supervisor.as_raw_handle()));
    assert!(assert_live(installer.as_raw_handle()));
    // The only original writer was in Controller; an inherited writer would
    // keep this pipe open while the supervisor is still alive.
    assert_eq!(
        unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        },
        0
    );
    assert_eq!(unsafe { GetLastError() }, ERROR_BROKEN_PIPE);
    // An inheritable file handle likewise must not outlive Controller.
    drop(
        fs::OpenOptions::new()
            .write(true)
            .share_mode(0)
            .open(dir.join("stdio-sentinel"))
            .unwrap(),
    );
    let before = fs::read(dir.join("writer")).unwrap();
    wait_for(|| fs::read(dir.join("writer")).is_ok_and(|now| !now.is_empty() && now != before));
    if end == "host" {
        host.stop();
    } else {
        check(unsafe { TerminateProcess(supervisor.as_raw_handle(), 79) });
    }
    for handle in [&supervisor, &installer] {
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle(), 5_000) },
            WAIT_OBJECT_0,
            "closing Host/root or supervisor/installer Job must terminate the owned descendants"
        );
    }
    // No writer remains after the exact installer handle has signalled.
    let after = fs::read(dir.join("writer")).unwrap();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(after, fs::read(dir.join("writer")).unwrap());
    if end == "supervisor" {
        assert!(
            host.live(),
            "installer containment must not depend on Host death"
        );
        host.stop();
    }
    for entry in fs::read_dir(&dir).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    fs::remove_dir(dir).unwrap();
}
