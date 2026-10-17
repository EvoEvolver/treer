//! Fixed launcher invocation data and a bounded interpreter-version probe,
//! NOT installer execution authority. The executor still needs retained image
//! proofs, durable child-start/identity, strict completion and final live health.
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Component, Path, PathBuf},
};

use anyhow::{bail, Result};
use treer_platform::installation_directory::InstallationDirectory;
use treer_platform::retained_executable::RetainedExecutable;

use super::{
    artifacts::Binary, handoff::OwnedActivation, launcher_preflight,
    launcher_temporary::LauncherTemporary, selection::Selected, Journal, Receipt,
};

mod start;
pub(super) use start::RegistrationGate;
pub use start::{RecordedIntent, RegisteredSupervisor};

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Install,
    Status,
}

/// Borrows the original owner so installation/enrollment locks cannot be
/// released while this recipe exists. Intentionally not Debug/Serialize.
pub struct Invocation<'a> {
    selected: &'a Selected,
    owned: &'a OwnedActivation,
    journal: &'a Journal,
    receipt: Receipt,
    operation: Operation,
    program: PathBuf,
    cwd: PathBuf,
    arguments: Vec<OsString>,
    environment: BTreeMap<OsString, OsString>,
    temporary: Option<&'a LauncherTemporary<'a>>,
    node: Option<RetainedExecutable>,
}

impl<'a> Invocation<'a> {
    /// Resolve trusted Node from the launcher's fixed OS locations, never PATH
    /// or an Agent override. HOME comes from the effective OS account database;
    /// the selected enrollment supplies XDG_CONFIG_HOME even for custom roots.
    /// This does not run Node or authenticate an installed Controller image.
    pub fn prepare(
        selected: &'a Selected,
        owned: &'a OwnedActivation,
        journal: &'a Journal,
        temporary: &'a LauncherTemporary<'a>,
        operation: Operation,
    ) -> Result<Self> {
        let receipt = launcher_preflight::checked(selected, owned, journal)?;
        temporary.verify_for(selected, owned, journal, &receipt)?;
        let home = nix::unistd::User::from_uid(nix::unistd::geteuid())
            .ok()
            .flatten()
            .and_then(|user| user.dir.canonicalize().ok())
            .ok_or_else(|| anyhow::anyhow!("update_launcher_account_home_unavailable"))?;
        InstallationDirectory::open(&home)
            .map_err(|_| anyhow::anyhow!("update_launcher_account_home_unavailable"))?;
        let node = crate::launcher::node_executable(None)
            .map_err(|_| anyhow::anyhow!("update_launcher_node_untrusted_location"))?;
        let node = retain_node(&node)?;
        Self::with_context(
            selected,
            owned,
            journal,
            operation,
            &home,
            node.executable(),
        )?
        .bind_node(node)?
        .bind_temporary(temporary)
    }

    // Private deterministic data-builder seam: production callers cannot
    // substitute an ambient home, Node path, environment, or arbitrary argv.
    fn with_context(
        selected: &'a Selected,
        owned: &'a OwnedActivation,
        journal: &'a Journal,
        operation: Operation,
        home: &Path,
        node: &Path,
    ) -> Result<Self> {
        let receipt = launcher_preflight::checked(selected, owned, journal)?;
        owned.locks().directory(selected, Binary::Controller)?;
        owned.locks().directory(selected, Binary::Cli)?;
        for path in [home, node] {
            if !path.is_absolute()
                || !path
                    .components()
                    .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
            {
                bail!("update_launcher_context_invalid");
            }
        }
        let url = selected.launcher_url()?;
        let arguments = vec![
            "launcher".into(),
            "--node".into(),
            node.as_os_str().to_owned(),
            "--treer".into(),
            selected.cli_path().into_os_string(),
            "--workspace".into(),
            selected.workspace().into(),
            "--url".into(),
            url.clone().into(),
            match operation {
                Operation::Install => "install",
                Operation::Status => "status",
            }
            .into(),
        ];
        // Construct from empty, never copy std::env::vars(). The future child
        // must call env_clear before applying this allowlist.
        let environment = [
            (OsString::from("HOME"), home.as_os_str().to_owned()),
            (
                "XDG_CONFIG_HOME".into(),
                selected.launcher_config_home()?.as_os_str().to_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("LANG".into(), "C".into()),
            ("LC_ALL".into(), "C".into()),
            ("TREER_WORKSPACE_ID".into(), selected.workspace().into()),
            ("TREER_AGENT_SERVER_URL".into(), url.into()),
            ("TREER_SERVER_ID".into(), selected.server_id().into()),
        ]
        .into_iter()
        .collect();
        let program = selected.controller_path().to_owned();
        let cwd = program
            .parent()
            .ok_or_else(|| anyhow::anyhow!("update_launcher_context_invalid"))?
            .to_owned();
        Ok(Self {
            selected,
            owned,
            journal,
            receipt,
            operation,
            program,
            cwd,
            arguments,
            environment,
            temporary: None,
            node: None,
        })
    }

    fn bind_node(mut self, node: RetainedExecutable) -> Result<Self> {
        self.verify_binding()?;
        if self.arguments.get(2).map(OsString::as_os_str) != Some(node.executable().as_os_str()) {
            bail!("update_launcher_binding_mismatch");
        }
        node.verify()
            .map_err(|_| anyhow::anyhow!("update_launcher_node_changed"))?;
        self.node = Some(node);
        Ok(self)
    }

    fn bind_temporary(mut self, temporary: &'a LauncherTemporary<'a>) -> Result<Self> {
        self.verify_binding()?;
        temporary.verify_for(self.selected, self.owned, self.journal, &self.receipt)?;
        self.environment
            .insert("TMPDIR".into(), temporary.path()?.as_os_str().to_owned());
        self.temporary = Some(temporary);
        Ok(self)
    }

    /// Recheck receipt/ownership and retained dependencies; never substitutes
    /// for release-image authentication or the installer's durable lifecycle.
    pub fn verify_binding(&self) -> Result<()> {
        if launcher_preflight::checked(self.selected, self.owned, self.journal)? != self.receipt {
            bail!("update_launcher_binding_mismatch");
        }
        self.owned
            .locks()
            .directory(self.selected, Binary::Controller)?;
        self.owned.locks().directory(self.selected, Binary::Cli)?;
        if let Some(temporary) = self.temporary {
            temporary.verify_for(self.selected, self.owned, self.journal, &self.receipt)?;
        }
        if let Some(node) = &self.node {
            node.verify()
                .map_err(|_| anyhow::anyhow!("update_launcher_node_changed"))?;
        }
        Ok(())
    }

    /// Compatibility check of the retained interpreter's self-reported version,
    /// not authentication of its contents. Grants no reusable execution
    /// authority and does not advance the journal.
    pub async fn check_node_version(&self) -> Result<()> {
        self.check_node_until(tokio::time::Instant::now() + std::time::Duration::from_secs(10))
            .await
    }

    async fn check_node_until(&self, deadline: tokio::time::Instant) -> Result<()> {
        let node = self
            .node
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("update_launcher_binding_mismatch"))?;
        let mut command = std::process::Command::new(node.executable());
        command
            .arg("--version")
            .env_clear()
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .current_dir(
                node.executable()
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("update_launcher_binding_mismatch"))?,
            );
        node_budget(deadline, || self.verify_binding())?;
        let output =
            treer_platform::bounded_child::output_quiescent_until(&mut command, deadline, 1024)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(match error.kind() {
                        std::io::ErrorKind::TimedOut => "update_launcher_node_probe_timeout",
                        std::io::ErrorKind::FileTooLarge =>
                            "update_launcher_node_probe_output_limit",
                        _ => "update_launcher_node_probe_failed",
                    })
                })?;
        node_budget(deadline, || self.verify_binding())?;
        if !output.status().success() {
            bail!("update_launcher_node_probe_failed");
        }
        node_budget(deadline, || parse_node_version(output.stdout()))
    }

    pub fn program(&self) -> &Path {
        &self.program
    }
    pub fn current_directory(&self) -> &Path {
        &self.cwd
    }
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
    pub fn environment(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.environment
            .iter()
            .map(|(key, value)| (key.as_os_str(), value.as_os_str()))
    }
}

