// Runs only inert native images in private fixture directories. This exercises
// the real owned launcher executor, not a Controller replacement or health proof.
#[tokio::test(flavor = "current_thread")]
#[ignore = "subprocess fixture selected by sigkill_owned_launcher_windows_preserve_intent"]
async fn launcher_death_child() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::standard_config();
    let source = f.path.join("inert-launcher.rs");
    std::fs::write(
        &source,
        r#"fn main() {
        use std::io::Write;
        assert_eq!(std::env::args().nth(1).as_deref(), Some("launcher"));
        let mut count = std::fs::OpenOptions::new().create(true).append(true)
            .open("launch-count").unwrap();
        count.write_all(b"ran").unwrap(); count.sync_all().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(700));
        println!("{{}}");
    }"#,
    )
    .unwrap();
    assert!(
        std::process::Command::new(std::env::var_os("TREER_TEST_RUSTC").unwrap())
            .arg(&source)
            .arg("-o")
            .arg(f.bin.join(Binary::Controller.name()))
            .status()
            .unwrap()
            .success()
    );
    let image = std::fs::read(f.bin.join(Binary::Controller.name())).unwrap();
    for name in [Binary::Controller.name(), Binary::Cli.name()] {
        std::fs::set_permissions(f.bin.join(name), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
    let selected = f.inventory().select(None, false).unwrap();
    let locks = selected.lock().unwrap();
    let mut r = receipt();
    r.workspace_id = "ws_a".into();
    r.server_id = "srv_a".into();
    r.controller_sha256 = format!("{:x}", Sha256::digest(&image));
    r.cli_sha256 = format!("{:x}", Sha256::digest(b"cli"));
    r.publications = Publications::new(
        r.controller_sha256.clone(),
        r.cli_sha256.clone(),
        &r.controller_sha256,
        &r.cli_sha256,
    );
    let jobs = PrivateDirectory::open(&f.path)
        .unwrap()
        .create_child("jobs")
        .unwrap();
    let journal = Journal::create(&jobs, &r).unwrap();
    let prepared = journal.prepare_fixture().unwrap();
    let activating = journal
        .advance(prepared.revision, Phase::Activating)
        .unwrap();
    let before = journal
        .advance(activating.revision, Phase::CoreHealthy)
        .unwrap();
    let owned = OwnedActivation::fixture(locks, prepared);
    let temporary = LauncherTemporary::create(
        &selected,
        &owned,
        &journal,
        &f.path.join("jobs").join(before.job_id.to_string()),
    )
    .unwrap();
    let node = NodeFixture::new();
    let plan = Invocation::with_context(
        &selected,
        &owned,
        &journal,
        Operation::Install,
        &f.path,
        &node.node(),
    )
    .unwrap()
    .bind_node(retain_node(&node.node()).unwrap())
    .unwrap()
    .bind_temporary(&temporary)
    .unwrap();
    let info = serde_json::json!({"root": f.path, "node": node.0, "job": before.job_id});
    std::fs::write(
        std::env::var_os("TREER_TEST_LAUNCHER_DEATH_INFO").unwrap(),
        serde_json::to_vec(&info).unwrap(),
    )
    .unwrap();
    super::super::worker::launcher::death_checkpoint("before_intent");
    let output = super::super::worker::launcher::execute(&plan)
        .await
        .unwrap();
    assert!(output.status().success());
    super::super::worker::launcher::death_checkpoint("after_install");
    panic!("fixture did not reach requested death boundary");
}

#[test]
fn sigkill_owned_launcher_windows_preserve_intent() {
    use std::os::unix::{fs::DirBuilderExt, process::ExitStatusExt};
    let sysroot = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .unwrap();
    assert!(sysroot.status.success());
    let compiler =
        PathBuf::from(String::from_utf8(sysroot.stdout).unwrap().trim()).join("bin/rustc");
    for stage in [
        "before_intent",
        "intent",
        "spawned",
        "child_record",
        "after_install",
    ] {
        let control = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("treer-launcher-death-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&control)
            .unwrap();
        let ready = control.join("ready");
        let info_path = control.join("info.json");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "self_update::launcher_invocation::tests::launcher_death_child",
                "--ignored",
            ])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &control)
            .env("TREER_TEST_RUSTC", &compiler)
            .env("TREER_TEST_LAUNCHER_DEATH_STAGE", stage)
            .env("TREER_TEST_LAUNCHER_DEATH_READY", &ready)
            .env("TREER_TEST_LAUNCHER_DEATH_INFO", &info_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(control.join("child.log")).unwrap())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !ready.exists() {
            if child.try_wait().unwrap().is_some() || std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "owned launcher fixture failed before {stage}: {}",
                    std::fs::read_to_string(control.join("child.log")).unwrap()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let info: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&info_path).unwrap()).unwrap();
        let root = PathBuf::from(info["root"].as_str().unwrap());
        let id = info["job"].as_str().unwrap();
        let job = root.join("jobs").join(id);
        let before = std::fs::read(job.join("receipt.json")).unwrap();
        let intent = std::fs::read(job.join("launcher-install-intent.json")).ok();
        let record = std::fs::read(job.join("launcher-install-child.json")).ok();
        assert_eq!(intent.is_some(), stage != "before_intent");
        assert_eq!(
            record.is_some(),
            matches!(stage, "child_record" | "after_install")
        );
        child.kill().unwrap(); // No Rust Drop or async cancellation cleanup.
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
        // The inert child has a bounded 700ms lifetime on platforms without
        // PDEATHSIG. We do not treat this wait as real installer quiescence.
        std::thread::sleep(std::time::Duration::from_millis(1000));
        let count_path = root.join("bin/launch-count");
        let count = std::fs::read(&count_path).unwrap_or_default();
        match stage {
            "after_install" => assert_eq!(count, b"ran"),
            "spawned" | "child_record" => assert!(count.is_empty() || count == b"ran"),
            _ => assert!(count.is_empty()),
        }
        if let Some(bytes) = &record {
            let recorded: serde_json::Value = serde_json::from_slice(bytes).unwrap();
            assert_eq!(recorded["parent_pid"], child.id());
            assert!(recorded["pid"].as_u64().unwrap() > 1);
            assert!(!recorded["creation"].as_str().unwrap().is_empty());
        }
        let jobs = PrivateDirectory::open(&root.join("jobs")).unwrap();
        for _ in 0..2 {
            let reopened = Journal::open(&jobs, id).unwrap();
            assert_eq!(reopened.read().unwrap().phase, Phase::CoreHealthy);
            assert_eq!(std::fs::read(job.join("receipt.json")).unwrap(), before);
            assert_eq!(
                std::fs::read(job.join("launcher-install-intent.json")).ok(),
                intent
            );
            assert_eq!(
                std::fs::read(job.join("launcher-install-child.json")).ok(),
                record
            );
            assert!(!job.join("launcher-status-intent.json").exists());
            assert!(!job.join("web-released.json").exists());
            assert_eq!(std::fs::read(&count_path).unwrap_or_default(), count);
        }
        // Observational recovery never rebuilds OwnedActivation from a receipt
        // or starts a status/install child. Existing exclusive-intent tests
        // cover explicit repeated execute refusal with the original owner.
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(info["node"].as_str().unwrap()).unwrap();
        std::fs::remove_dir_all(control).unwrap();
    }
}
