//! Actual hosted systemd topology, not the inert placement model. Never default-run.
use super::*;
use std::{
    collections::BTreeMap,
    path::Path,
    process::{Child, Command, Stdio},
    time::Instant,
};

const HELPER: &str =
    "self_update::hosted_acceptance::fixture_tests::manager::hosted_systemd_submit_helper";

fn checked_group(group: &str, unit: &str) -> Result<()> {
    if !group.starts_with('/')
        || !group.ends_with(&format!("/{unit}"))
        || group.split('/').skip(1).any(|s| {
            s.is_empty()
                || s == "."
                || s == ".."
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.:@\\".contains(&b))
        })
    {
        bail!("hosted_manager_cgroup_invalid");
    }
    Ok(())
}
fn cgroup(pid: u32) -> Result<String> {
    diagnostics::manager_cgroup(pid)
}
fn quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    )
}
fn environment(root: &Path) -> Vec<String> {
    clean_command(Path::new("/usr/bin/env"), root)
        .get_envs()
        .map(|(k, v)| format!("{}={}", k.to_str().unwrap(), v.unwrap().to_str().unwrap()))
        .collect()
}
// Only task-owned fixed tools. No shell, output logs or inherited identity.
fn tool(program: &str, args: &[String]) -> Result<String> {
    let uid = nix::unistd::geteuid().as_raw();
    let runtime = format!("/run/user/{uid}");
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("XDG_RUNTIME_DIR", &runtime)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={runtime}/bus"),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait()? {
            let mut output = String::new();
            child
                .stdout
                .take()
                .unwrap()
                .take(16385)
                .read_to_string(&mut output)?;
            let missing_unit = program == "/usr/bin/systemctl"
                && args.iter().any(|a| a == "show")
                && output.lines().any(|s| s == "LoadState=not-found");
            if !(status.success() || missing_unit) || output.len() > 16384 {
                bail!("hosted_manager_query_or_command_failed");
            }
            return Ok(output);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("hosted_manager_command_timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn show(unit: &str) -> Result<BTreeMap<String, String>> {
    let output = tool("/usr/bin/systemctl", &["--user".into(), "show".into(), unit.into(),
        "--property=Id,LoadState,MainPID,ControlGroup,InvocationID,NRestarts,Transient,KillMode,ActiveState,SubState,Type,Restart,StartLimitBurst".into()])?;
    Ok(output
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .collect())
}
fn field<'a>(values: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    values
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("hosted_manager_property_missing"))
}

