// Real bootstrap/descriptor handoff with private copied native test images.
// No installation, networking service, account or manager is created.
fn wait_fixture(mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "worker fixture deadline"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

pub(super) fn bootstrap_checkpoint(stage: &str) {
    let Some(root) = std::env::var_os("TREER_TEST_WORKER_JOB") else {
        return;
    };
    let root = PathBuf::from(root);
    std::fs::write(root.join(format!("test-worker-{stage}")), b"ready").unwrap();
    if stage == "authenticated" {
        // Keep full debug-image hashing outside the bounded framed exchange.
        wait_fixture(|| root.join("test-parent-authenticated").exists());
    }
    if std::env::var("TREER_TEST_WORKER_STAGE").as_deref() == Ok(stage) {
        loop {
            std::thread::park();
        }
    }
}

#[test]
#[ignore = "private installed-parent fixture invoked by sigkill_worker_bootstrap_keeps_claim"]
fn bootstrap_death_parent() {
    use crate::self_update::{activator::ActivatorImage, handoff::Endpoint};
    use treer_platform::process_image::ProcessImage;
    let config = PathBuf::from(std::env::var_os("TREER_TEST_WORKER_CONFIG").unwrap());
    let root_path = PathBuf::from(std::env::var_os("TREER_TEST_WORKER_ROOT").unwrap());
    let id = std::env::var("TREER_TEST_WORKER_ID").unwrap();
    let stage = std::env::var("TREER_TEST_WORKER_STAGE").unwrap();
    let inventory = Inventory::load(&config, "fixture-host").unwrap();
    let selected = inventory.select(Some("ws_a"), true).unwrap();
    let held = selected.lock().unwrap();
    let root = PrivateDirectory::open(&root_path).unwrap();
    let journal = Journal::open(&root, &id).unwrap();
    let job = root_path.join(&id);
    let activator = ActivatorImage::prepare(&selected, &held, &journal).unwrap();
    Endpoint::mark_prepared(&journal, &job).unwrap();
    crate::self_update::handoff::parent::save(&journal, &selected, &held).unwrap();
    let endpoint = Endpoint::bind(&journal, &job).unwrap();
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = OwnedChild(
        std::process::Command::new(job.join("activator"))
            .args([
                "--exact",
                "self_update::worker::tests::bootstrap_death_worker",
                "--ignored",
            ])
            .env_clear()
            .env("TREER_TEST_WORKER_CONFIG", &config)
            .env("TREER_TEST_WORKER_ROOT", &root_path)
            .env("TREER_TEST_WORKER_ID", &id)
            .env("TREER_TEST_WORKER_JOB", &job)
            .env("TREER_TEST_WORKER_STAGE", &stage)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(job.join("test-worker-stderr")).unwrap())
            .spawn()
            .unwrap(),
    );
    wait_fixture(|| {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "worker exited during image bootstrap"
        );
        job.join("test-worker-running").exists()
    });
    if stage != "running" {
        let process = ProcessImage::capture(child.0.id()).unwrap();
        process.verify_private_session().unwrap();
        let mut peer = None;
        wait_fixture(|| {
            assert!(child.0.try_wait().unwrap().is_none());
            peer = endpoint
                .accept(&process, activator.image(), activator.digest())
                .unwrap();
            peer.is_some()
        });
        wait_fixture(|| job.join("test-worker-authenticated").exists());
        std::fs::write(job.join("test-parent-authenticated"), b"ready").unwrap();
        if matches!(stage.as_str(), "owned" | "core_refused") {
            peer.unwrap()
                .offer_ownership(&journal, &selected, &held)
                .unwrap();
            wait_fixture(|| {
                assert!(child.0.try_wait().unwrap().is_none());
                job.join(format!("test-worker-{stage}")).exists()
            });
        }
    }
    let receipt = std::fs::read(job.join("receipt.json")).unwrap();
    let claim = std::fs::read(root_path.join("web-active.json")).unwrap();
    let ownership = std::fs::read(job.join("ownership.json")).ok();
    assert_eq!(
        ownership.is_some(),
        matches!(stage.as_str(), "owned" | "core_refused")
    );
    if let Some(bytes) = &ownership {
        let value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(value["pid"], child.0.id());
        assert!(!value["creation"].as_str().unwrap().is_empty());
    }
    drop(held);
    if stage == "owned" {
        assert!(selected.lock().is_err());
    }
    child.0.kill().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(child.0.wait().unwrap().signal(), Some(libc::SIGKILL));
    // Kernel lock release does not close the durable machine-update duty.
    assert!(selected.lock().is_ok());
    for _ in 0..2 {
        let reopened = Journal::open(&root, &id).unwrap();
        let operation = crate::self_update::web_adapter::read(&reopened, "ws_a", "srv_a").unwrap();
        assert_eq!(
            operation.state,
            treer_protocol::controller_update::State::Updating
        );
        assert!(
            WorkerResources::open_at(&root_path, &id, || Ok(Inventory::load(
                &config,
                "fixture-host"
            )
            .unwrap()))
            .is_ok()
        );
        // Resource lookup is read-only, not authority to reenter/replay a job.
        let error = crate::self_update::web_adapter::tests::legacy_fixture_guard(
            &inventory,
            selected.controller_path(),
            &selected.cli_path(),
            &root_path,
        )
        .err()
        .expect("claim must block an explicit legacy update");
        assert_eq!(error.to_string(), "update_recovery_required");
        assert_eq!(std::fs::read(job.join("receipt.json")).unwrap(), receipt);
        assert_eq!(
            std::fs::read(root_path.join("web-active.json")).unwrap(),
            claim
        );
        assert_eq!(std::fs::read(job.join("ownership.json")).ok(), ownership);
        for name in [
            "launcher-install-intent.json",
            "launcher-status-intent.json",
            "web-released.json",
        ] {
            assert!(!job.join(name).exists());
        }
    }
}

