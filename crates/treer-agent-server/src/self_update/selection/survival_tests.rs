//! Ordinary parent-exit acceptance, not a service-manager/cgroup guarantee.
use super::*;
use crate::self_update::{
    activator::{ActivatorImage, RunningActivator},
    handoff::{AuthenticatedPeer, Endpoint},
    images::digest_file,
    publication::Publications,
    Journal,
};
use std::{
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use treer_platform::process_image::ProcessImage;

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "isolated handoff fixture deadline"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn context() -> (PathBuf, PathBuf, String) {
    (
        std::env::var_os("TREER_TEST_CONFIG").unwrap().into(),
        std::env::var_os("TREER_TEST_JOBS").unwrap().into(),
        std::env::var("TREER_TEST_JOB").unwrap(),
    )
}

#[test]
fn detached_owner_retains_both_workspace_locks_after_parent_exit() {
    let f = super::tests::Fixture::new();
    f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
    f.enrollment("srv_b", "ws_b", &f.bin.join("treer-agent-server"));
    let own = ProcessImage::capture(std::process::id()).unwrap();
    let digest = digest_file(own.executable()).unwrap();
    std::fs::copy(
        std::env::current_exe().unwrap(),
        f.bin.join("treer-agent-server"),
    )
    .unwrap();
    std::fs::set_permissions(
        f.bin.join("treer-agent-server"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    // Short, physical, private path also works with Darwin's sockaddr_un limit.
    let jobs = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("ts-{}", uuid::Uuid::new_v4().simple()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&jobs)
        .unwrap();
    struct Jobs(PathBuf);
    impl Drop for Jobs {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let jobs = Jobs(jobs);
    let root = PrivateDirectory::open(&jobs.0).unwrap();
    let mut receipt = crate::self_update::tests::receipt();
    receipt.workspace_id = "ws_a".into();
    receipt.server_id = "srv_a".into();
    receipt.publications = Publications::new(
        digest,
        "0".repeat(64),
        &receipt.controller_sha256,
        &receipt.cli_sha256,
    );
    let journal = Journal::create(&root, &receipt).unwrap();
    let job = jobs.0.join(receipt.job_id.to_string());
    // Even on an outer assertion failure, request worker release. Its own
    // deadline also bounds its lifetime; never signal an unowned/reused PID.
    struct Release(PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.join("release"), b"release");
            if self.0.join("ready").exists() {
                let deadline = Instant::now() + Duration::from_secs(10);
                while !self.0.join("done").exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
    let _release = Release(job.clone());
    let mut parent = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "self_update::selection::survival_tests::parent",
                "--ignored",
            ])
            .env_clear()
            .env("TREER_TEST_CONFIG", f.path.join("config"))
            .env("TREER_TEST_JOBS", &jobs.0)
            .env("TREER_TEST_JOB", receipt.job_id.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| parent.0.try_wait().unwrap().is_some());
    assert!(parent.0.wait().unwrap().success(), "handoff parent failed");
    assert!(job.join("ready").exists());
    let prepared = journal.read().unwrap();
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(job.join("ownership.json")).unwrap()).unwrap();
    let pid = u32::try_from(stored["pid"].as_u64().unwrap()).unwrap();
    let worker = ProcessImage::capture(pid).unwrap();
    assert_eq!(stored["creation"], worker.creation_identity());
    assert_eq!(stored["prepared"]["job_id"], receipt.job_id.to_string());
    assert_eq!(stored["prepared"], serde_json::to_value(&prepared).unwrap());
    worker.verify_private_session().unwrap();
    let image = journal.directory.read_file("activator").unwrap();
    assert!(worker.matches_file(&image).unwrap());
    assert_eq!(
        digest_file(&image).unwrap(),
        digest_file(worker.executable()).unwrap()
    );
    let inventory = f.inventory();
    let a = inventory.select(Some("ws_a"), true).unwrap();
    let b = inventory.select(Some("ws_b"), true).unwrap();
    assert!(a.lock().is_err());
    assert!(b.lock().is_err());
    // Ask the surviving worker to check its proof and retained locks once more
    // after the parent is definitely reaped, not merely after its original ACK.
    std::fs::write(job.join("probe"), b"probe").unwrap();
    wait_for(|| job.join("probed").exists());
    worker.verify_current().unwrap();
    assert!(a.lock().is_err());
    assert!(b.lock().is_err());
    std::fs::write(job.join("release"), b"release").unwrap();
    wait_for(|| job.join("done").exists());
    wait_for(|| worker.verify_current().is_err());
    wait_for(|| a.lock().is_ok());
    assert!(b.lock().is_ok());
    assert_eq!(prepared.phase, crate::self_update::Phase::Prepared);
    assert_eq!(journal.read().unwrap(), prepared);
}

#[test]
#[ignore = "isolated submitting parent, invoked by parent-exit acceptance"]
fn parent() {
    let (config, jobs, id) = context();
    let inventory = Inventory::load(&config, "fixture-host").unwrap();
    let selected = inventory.select(Some("ws_a"), true).unwrap();
    let held = selected.lock().unwrap();
    let root = PrivateDirectory::open(&jobs).unwrap();
    let journal = Journal::open(&root, &id).unwrap();
    let job = jobs.join(&id);
    let image = ActivatorImage::prepare(&selected, &held, &journal).unwrap();
    Endpoint::mark_prepared(&journal, &job).unwrap();
    let endpoint = Endpoint::bind(&journal, &job).unwrap();
    let own = ProcessImage::capture(std::process::id()).unwrap();
    let child = OwnedChild(
        Command::new(job.join("activator"))
            .args([
                "--exact",
                "self_update::selection::survival_tests::worker",
                "--ignored",
            ])
            .env_clear()
            .env("TREER_TEST_CONFIG", config)
            .env("TREER_TEST_JOBS", &jobs)
            .env("TREER_TEST_JOB", id)
            .env("TREER_TEST_PARENT", own.pid().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    // Startup hashing is separate from the bounded framed handoff exchange.
    wait_for(|| job.join("bootstrapped").exists());
    let process = ProcessImage::capture(child.0.id()).unwrap();
    let mut peer = None;
    wait_for(|| {
        peer = endpoint
            .accept(&process, image.image(), image.digest())
            .unwrap();
        peer.is_some()
    });
    std::fs::write(job.join("parent-authed"), b"ready").unwrap();
    wait_for(|| job.join("worker-authed").exists());
    peer.unwrap()
        .offer_ownership(&journal, &selected, &held)
        .unwrap();
    wait_for(|| job.join("ready").exists());
    // Deliberately bypass destructors: OS process exit must close all original
    // descriptors without releasing the worker's transferred OFD locks.
    std::process::exit(0);
}

#[test]
#[ignore = "isolated private-image worker, invoked by submitting parent"]
fn worker() {
    fn assert_null(descriptor: impl std::os::fd::AsFd) {
        use std::os::unix::fs::MetadataExt;
        let file = std::fs::File::from(descriptor.as_fd().try_clone_to_owned().unwrap());
        let actual = file.metadata().unwrap();
        let null = std::fs::metadata("/dev/null").unwrap();
        assert_eq!(
            (actual.dev(), actual.ino(), actual.rdev()),
            (null.dev(), null.ino(), null.rdev())
        );
    }
    assert_null(std::io::stdin());
    assert_null(std::io::stdout());
    assert_null(std::io::stderr());
    let (config, jobs, id) = context();
    let inventory = Inventory::load(&config, "fixture-host").unwrap();
    let selected = inventory.select(Some("ws_a"), true).unwrap();
    let root = PrivateDirectory::open(&jobs).unwrap();
    let journal = Journal::open(&root, &id).unwrap();
    let job = jobs.join(&id);
    let running = RunningActivator::enter(&selected, &journal).unwrap();
    std::fs::write(job.join("bootstrapped"), b"ready").unwrap();
    let parent =
        ProcessImage::capture(std::env::var("TREER_TEST_PARENT").unwrap().parse().unwrap())
            .unwrap();
    let digest = journal
        .read()
        .unwrap()
        .publications
        .original_digest(Binary::Controller)
        .to_owned();
    let peer = AuthenticatedPeer::authenticate(
        journal.directory.connect_handoff(&job).unwrap(),
        &parent,
        parent.executable(),
        &digest,
    )
    .unwrap();
    std::fs::write(job.join("worker-authed"), b"ready").unwrap();
    wait_for(|| job.join("parent-authed").exists());
    let owned = peer.take_ownership(&journal, &selected).unwrap();
    std::fs::write(job.join("ready"), b"ready").unwrap();
    wait_for(|| {
        if job.join("release").exists() {
            return true;
        }
        if job.join("probe").exists() && !job.join("probed").exists() {
            running.verify(&selected, &journal).unwrap();
            owned
                .locks()
                .directory(&selected, Binary::Controller)
                .unwrap();
            assert_eq!(owned.prepared(), &journal.read().unwrap());
            std::fs::write(job.join("probed"), b"ready").unwrap();
        }
        false
    });
    drop(owned);
    std::fs::write(job.join("done"), b"done").unwrap();
}