pub(super) struct Unit {
    name: String,
    group: String,
    invocation: String,
    pid: u32,
}
impl Unit {
    fn start(root: &Path) -> Result<Self> {
        if !cfg!(target_os = "linux") {
            bail!("hosted_manager_linux_required");
        }
        // root() enforces hosted/nonroot/real HOME/private fresh-root eligibility.
        if super::super::root()? != root {
            bail!("hosted_manager_root_invalid");
        }
        let name = format!(
            "treer-native-{}.service",
            &root.file_name().unwrap().to_str().unwrap()[3..]
        );
        if field(&show(&name)?, "LoadState")? != "not-found" {
            bail!("hosted_manager_unit_collision");
        }
        let env = environment(root);
        let mut post = vec!["/usr/bin/env".to_owned(), "-i".into()];
        post.extend(env.clone());
        post.push("TREER_ACCEPTANCE_SYSTEMD=1".into());
        post.push(
            std::env::current_exe()?
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("hosted_manager_test_path_invalid"))?
                .into(),
        );
        post.extend([HELPER.into(), "--exact".into(), "--ignored".into()]);
        let post = post.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ");
        let mut args = vec![
            "--user".into(),
            "--no-block".into(),
            format!("--unit={name}"),
            "--property=Type=simple".into(),
            "--property=Restart=always".into(),
            "--property=RestartSec=2".into(),
            "--property=StartLimitIntervalSec=600".into(),
            "--property=StartLimitBurst=1".into(),
            "--property=KillMode=control-group".into(),
            "--property=TimeoutStartSec=480".into(),
            "--property=TimeoutStopSec=15".into(),
            "--property=StandardOutput=null".into(),
            "--property=StandardError=null".into(),
            format!("--property=ExecStartPost={post}"),
            "--".into(),
            "/usr/bin/env".into(),
            "-i".into(),
        ];
        args.extend(env.into_iter().map(|v| v.replace('$', "$$")));
        args.push(root.join("bin/treer-agent-host").to_str().unwrap().into());
        args.extend([
            "run".into(),
            "--config".into(),
            root.join("xdg/treer/agent-servers/srv_native_acceptance-host.json")
                .to_str()
                .unwrap()
                .into(),
        ]);
        let directory = PrivateDirectory::open(root)?;
        write_json(
            &directory,
            "native-manager-intent.json",
            &serde_json::json!({"schema":1,"unit":name,"create_once":true,"recovery_authority":false}),
        )?;
        // Only success acknowledges unit creation. An uncertain failure is never adopted/replayed.
        tool("/usr/bin/systemd-run", &args)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let values = show(&name)?;
            let pid = field(&values, "MainPID")?.parse::<u32>()?;
            if pid != 0 {
                let group = field(&values, "ControlGroup")?.to_owned();
                checked_group(&group, &name)?;
                let unit = Self {
                    name: name.clone(),
                    group,
                    invocation: field(&values, "InvocationID")?.to_owned(),
                    pid,
                };
                let host = ProcessImage::capture(pid)?;
                if !host.matches_file(
                    &PrivateDirectory::open(&root.join("bin"))?.read_file("treer-agent-host")?,
                )? {
                    if Instant::now() >= deadline {
                        bail!("hosted_manager_host_image_unavailable");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                unit.verify()?;
                write_json(
                    &directory,
                    "native-manager-expected.json",
                    &serde_json::json!({"schema":1,"unit":unit.name,"control_group":unit.group}),
                )?;
                return Ok(unit);
            }
            if Instant::now() >= deadline {
                bail!("hosted_manager_host_start_timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn verify(&self) -> Result<()> {
        let values = show(&self.name)?;
        self.check_metadata(&values)?;
        if cgroup(self.pid)? != self.group {
            bail!("hosted_manager_host_cgroup_changed");
        }
        // Prove that the mounted cgroup tree corresponds to the live membership
        // BEFORE interpreting a missing path after stop as removed/empty.
        let events = std::fs::read_to_string(
            Path::new("/sys/fs/cgroup")
                .join(self.group.trim_start_matches('/'))
                .join("cgroup.events"),
        )?;
        if !events.lines().any(|line| line == "populated 1") {
            bail!("hosted_manager_cgroup_mount_unavailable");
        }
        Ok(())
    }
    fn check_metadata(&self, values: &BTreeMap<String, String>) -> Result<()> {
        for (k, v) in [
            ("Id", self.name.as_str()),
            ("MainPID", self.pid.to_string().as_str()),
            ("ControlGroup", self.group.as_str()),
            ("InvocationID", self.invocation.as_str()),
            ("NRestarts", "0"),
            ("Transient", "yes"),
            ("KillMode", "control-group"),
            ("Type", "simple"),
            ("Restart", "always"),
            ("StartLimitBurst", "1"),
        ] {
            if field(values, k)? != v {
                bail!("hosted_manager_identity_changed");
            }
        }
        if self.invocation.len() != 32
            || !self.invocation.bytes().all(|b| b.is_ascii_hexdigit())
            || !matches!(field(values, "ActiveState")?, "activating" | "active")
        {
            bail!("hosted_manager_not_running");
        }
        Ok(())
    }
    async fn stop(&self) -> Result<bool> {
        self.verify()?;
        tool(
            "/usr/bin/systemctl",
            &["--user".into(), "stop".into(), self.name.clone()],
        )?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let values = show(&self.name)?;
            let stopped = field(&values, "LoadState")? == "not-found"
                || (field(&values, "MainPID")? == "0"
                    && matches!(field(&values, "ActiveState")?, "inactive" | "failed")
                    && field(&values, "NRestarts")? == "0");
            let path = Path::new("/sys/fs/cgroup")
                .join(self.group.trim_start_matches('/'))
                .join("cgroup.events");
            let empty = match std::fs::read_to_string(path) {
                Ok(events) => events.lines().any(|s| s == "populated 0"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                Err(_) => false,
            };
            if stopped && empty {
                return Ok(true);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

pub(super) enum FixtureHost {
    Direct(Child),
    Systemd(Unit),
}
impl FixtureHost {
    pub(super) fn start(root: &Path, systemd: bool) -> Result<Self> {
        if systemd {
            return Ok(Self::Systemd(Unit::start(root)?));
        }
        Ok(Self::Direct(
            clean_command(&root.join("bin/treer-agent-host"), root)
                .args(["run", "--config"])
                .arg(root.join("xdg/treer/agent-servers/srv_native_acceptance-host.json"))
                .spawn()?,
        ))
    }
    pub(super) fn image(&self) -> Result<ProcessImage> {
        let image = ProcessImage::capture(match self {
            Self::Direct(c) => c.id(),
            Self::Systemd(u) => u.pid,
        })?;
        // For systemd this also refuses an env wrapper that has not exec'd Host.
        Ok(image)
    }
    pub(super) fn verify(&self, image: &ProcessImage) -> Result<()> {
        image.verify_current()?;
        if let Self::Systemd(unit) = self {
            unit.verify()?;
        }
        Ok(())
    }
    pub(super) fn exited(&mut self) -> Result<bool> {
        match self {
            Self::Direct(c) => Ok(c.try_wait()?.is_some()),
            Self::Systemd(u) => {
                u.verify()?;
                Ok(false)
            }
        }
    }
    pub(super) fn controller(&self, host: &ProcessImage, digest: &str) -> Result<ProcessImage> {
        self.verify(host)?;
        let mut matches = Vec::new();
        for child in host.children()? {
            if super::super::super::images::digest_file(child.executable())? == digest {
                child.verify_parent(host)?;
                if let Self::Systemd(unit) = self {
                    if cgroup(child.pid())? != unit.group {
                        bail!("hosted_manager_controller_cgroup_changed");
                    }
                }
                child.verify_current()?;
                matches.push(child);
            }
        }
        if matches.len() != 1 {
            bail!("hosted_manager_controller_identity_unavailable");
        }
        Ok(matches.remove(0))
    }
    pub(super) fn release_submit(&self, directory: &PrivateDirectory) -> Result<()> {
        write_json(
            directory,
            "native-manager-ready.json",
            &serde_json::json!({"schema":1,"ready":true}),
        )
    }
    pub(super) async fn wait_active(&self) -> Result<()> {
        if let Self::Systemd(unit) = self {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                unit.verify()?;
                if field(&show(&unit.name)?, "ActiveState")? == "active" {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    bail!("hosted_manager_post_start_timeout");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        Ok(())
    }
    pub(super) async fn stop(&mut self, image: &ProcessImage) -> Result<bool> {
        if let Self::Systemd(unit) = self {
            image.verify_current()?;
            let empty = unit.stop().await?;
            let same_host = ProcessImage::capture(image.pid())
                .is_ok_and(|current| current.creation_identity() == image.creation_identity());
            return Ok(empty && !same_host);
        }
        let Self::Direct(child) = self else {
            unreachable!()
        };
        if child.try_wait()?.is_none() {
            image.verify_current()?;
            rustix::process::kill_process(
                rustix::process::Pid::from_raw(child.id() as i32).unwrap(),
                rustix::process::Signal::TERM,
            )?;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if child.try_wait()?.is_some() {
                return Ok(true);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[test]
#[ignore = "only fixed ExecStartPost helper in explicit hosted systemd fixture"]
fn hosted_systemd_submit_helper() {
    let root = super::super::root().expect("hosted effective account required");
    let directory = PrivateDirectory::open(&root).unwrap();
    write_json(
        &directory,
        "native-manager-helper-intent.json",
        &serde_json::json!({"schema":1,"submit_once":true}),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        match read_json::<serde_json::Value>(&directory, "native-manager-ready.json") {
            Ok(v) => {
                assert_eq!(v, serde_json::json!({"schema":1,"ready":true}));
                break;
            }
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
            Err(_) => panic!("hosted manager ready invalid"),
        }
        assert!(Instant::now() < deadline, "hosted manager ready timeout");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut command = clean_command(&root.join("bin/treer-agent-server"), &root);
    command
        .env("TREER_ACCEPTANCE_SYSTEMD", "1")
        .arg("--hosted-native-update-acceptance");
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(420);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "hosted manager submit failed");
            break;
        }
        // Never resend or kill uncertain submit; unit owner retains cleanup duty.
        assert!(Instant::now() < deadline, "hosted manager submit timeout");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn manager_group_and_command_controls_are_fail_closed() {
    let unit = "treer-native-0123456789abcdef.service";
    assert!(checked_group(&format!("/user.slice/app.slice/{unit}"), unit).is_ok());
    for group in [
        "/other.service".to_owned(),
        format!("/../{unit}"),
        format!("//{unit}"),
        format!("relative/{unit}"),
    ] {
        assert!(checked_group(&group, unit).is_err());
    }
    assert_eq!(quote("a%b$c\\d\""), "\"a%%b$$c\\\\d\\\"\"");
    assert_eq!(
        HELPER,
        "self_update::hosted_acceptance::fixture_tests::manager::hosted_systemd_submit_helper"
    );
}

#[test]
fn manager_metadata_rejects_restart_replacement_and_unowned_units() {
    let unit = Unit {
        name: "treer-native-0123456789abcdef.service".into(),
        group: "/slice/treer-native-0123456789abcdef.service".into(),
        invocation: "a".repeat(32),
        pid: 42,
    };
    let values: BTreeMap<String, String> = [
        ("Id", unit.name.as_str()),
        ("MainPID", "42"),
        ("ControlGroup", unit.group.as_str()),
        ("InvocationID", unit.invocation.as_str()),
        ("NRestarts", "0"),
        ("Transient", "yes"),
        ("KillMode", "control-group"),
        ("ActiveState", "activating"),
        ("Type", "simple"),
        ("Restart", "always"),
        ("StartLimitBurst", "1"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    unit.check_metadata(&values).unwrap();
    for (key, value) in [
        ("Id", "other.service"),
        ("MainPID", "43"),
        ("ControlGroup", "/other"),
        ("InvocationID", "bbbb"),
        ("NRestarts", "1"),
        ("Transient", "no"),
        ("KillMode", "process"),
        ("ActiveState", "failed"),
        ("Type", "oneshot"),
        ("Restart", "no"),
        ("StartLimitBurst", "5"),
    ] {
        let mut changed = values.clone();
        changed.insert(key.into(), value.into());
        assert!(unit.check_metadata(&changed).is_err(), "{key}");
        changed.remove(key);
        assert!(unit.check_metadata(&changed).is_err(), "missing {key}");
    }
}