#[test]
#[ignore = "private activator fixture invoked by bootstrap_death_parent"]
fn bootstrap_death_worker() {
    let config = PathBuf::from(std::env::var_os("TREER_TEST_WORKER_CONFIG").unwrap());
    let root = PathBuf::from(std::env::var_os("TREER_TEST_WORKER_ROOT").unwrap());
    let id = std::env::var("TREER_TEST_WORKER_ID").unwrap();
    let resources =
        WorkerResources::open_at(&root, &id, || Inventory::load(&config, "fixture-host")).unwrap();
    let mut owned = resources.enter().unwrap(); // Actual authenticated SAME-lock transfer.
    assert_eq!(owned.progress(), CoreProgress::RecoveryRequired);
    if std::env::var("TREER_TEST_WORKER_STAGE").as_deref() == Ok("core_refused") {
        // Exercise the real Live engine with missing baseline, never a Fake
        // engine or a fabricated CoreHealthy receipt/launcher permission.
        let before = owned.journal.read().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(
            runtime.block_on(owned.begin_core().unwrap().run()),
            CoreProgress::RecoveryRequired
        );
        assert_eq!(owned.journal.read().unwrap(), before);
        assert!(owned.selected.lock().is_err());
        assert!(owned.begin_core().is_err());
        assert_eq!(
            runtime
                .block_on(owned.finish_launcher())
                .unwrap_err()
                .to_string(),
            "update_recovery_required"
        );
        bootstrap_checkpoint("core_refused");
    }
    bootstrap_checkpoint("owned");
    panic!("fixture did not stop at selected boundary");
}

#[test]
fn sigkill_worker_bootstrap_keeps_claim() {
    use crate::self_update::{images::digest_file, publication::Publications};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let f = Fixture::new();
    let installed = f.bin.join("treer-agent-server");
    std::fs::copy(std::env::current_exe().unwrap(), &installed).unwrap();
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.enrollment("srv_a", "ws_a", &installed);
    let digest = digest_file(&std::fs::File::open(&installed).unwrap()).unwrap();
    for stage in ["running", "authenticated", "owned", "core_refused"] {
        // Short physical path for Darwin sockaddr_un; never an enrolled state root.
        let path = std::fs::canonicalize("/tmp")
            .unwrap()
            .join(format!("tw-{}", uuid::Uuid::new_v4().simple()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        let root = PrivateDirectory::open(&path).unwrap();
        let (mut receipt, mut binding) = crate::self_update::web_adapter::tests::bound();
        receipt.workspace_id = "ws_a".into();
        receipt.server_id = "srv_a".into();
        binding.workspace_id = "ws_a".into();
        binding.server_id = "srv_a".into();
        receipt.publications = Publications::new(
            digest.clone(),
            "0".repeat(64),
            &receipt.controller_sha256,
            &receipt.cli_sha256,
        );
        binding.current.controller_sha256 = digest.clone();
        let journal = Journal::create(&root, &receipt).unwrap();
        use std::io::Write;
        for (directory, name) in [
            (&root, "web-active.json"),
            (&journal.directory, crate::self_update::web_adapter::BINDING),
        ] {
            let mut file = directory.create_file(name).unwrap();
            file.write_all(&serde_json::to_vec(&binding).unwrap())
                .unwrap();
            file.sync_all().unwrap();
            directory.sync().unwrap();
        }
        let output = std::process::Command::new(&installed)
            .args([
                "--exact",
                "self_update::worker::tests::bootstrap_death_parent",
                "--ignored",
                "--nocapture",
            ])
            .env_clear()
            .env("TREER_TEST_WORKER_CONFIG", f.path.join("config"))
            .env("TREER_TEST_WORKER_ROOT", &path)
            .env("TREER_TEST_WORKER_ID", receipt.job_id.to_string())
            .env("TREER_TEST_WORKER_STAGE", stage)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "bootstrap {stage}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::remove_dir_all(&path).unwrap();
    }
}