fn retain_node(path: &Path) -> Result<RetainedExecutable> {
    RetainedExecutable::open(path)
        .map_err(|_| anyhow::anyhow!("update_launcher_node_untrusted_location"))
}

fn node_budget<T>(deadline: tokio::time::Instant, work: impl FnOnce() -> Result<T>) -> Result<T> {
    if tokio::time::Instant::now() >= deadline {
        bail!("update_launcher_node_probe_timeout");
    }
    let result = work();
    if tokio::time::Instant::now() >= deadline {
        bail!("update_launcher_node_probe_timeout");
    }
    result
}

fn parse_node_version(bytes: &[u8]) -> Result<()> {
    let invalid = || anyhow::anyhow!("invalid_update_launcher_node_version");
    if bytes.len() > 64 {
        return Err(invalid());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    let text = text.strip_prefix('v').ok_or_else(invalid)?;
    let components = text.split('.').collect::<Vec<_>>();
    if components.len() != 3
        || components.iter().any(|c| {
            c.is_empty()
                || c.len() > 10
                || !c.bytes().all(|b| b.is_ascii_digit())
                || (c.len() > 1 && c.starts_with('0'))
        })
    {
        return Err(invalid());
    }
    let numbers = components
        .iter()
        .map(|c| c.parse::<u32>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| invalid())?;
    if numbers[0] < 22 {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::{
        publication::Publications, selection::tests::Fixture, tests::receipt, Phase,
    };
    use treer_platform::private_directory::PrivateDirectory;

    // The normal selection fixture lives under OS temp (a writable ancestor),
    // which the production Node trust check must reject. Use an exclusive
    // directory beside this test binary in the private Cargo target instead.
    struct NodeFixture(PathBuf);
    impl NodeFixture {
        fn new() -> Self {
            use std::os::unix::fs::DirBuilderExt;
            let parent = std::env::current_exe().unwrap().canonicalize().unwrap();
            let path = parent
                .parent()
                .unwrap()
                .join(format!("launcher-node-test-{}", uuid::Uuid::new_v4()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            let fixture = Self(path);
            fixture.write("node");
            fixture
        }
        fn write(&self, name: &str) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let path = self.0.join(name);
            std::fs::write(&path, b"isolated inert interpreter fixture").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
            path
        }
        fn node(&self) -> PathBuf {
            self.0.join("node")
        }
        fn script(&self, body: &str) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::remove_file(self.node()).unwrap();
            std::fs::write(self.node(), format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(self.node(), std::fs::Permissions::from_mode(0o500)).unwrap();
        }
    }
    impl Drop for NodeFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn launch_intent_is_exclusive_private_bound_and_never_means_child_started() {
        use std::os::unix::fs::PermissionsExt;
        for (operation, suffix) in [
            (Operation::Install, "install"),
            (Operation::Status, "status"),
        ] {
            let f = Fixture::standard_config();
            f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
            let (selected, owned, journal) = bind(&f, "ws_a");
            let before = journal.read().unwrap();
            let job = f.path.join("jobs").join(before.job_id.to_string());
            let temporary = LauncherTemporary::create(&selected, &owned, &journal, &job).unwrap();
            let node = NodeFixture::new(); // Inert, never executed.
            let plan = Invocation::with_context(
                &selected,
                &owned,
                &journal,
                operation,
                &f.path,
                &node.node(),
            )
            .unwrap()
            .bind_node(retain_node(&node.node()).unwrap())
            .unwrap()
            .bind_temporary(&temporary)
            .unwrap();
            let intent = plan.record_start_intent().unwrap();
            intent.verify().unwrap();
            let path = job.join(format!("launcher-{suffix}-intent.json"));
            let bytes = std::fs::read(&path).unwrap();
            use sha2::{Digest, Sha256};
            assert_eq!(
                intent.sha256().unwrap(),
                format!("{:x}", Sha256::digest(&bytes))
            );
            assert!(bytes.len() <= 64 * 1024);
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["schema_version"], 1);
            assert_eq!(value["receipt"], serde_json::to_value(&before).unwrap());
            assert_eq!(value["operation"], suffix);
            assert_eq!(
                value["operation_id"],
                format!("update-{}-launcher-{suffix}", before.job_id)
            );
            assert_eq!(value.as_object().unwrap().len(), 4);
            assert!(!String::from_utf8_lossy(&bytes).contains(f.path.to_str().unwrap()));
            assert!(!String::from_utf8_lossy(&bytes).contains(node.node().to_str().unwrap()));
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                plan.record_start_intent().err().unwrap().to_string(),
                "update_recovery_required"
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(journal.read().unwrap(), before);
            drop(plan);
            assert_eq!(std::fs::read(path).unwrap(), bytes); // Recovery evidence remains.
        }
    }

    #[test]
    fn production_gate_capability_is_minted_only_by_the_registration_leaf() {
        fn visit(path: &Path, found: &mut Vec<PathBuf>, constructors: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    visit(&entry.path(), found, constructors);
                } else if kind.is_file() && entry.path().extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(entry.path()).unwrap();
                    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
                    for _ in text.match_indices(&["Registration", "Gate", "{"].concat()) {
                        found.push(entry.path());
                    }
                    for _ in text.match_indices(&["Arbitration", "::registered", "("].concat()) {
                        constructors.push(entry.path());
                    }
                }
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/self_update");
        let mut found = Vec::new();
        let mut constructors = Vec::new();
        visit(&root, &mut found, &mut constructors);
        let leaf = root.join("launcher_invocation/start/registration.rs");
        // Definition, impl and sole literal; source tripwire, not Rust parsing.
        assert_eq!(found, [leaf.clone(), leaf.clone(), leaf.clone()]);
        assert_eq!(constructors.as_slice(), std::slice::from_ref(&leaf));
        let text = std::fs::read_to_string(leaf).unwrap();
        assert!(!text.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("mod ") || line.starts_with("pub mod ")
        }));
        let arbitration = include_str!("launcher_arbitration.rs");
        let compact: String = arbitration.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("#[cfg(test)]pubfnnew(binding:Binding)"));
    }

    #[test]
    #[ignore = "owned subprocess helper, invoked by registration fixture"]
    fn supervisor_registration_child() {
        if std::env::var_os("TREER_TEST_REGISTRATION_NOT_PRIVATE").is_none() {
            treer_platform::process_image::enter_private_session().unwrap();
        }
        let _owner_socket = std::env::var_os("TREER_TEST_REGISTRATION_OWNER_SOCKET")
            .map(|path| std::os::unix::net::UnixStream::connect(path).unwrap());
        std::fs::write(
            std::env::var_os("TREER_TEST_REGISTRATION_READY").unwrap(),
            b"ready",
        )
        .unwrap();
        use std::io::Read;
        let mut byte = [0];
        let _ = std::io::stdin().read(&mut byte);
        if let Some(next) = std::env::var_os("TREER_TEST_REGISTRATION_EXEC") {
            use std::os::unix::process::CommandExt;
            let error = std::process::Command::new(next)
                .args([
                    "--exact",
                    "self_update::launcher_invocation::tests::supervisor_registration_child",
                    "--ignored",
                ])
                .env_clear()
                // The session survives exec; do not call setsid twice.
                .env("TREER_TEST_REGISTRATION_NOT_PRIVATE", "1")
                .env(
                    "TREER_TEST_REGISTRATION_READY",
                    std::env::var_os("TREER_TEST_REGISTRATION_EXEC_READY").unwrap(),
                )
                .exec();
            panic!("fixture exec failed: {error}");
        }
    }

    #[test]
    fn supervisor_registration_binds_durable_intent_and_rejects_changed_evidence() {
        supervisor_registration_cases(&[
            "ok",
            "replacement",
            "mutation",
            "intent",
            "second_attempt",
            "lost_gate",
            "exit",
            "existing_empty",
            "existing_corrupt",
            "existing_symlink",
            "nil",
            "not_private",
            "already_exited",
            "wrong_image",
            "pre_exec",
        ]);
    }

    #[test]
    fn supervisor_registration_retains_expected_image_and_rejects_path_replacement() {
        supervisor_registration_cases(&["expected_replaced"]);
    }

    #[test]
    fn supervisor_registration_rejects_an_authenticated_child_as_permit_owner() {
        supervisor_registration_cases(&["wrong_owner"]);
    }

    fn supervisor_registration_cases(faults: &[&str]) {
        use std::{
            io::Write,
            os::unix::fs::{symlink, DirBuilderExt, MetadataExt, PermissionsExt},
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        use treer_platform::process_image::ProcessImage;
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        struct SocketDirectory(std::path::PathBuf);
        impl Drop for SocketDirectory {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
        for &fault in faults {
            let f = Fixture::standard_config();
            f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
            let (selected, owned, journal) = bind(&f, "ws_a");
            let before = journal.read().unwrap();
            let job = f.path.join("jobs").join(before.job_id.to_string());
            let temporary = LauncherTemporary::create(&selected, &owned, &journal, &job).unwrap();
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
            let intent = plan.record_start_intent().unwrap();
            let intent_path = job.join("launcher-install-intent.json");
            let intent_digest = intent.sha256().unwrap().to_owned();
            let intent_metadata = std::fs::metadata(&intent_path).unwrap();
            let ready = job.join("test-ready");
            let exec_ready = job.join("test-exec-ready");
            let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
            let copied = job.join("test-supervisor");
            if matches!(fault, "wrong_image" | "pre_exec" | "expected_replaced") {
                // Identical bytes at a different inode must not pass identity.
                std::fs::copy(&executable, &copied).unwrap();
                std::fs::set_permissions(&copied, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let expected_path = if matches!(fault, "pre_exec" | "expected_replaced") {
                &copied
            } else {
                &executable
            };
            let expected_directory =
                InstallationDirectory::open(expected_path.parent().unwrap()).unwrap();
            let expected_digest = crate::self_update::images::digest_file(
                &std::fs::File::open(expected_path).unwrap(),
            )
            .unwrap();
            let expected = crate::self_update::verified_image::VerifiedImage::open(
                &expected_directory,
                expected_path,
                &expected_digest,
            )
            .unwrap();
            // A separate short private directory avoids sockaddr_un's limit
            // even with a long macOS TMPDIR or deeply nested job directory.
            let owner_directory = (fault == "wrong_owner").then(|| {
                let path =
                    std::path::Path::new("/tmp").join(format!("tu-{}", uuid::Uuid::new_v4()));
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&path)
                    .unwrap();
                SocketDirectory(path)
            });
            let owner_socket = owner_directory
                .as_ref()
                .map(|directory| directory.0.join("s"));
            let owner_listener = (fault == "wrong_owner").then(|| {
                std::os::unix::net::UnixListener::bind(owner_socket.as_ref().unwrap()).unwrap()
            });
            let mut child = Child(
                Command::new(if matches!(fault, "wrong_image" | "expected_replaced") {
                    &copied
                } else {
                    &executable
                })
                .args([
                    "--exact",
                    "self_update::launcher_invocation::tests::supervisor_registration_child",
                    "--ignored",
                ])
                .env_clear()
                .env("TREER_TEST_REGISTRATION_READY", &ready)
                .envs(
                    owner_socket
                        .as_ref()
                        .map(|path| ("TREER_TEST_REGISTRATION_OWNER_SOCKET", path)),
                )
                .envs((fault == "pre_exec").then_some(("TREER_TEST_REGISTRATION_EXEC", &copied)))
                .env("TREER_TEST_REGISTRATION_EXEC_READY", &exec_ready)
                .envs(
                    (fault == "not_private")
                        .then_some(("TREER_TEST_REGISTRATION_NOT_PRIVATE", "1")),
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready.exists() {
                assert!(Instant::now() < deadline);
                assert!(child.0.try_wait().unwrap().is_none());
                std::thread::sleep(Duration::from_millis(1));
            }
            if fault == "already_exited" {
                child.0.stdin.take();
                assert!(child.0.wait().unwrap().success());
            }
            let path = job.join("launcher-install-supervisor.json");
            match fault {
                "existing_empty" => std::fs::write(&path, b"").unwrap(),
                "existing_corrupt" => std::fs::write(&path, b"partial private evidence").unwrap(),
                "existing_symlink" => symlink(&intent_path, &path).unwrap(),
                _ => (),
            }
            let attempt = if fault == "nil" {
                uuid::Uuid::nil()
            } else {
                uuid::Uuid::new_v4()
            };
            let result = intent.register_supervisor(attempt, &mut child.0, &expected);
            if fault.starts_with("existing")
                || matches!(
                    fault,
                    "nil" | "not_private" | "already_exited" | "wrong_image" | "pre_exec"
                )
            {
                assert_eq!(
                    result.err().unwrap().to_string(),
                    "update_recovery_required",
                    "{fault}"
                );
                if fault == "existing_corrupt" {
                    assert_eq!(std::fs::read(&path).unwrap(), b"partial private evidence");
                }
                if matches!(
                    fault,
                    "nil" | "not_private" | "already_exited" | "wrong_image" | "pre_exec"
                ) {
                    assert!(!path.exists());
                }
                if fault == "pre_exec" {
                    // Only now release the real same-PID exec transition. The
                    // earlier private-session image was not the expected image.
                    child.0.stdin.as_mut().unwrap().write_all(b"x").unwrap();
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !exec_ready.exists() {
                        assert!(Instant::now() < deadline);
                        assert!(child.0.try_wait().unwrap().is_none());
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    expected
                        .verify_process(&ProcessImage::capture(child.0.id()).unwrap())
                        .unwrap();
                    assert!(!path.exists());
                    assert!(intent_path.exists());
                    assert_eq!(journal.read().unwrap(), before);
                }
                continue;
            }
            let mut registered = result.unwrap();
            registered.binding().unwrap();
            let bytes = std::fs::read(&path).unwrap();
            if let Some(listener) = owner_listener {
                use crate::self_update::{
                    handoff::AuthenticatedPeer, launcher_arbitration::Decision,
                };
                use std::os::unix::net::UnixStream;
                let (stream, _) = listener.accept().unwrap();
                let image = ProcessImage::capture(child.0.id()).unwrap();
                let digest = crate::self_update::images::digest_file(image.executable()).unwrap();
                let mut peer =
                    AuthenticatedPeer::authenticate(stream, &image, image.executable(), &digest)
                        .unwrap();
                let (cancel, _writer) = UnixStream::pair().unwrap();
                // Real authenticated peer, same UID/image, correct registered
                // child: still not its retained parent. Refuse before reading.
                assert_eq!(
                    registered
                        .read_launcher_frame(&mut peer, &cancel)
                        .err()
                        .unwrap()
                        .to_string(),
                    "update_recovery_required"
                );
                assert_eq!(registered.decision(), Decision::Uncertain);
                assert!(registered.read_launcher_frame(&mut peer, &cancel).is_err());
                assert_eq!(registered.decision(), Decision::Uncertain);
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
                assert_eq!(journal.read().unwrap(), before);
                continue;
            }
            if fault == "lost_gate" {
                // Model loss of all in-memory registration authority, without
                // claiming this is a real supervisor process-crash test.
                drop(registered);
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
                assert_eq!(
                    plan.record_start_intent().err().unwrap().to_string(),
                    "update_recovery_required"
                );
                // Even deliberately substituting a fresh intent in this
                // fixture cannot recover an accept-capable supervisor value.
                std::fs::rename(&intent_path, job.join("saved-intent")).unwrap();
                let next = plan.record_start_intent().unwrap();
                assert_eq!(
                    next.register_supervisor(attempt, &mut child.0, &expected)
                        .err()
                        .unwrap()
                        .to_string(),
                    "update_recovery_required"
                );
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
                assert_eq!(journal.read().unwrap(), before);
                continue;
            }
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["intent_sha256"], intent_digest);
            assert_eq!(value["intent_device"], intent_metadata.dev());
            assert_eq!(value["intent_inode"], intent_metadata.ino());
            assert_eq!(value["attempt"], attempt.to_string());
            assert_eq!(value["pid"], child.0.id());
            assert_eq!(value["schema_version"], 2);
            assert_eq!(value["owner_pid"], std::process::id());
            let owner_image = ProcessImage::capture(std::process::id()).unwrap();
            assert_eq!(
                value["owner_creation_identity"],
                owner_image.creation_identity()
            );
            assert_eq!(
                value["owner_image_device"],
                owner_image.executable().metadata().unwrap().dev()
            );
            assert_eq!(
                value["owner_image_inode"],
                owner_image.executable().metadata().unwrap().ino()
            );
            assert_eq!(
                value["owner_image_sha256"],
                crate::self_update::images::digest_file(owner_image.executable()).unwrap()
            );
            assert_eq!(value["session"], child.0.id());
            assert_eq!(value["process_group"], child.0.id());
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert!(!String::from_utf8_lossy(&bytes).contains(f.path.to_str().unwrap()));
            use crate::self_update::{handoff::AuthenticatedPeer, launcher_arbitration::Decision};
            use sha2::{Digest, Sha256};
            use std::os::unix::net::UnixStream;
            let (cancel, _cancel_writer) = UnixStream::pair().unwrap();
            // Permit owner is the authenticated test parent; registered
            // supervisor is the separate owned child. Their PIDs differ.
            let mut connection = if matches!(fault, "ok" | "mutation") {
                let (reader, writer) = UnixStream::pair().unwrap();
                let owner = ProcessImage::capture(std::process::id()).unwrap();
                let owner_digest =
                    crate::self_update::images::digest_file(owner.executable()).unwrap();
                let peer = AuthenticatedPeer::authenticate(
                    reader,
                    &owner,
                    owner.executable(),
                    &owner_digest,
                )
                .unwrap();
                Some((peer, writer))
            } else {
                None
            };
            let frame = if let Some((peer, writer)) = connection.as_mut() {
                let mut wire = b"TRLP\x01\0\0\0".to_vec();
                wire.extend_from_slice(attempt.as_bytes());
                wire.extend_from_slice(intent_digest.as_bytes());
                wire.extend_from_slice(format!("{:x}", Sha256::digest(&bytes)).as_bytes());
                writer.write_all(&wire).unwrap();
                writer.shutdown(std::net::Shutdown::Write).unwrap();
                let frame = registered.read_launcher_frame(peer, &cancel).unwrap();
                assert_eq!(registered.decision(), Decision::Pending);
                Some(frame)
            } else {
                None
            };
            match fault {
                "expected_replaced" => {
                    // The running image is unchanged, but its retained expected
                    // pathname now names a byte-identical different inode.
                    std::fs::rename(&copied, job.join("saved-supervisor")).unwrap();
                    std::fs::copy(&executable, &copied).unwrap();
                    std::fs::set_permissions(&copied, std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                }
                "replacement" => {
                    let inode = std::fs::metadata(&path).unwrap().ino();
                    std::fs::rename(&path, job.join("saved-registration")).unwrap();
                    std::fs::write(&path, &bytes).unwrap();
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    assert_ne!(std::fs::metadata(&path).unwrap().ino(), inode);
                }
                "mutation" | "intent" => {
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(if fault == "intent" {
                            &intent_path
                        } else {
                            &path
                        })
                        .unwrap()
                        .write_all(b"\n")
                        .unwrap();
                }
                "second_attempt" => {
                    // Even a fresh intent and a new UUID cannot supersede an
                    // existing registration for this operation.
                    std::fs::rename(&intent_path, job.join("saved-intent")).unwrap();
                    let next = plan.record_start_intent().unwrap();
                    let result =
                        next.register_supervisor(uuid::Uuid::new_v4(), &mut child.0, &expected);
                    assert_eq!(
                        result.err().unwrap().to_string(),
                        "update_recovery_required"
                    );
                    assert_eq!(std::fs::read(&path).unwrap(), bytes);
                }
                "exit" => {
                    child.0.stdin.take();
                    assert!(child.0.wait().unwrap().success());
                }
                "ok" => (),
                _ => unreachable!(),
            }
            if fault != "ok" {
                assert_eq!(
                    registered.verify().unwrap_err().to_string(),
                    "update_recovery_required",
                    "{fault}"
                );
                assert_eq!(
                    registered.binding().err().unwrap().to_string(),
                    "update_recovery_required",
                    "{fault}"
                );
            }
            if let Some(frame) = frame {
                if fault == "ok" {
                    registered.accept_launcher_frame(frame).unwrap();
                    assert_eq!(registered.decision(), Decision::Accepted);
                } else {
                    assert_eq!(
                        registered
                            .accept_launcher_frame(frame)
                            .unwrap_err()
                            .to_string(),
                        "update_recovery_required"
                    );
                    assert_eq!(registered.decision(), Decision::Uncertain);
                    // Restoring exact evidence cannot reopen the private gate.
                    std::fs::write(&path, &bytes).unwrap();
                    registered.verify().unwrap();
                }
                assert!(registered
                    .read_launcher_frame(&mut connection.as_mut().unwrap().0, &cancel)
                    .is_err());
                assert_eq!(
                    registered.decision(),
                    if fault == "ok" {
                        Decision::Accepted
                    } else {
                        Decision::Uncertain
                    }
                );
            }
            drop(registered);
            assert!(path.exists()); // Drop preserves recovery evidence.
            assert_eq!(journal.read().unwrap(), before);
        }
    }

    #[test]
    fn recorded_intent_rejects_replacement_mutation_and_revoked_recipe() {
        use std::os::unix::fs::{symlink, MetadataExt};
        for fault in [
            "replacement",
            "mutation",
            "symlink",
            "missing",
            "receipt",
            "node",
            "temporary",
        ] {
            let f = Fixture::standard_config();
            f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
            let (selected, owned, journal) = bind(&f, "ws_a");
            let before = journal.read().unwrap();
            let job = f.path.join("jobs").join(before.job_id.to_string());
            let temporary = LauncherTemporary::create(&selected, &owned, &journal, &job).unwrap();
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
            let intent = plan.record_start_intent().unwrap();
            let path = job.join("launcher-install-intent.json");
            let bytes = std::fs::read(&path).unwrap();
            let inode = std::fs::metadata(&path).unwrap().ino();
            intent.sha256().unwrap();
            match fault {
                "replacement" | "symlink" => {
                    // Retain the original evidence; a byte-identical replacement
                    // must still fail the retained-inode comparison.
                    let saved = job.join("saved-intent");
                    std::fs::rename(&path, &saved).unwrap();
                    if fault == "symlink" {
                        symlink(&saved, &path).unwrap();
                    } else {
                        std::fs::write(&path, &bytes).unwrap();
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                            .unwrap();
                        assert_ne!(std::fs::metadata(&path).unwrap().ino(), inode);
                    }
                }
                "mutation" => {
                    // Even semantically equivalent JSON differs from the exact
                    // bytes whose digest the future registration will bind.
                    let mut changed = bytes.clone();
                    changed.push(b'\n');
                    std::fs::write(&path, changed).unwrap();
                    assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
                }
                "missing" => std::fs::rename(&path, job.join("saved-intent")).unwrap(),
                "receipt" => {
                    let mut changed = before.clone();
                    changed.revision += 1;
                    journal.write(&changed).unwrap();
                }
                "node" => node.script("exit 0"),
                "temporary" => {
                    std::fs::rename(job.join("launcher-tmp"), job.join("saved-tmp")).unwrap()
                }
                _ => unreachable!(),
            }
            assert_eq!(
                intent.verify().unwrap_err().to_string(),
                "update_recovery_required",
                "{fault}"
            );
            assert_eq!(
                intent.sha256().unwrap_err().to_string(),
                "update_recovery_required",
                "{fault}"
            );
            // Verification never repairs, replays or launches the inert recipe.
            if !matches!(fault, "missing" | "symlink" | "mutation") {
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
            }
        }
    }

    #[test]
    fn launch_intent_preserves_empty_corrupt_and_nonfile_entries() {
        use std::os::unix::fs::symlink;
        for kind in ["empty", "corrupt", "directory", "symlink"] {
            let f = Fixture::standard_config();
            f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
            let (selected, owned, journal) = bind(&f, "ws_a");
            let job = f
                .path
                .join("jobs")
                .join(journal.read().unwrap().job_id.to_string());
            let temporary = LauncherTemporary::create(&selected, &owned, &journal, &job).unwrap();
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
            let path = job.join("launcher-install-intent.json");
            match kind {
                "empty" => std::fs::write(&path, b"").unwrap(),
                "corrupt" => std::fs::write(&path, b"{private partial evidence").unwrap(),
                "directory" => std::fs::create_dir(&path).unwrap(),
                "symlink" => symlink(node.node(), &path).unwrap(),
                _ => unreachable!(),
            }
            let before = std::fs::symlink_metadata(&path).unwrap();
            for _ in 0..2 {
                assert_eq!(
                    plan.record_start_intent().err().unwrap().to_string(),
                    "update_recovery_required"
                );
            }
            assert_eq!(
                std::fs::symlink_metadata(&path).unwrap().file_type(),
                before.file_type()
            );
            if kind == "corrupt" {
                assert_eq!(std::fs::read(&path).unwrap(), b"{private partial evidence");
            }
            if kind == "empty" {
                assert!(std::fs::read(&path).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn launch_intent_rejects_unbound_recipe_or_changed_receipt_before_writing() {
        let f = Fixture::standard_config();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        let before = journal.read().unwrap();
        let job = f.path.join("jobs").join(before.job_id.to_string());
        let temporary = LauncherTemporary::create(&selected, &owned, &journal, &job).unwrap();
        let node = NodeFixture::new();
        let plan = Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Install,
            &f.path,
            &node.node(),
        )
        .unwrap();
        assert_eq!(
            plan.record_start_intent().err().unwrap().to_string(),
            "update_launcher_binding_mismatch"
        );
        let plan = plan
            .bind_node(retain_node(&node.node()).unwrap())
            .unwrap()
            .bind_temporary(&temporary)
            .unwrap();
        let mut changed = before;
        changed.revision += 1;
        journal.write(&changed).unwrap();
        assert!(plan.record_start_intent().is_err());
        assert!(!job.join("launcher-install-intent.json").exists());
        assert_eq!(journal.read().unwrap(), changed);
    }

    #[test]
    fn node_versions_require_stable_numeric_22_or_newer_without_extra_text() {
        for text in ["v22.0.0", "v22.14.0\n", "v24.1.2\n"] {
            parse_node_version(text.as_bytes()).unwrap();
        }
        for text in [
            "",
            "v21.9.9",
            "22.14.0",
            "v022.1.0",
            "v22.1",
            "v22.1.0-pre",
            "v22.1.0 private",
            "v22.1.0\n\n",
            "v22.1.0\r\n",
            "v4294967296.0.0",
        ] {
            assert_eq!(
                parse_node_version(text.as_bytes()).unwrap_err().to_string(),
                "invalid_update_launcher_node_version"
            );
        }
        assert!(parse_node_version(&[b'1'; 65]).is_err());
        assert!(parse_node_version(&[0xff]).is_err());
    }

    #[tokio::test]
    async fn node_version_probe_has_fixed_argv_clean_environment_and_preserves_journal() {
        let f = Fixture::standard_config();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        let before = journal.read().unwrap();
        let node = NodeFixture::new();
        node.script("[ \"$#\" = 1 ] && [ \"$1\" = --version ] || exit 2\n[ \"$LANG\" = C ] && [ \"$LC_ALL\" = C ] || exit 3\n[ -z \"${HOME+x}${XDG_CONFIG_HOME+x}${TREER_WORKSPACE_ID+x}${TREER_WORKLOAD_CREDENTIAL+x}${NODE_OPTIONS+x}${TMPDIR+x}\" ] || exit 4\nprintf 'v22.14.0\\n'");
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
        .unwrap();
        plan.check_node_version().await.unwrap();
        assert_eq!(journal.read().unwrap(), before);
        plan.verify_binding().unwrap();
    }

    #[tokio::test]
    async fn node_version_probe_rechecks_same_inode_after_child_quiescence() {
        let f = Fixture::standard_config();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        let node = NodeFixture::new();
        node.script("/bin/mv \"$0.next\" \"$0\" || exit 1\nprintf 'v22.14.0\\n'");
        node.write("node.next");
        let plan = Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Status,
            &f.path,
            &node.node(),
        )
        .unwrap()
        .bind_node(retain_node(&node.node()).unwrap())
        .unwrap();
        assert_eq!(
            plan.check_node_version().await.unwrap_err().to_string(),
            "update_launcher_node_changed"
        );
    }

    #[tokio::test]
    async fn node_probe_failure_output_limit_and_deadline_are_fixed_errors() {
        let cases = [
            (
                "printf 'private failure'; exit 1".to_owned(),
                "update_launcher_node_probe_failed",
            ),
            (
                format!("printf '%s' '{}'", "x".repeat(1025)),
                "update_launcher_node_probe_output_limit",
            ),
            (
                "printf 'v21.0.0\\n'".to_owned(),
                "invalid_update_launcher_node_version",
            ),
            (
                "/bin/sleep 1; printf 'v22.0.0\\n'".to_owned(),
                "update_launcher_node_probe_timeout",
            ),
        ];
        for (script, expected) in cases {
            let f = Fixture::standard_config();
            f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
            let (selected, owned, journal) = bind(&f, "ws_a");
            let node = NodeFixture::new();
            node.script(&script);
            let plan = Invocation::with_context(
                &selected,
                &owned,
                &journal,
                Operation::Status,
                &f.path,
                &node.node(),
            )
            .unwrap()
            .bind_node(retain_node(&node.node()).unwrap())
            .unwrap();
            let budget = if expected == "update_launcher_node_probe_timeout" {
                std::time::Duration::from_millis(200)
            } else {
                std::time::Duration::from_secs(10)
            };
            let deadline = tokio::time::Instant::now() + budget;
            assert_eq!(
                plan.check_node_until(deadline)
                    .await
                    .unwrap_err()
                    .to_string(),
                expected
            );
        }
        assert_eq!(
            node_budget::<()>(tokio::time::Instant::now(), || panic!(
                "expired budget must not work"
            ))
            .unwrap_err()
            .to_string(),
            "update_launcher_node_probe_timeout"
        );
    }

    #[test]
    fn trusted_node_symlink_resolves_before_retention_and_is_not_resolved_again() {
        use std::os::unix::fs::symlink;
        let f = Fixture::standard_config();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        let node = NodeFixture::new();
        let alias = node.0.join("package-manager-node");
        symlink(node.node(), &alias).unwrap();
        // Explicit fixture input reaches the exact trusted_executable helper
        // used by fixed-location resolution, without changing an OS location.
        let resolved = crate::launcher::node_executable(Some(alias.clone())).unwrap();
        assert_eq!(resolved, node.node().canonicalize().unwrap());
        let proof = retain_node(&resolved).unwrap();
        let plan = Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Status,
            &f.path,
            proof.executable(),
        )
        .unwrap()
        .bind_node(proof)
        .unwrap();
        assert_eq!(plan.arguments()[2], resolved.as_os_str());
        let other = node.write("other-node");
        std::fs::remove_file(&alias).unwrap();
        symlink(&other, &alias).unwrap();
        assert_eq!(
            crate::launcher::node_executable(Some(alias)).unwrap(),
            other
        );
        plan.verify_binding().unwrap(); // Still verifies the original physical path.
        assert_eq!(plan.arguments()[2], resolved.as_os_str());
        std::fs::rename(node.node(), node.0.join("old-node")).unwrap();
        node.write("node");
        assert_eq!(
            plan.verify_binding().unwrap_err().to_string(),
            "update_launcher_node_changed"
        );
    }

    #[test]
    fn retained_node_rejects_identical_replacement_and_never_adopts_it() {
        let f = Fixture::standard_config();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        let node = NodeFixture::new();
        let proof = retain_node(&node.node()).unwrap();
        let plan = Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Install,
            &f.path,
            &node.node(),
        )
        .unwrap()
        .bind_node(proof)
        .unwrap();
        plan.verify_binding().unwrap();
        std::fs::rename(node.node(), node.0.join("old-node")).unwrap();
        node.write("node"); // Same bytes/mode, different inode.
        assert_eq!(
            plan.verify_binding().unwrap_err().to_string(),
            "update_launcher_node_changed"
        );
        assert_eq!(
            plan.verify_binding().unwrap_err().to_string(),
            "update_launcher_node_changed"
        );
        // Fresh identity is available only to a new recipe, never adopted above.
        assert!(retain_node(&node.node()).is_ok());
    }

    #[test]
    fn node_trust_errors_are_fixed_and_recipe_requires_the_exact_node_path() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let f = Fixture::standard_config();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        let node = NodeFixture::new();
        symlink(node.node(), node.0.join("alias")).unwrap();
        for path in [
            node.0.join("missing-private-path"),
            node.0.join("alias"),
            node.0.clone(),
        ] {
            assert_eq!(
                retain_node(&path).err().unwrap().to_string(),
                "update_launcher_node_untrusted_location"
            );
        }
        let other = node.write("other-node");
        let plan = Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Status,
            &f.path,
            &other,
        )
        .unwrap();
        assert_eq!(
            plan.bind_node(retain_node(&node.node()).unwrap())
                .err()
                .unwrap()
                .to_string(),
            "update_launcher_binding_mismatch"
        );
        let proof = retain_node(&node.node()).unwrap();
        std::fs::set_permissions(node.node(), std::fs::Permissions::from_mode(0o700 | 0o020))
            .unwrap();
        let plan = Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Status,
            &f.path,
            &node.node(),
        )
        .unwrap();
        assert_eq!(
            plan.bind_node(proof).err().unwrap().to_string(),
            "update_launcher_node_changed"
        );
        assert_eq!(
            retain_node(&node.node()).err().unwrap().to_string(),
            "update_launcher_node_untrusted_location"
        );
    }

    fn bind(f: &Fixture, workspace: &str) -> (Selected, OwnedActivation, Journal) {
        let selected = f.inventory().select(Some(workspace), true).unwrap();
        let locks = selected.lock().unwrap();
        let mut r = receipt();
        r.workspace_id = workspace.into();
        r.server_id = selected.server_id().into();
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
        journal
            .advance(activating.revision, Phase::CoreHealthy)
            .unwrap();
        (selected, OwnedActivation::fixture(locks, prepared), journal)
    }

    #[test]
    fn fixed_operations_select_only_the_bound_enrollment_with_clean_environment() {
        use clap::Parser;
        let f = Fixture::standard_config();
        for (server, workspace) in [("srv_a", "ws_a"), ("srv_b", "ws_b")] {
            f.enrollment(server, workspace, &f.bin.join(Binary::Controller.name()));
        }
        let (selected, owned, journal) = bind(&f, "ws_b");
        let before = journal.read().unwrap();
        let home = f.path.join("account-home");
        let node = f.bin.join("trusted-node");
        let temporary = LauncherTemporary::create(
            &selected,
            &owned,
            &journal,
            &f.path.join("jobs").join(before.job_id.to_string()),
        )
        .unwrap();
        for (operation, name) in [
            (Operation::Install, "install"),
            (Operation::Status, "status"),
        ] {
            let plan =
                Invocation::with_context(&selected, &owned, &journal, operation, &home, &node)
                    .unwrap()
                    .bind_temporary(&temporary)
                    .unwrap();
            plan.verify_binding().unwrap();
            assert_eq!(plan.program(), selected.controller_path());
            assert_eq!(plan.current_directory(), f.bin);
            assert_eq!(
                plan.arguments(),
                [
                    OsString::from("launcher"),
                    "--node".into(),
                    node.as_os_str().to_owned(),
                    "--treer".into(),
                    f.bin.join("treer").into_os_string(),
                    "--workspace".into(),
                    "ws_b".into(),
                    "--url".into(),
                    "http://127.0.0.1:1234".into(),
                    name.into(),
                ]
            );
            let parsed = crate::Args::try_parse_from(
                std::iter::once(plan.program().as_os_str())
                    .chain(plan.arguments().iter().map(OsString::as_os_str)),
            )
            .unwrap();
            assert!(matches!(parsed.command, Some(crate::Command::Launcher(_))));
            let env: BTreeMap<_, _> = plan.environment().collect();
            assert_eq!(env.len(), 9);
            assert_eq!(env[OsStr::new("TMPDIR")], temporary.path().unwrap());
            assert_eq!(env[OsStr::new("HOME")], home);
            assert_eq!(env[OsStr::new("XDG_CONFIG_HOME")], f.path.join("xdg"));
            assert_eq!(env[OsStr::new("TREER_WORKSPACE_ID")], "ws_b");
            assert_eq!(env[OsStr::new("TREER_SERVER_ID")], "srv_b");
            assert_eq!(
                env[OsStr::new("TREER_AGENT_SERVER_URL")],
                "http://127.0.0.1:1234"
            );
            assert_eq!(env[OsStr::new("PATH")], "/usr/bin:/bin:/usr/sbin:/sbin");
            assert_eq!(env[OsStr::new("LANG")], "C");
            assert_eq!(env[OsStr::new("LC_ALL")], "C");
        }
        assert_eq!(journal.read().unwrap(), before);
        assert!(!f.bin.join("trusted-node").exists()); // No child execution.
    }

    #[test]
    fn temporary_binding_rejects_a_copied_journal_and_tracks_directory_replacement() {
        let f = Fixture::standard_config();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        let before = journal.read().unwrap();
        let job_path = f.path.join("jobs").join(before.job_id.to_string());
        let temporary = LauncherTemporary::create(&selected, &owned, &journal, &job_path).unwrap();
        let jobs = PrivateDirectory::open(&f.path.join("jobs")).unwrap();
        let copied = Journal::open(&jobs, &before.job_id.to_string()).unwrap();
        assert!(Invocation::with_context(
            &selected,
            &owned,
            &copied,
            Operation::Status,
            &f.path,
            &f.bin.join("node"),
        )
        .unwrap()
        .bind_temporary(&temporary)
        .is_err());
        let plan = Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Status,
            &f.path,
            &f.bin.join("node"),
        )
        .unwrap()
        .bind_temporary(&temporary)
        .unwrap();
        let target = temporary.path().unwrap().to_owned();
        std::fs::rename(&target, job_path.join("old-tmp")).unwrap();
        std::fs::create_dir(&target).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            plan.verify_binding().unwrap_err().to_string(),
            "update_launcher_temporary_changed"
        );
    }

    #[test]
    fn changed_journal_invalidates_an_existing_recipe() {
        let f = Fixture::standard_config();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        let plan = Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Install,
            &f.path,
            &f.bin.join("node"),
        )
        .unwrap();
        let r = journal.read().unwrap();
        journal.advance(r.revision, Phase::Partial).unwrap();
        assert_eq!(
            plan.verify_binding().unwrap_err().to_string(),
            "update_launcher_binding_mismatch"
        );
        assert!(Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Status,
            &f.path,
            &f.bin.join("node"),
        )
        .is_err());
    }

    #[test]
    fn same_phase_revision_change_also_invalidates_the_original_recipe() {
        let f = Fixture::standard_config();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        let plan = Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Status,
            &f.path,
            &f.bin.join("node"),
        )
        .unwrap();
        let mut changed = journal.read().unwrap();
        changed.revision += 1;
        // Test-only rewrite keeps the phase and immutable job fields valid.
        // An ownership/phase-only recheck would erroneously accept it.
        journal.write(&changed).unwrap();
        launcher_preflight::checked(&selected, &owned, &journal).unwrap();
        assert_eq!(
            plan.verify_binding().unwrap_err().to_string(),
            "update_launcher_binding_mismatch"
        );
    }

    #[test]
    fn nonstandard_config_layout_cannot_fall_back_to_ambient_config() {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        let result = Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Install,
            &f.path,
            &f.bin.join("node"),
        );
        assert_eq!(
            result.err().unwrap().to_string(),
            "update_launcher_config_binding_mismatch"
        );
    }

    #[test]
    fn context_paths_must_be_absolute_without_parent_components() {
        let f = Fixture::standard_config();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        for (home, node) in [
            (Path::new("relative-home"), f.bin.as_path()),
            (f.path.as_path(), Path::new("node")),
            (Path::new("/tmp/../home"), f.bin.as_path()),
        ] {
            let error = Invocation::with_context(
                &selected,
                &owned,
                &journal,
                Operation::Install,
                home,
                node,
            )
            .err()
            .unwrap();
            assert_eq!(error.to_string(), "update_launcher_context_invalid");
        }
    }
}
