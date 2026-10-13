use super::*;

fn test_root() -> std::path::PathBuf {
    #[cfg(unix)]
    let base = std::path::PathBuf::from("/tmp");
    #[cfg(windows)]
    let base = std::env::temp_dir();
    let root = base.join(format!("trr-ee-{}", Uuid::new_v4().simple()));
    treer_platform::secure_directory(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

async fn runtime_for_exec(root: &Path) -> (ControllerRuntime, tokio::task::JoinHandle<()>) {
    #[cfg(unix)]
    let socket = root.join("exec-host.sock");
    #[cfg(windows)]
    let socket = treer_platform::endpoint_name(
        &Uuid::new_v4().to_string(),
        &treer_platform::current_user_sid().unwrap(),
    );
    let mut listener = treer_platform::LocalListener::bind(&socket).unwrap();
    let task = tokio::spawn(async move {
        let _stream = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let (host, events) = HostClient::connect(&socket).await.unwrap();
    let (runtime, _) = ControllerRuntime::from_sync(
        host,
        HostResponse::Synced {
            host_epoch: "exec-test".into(),
            host_build: treer_host_protocol::HostBuildInfo {
                version: "fixture".into(),
                git_commit: "fixture".into(),
            },
            processes: vec![],
            replay: vec![],
        },
        events,
        ControllerConfig {
            protected_paths: vec![],
            workspace_id: "fixture-workspace".into(),
            server_id: "fixture-server".into(),
            agent_server_url: "http://127.0.0.1:1".into(),
            proxy_public_url: "https://proxy.invalid/".into(),
            treer_binary: None,
            interface_cache_path: root.join("interfaces.json"),
            startup_store_path: root.join("startup.json"),
            root: root.to_path_buf(),
        },
    )
    .unwrap();
    (runtime, task)
}

#[test]
fn machine_exec_does_not_inherit_credentials_or_inject_empty_identity_variables() {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child.args([
        "--exact",
        "controller::exec_environment_tests::machine_exec_polluted_environment_fixture",
        "--ignored",
        "--nocapture",
    ]);
    for name in [
        "TREER_MACHINE_TOKEN",
        "TREER_OPERATOR_CREDENTIAL",
        "TREER_ENROLLMENT_KEY",
        "TREER_WORKLOAD_CREDENTIAL",
        "TREER_AGENT_ID",
        "TREER_BIN",
        "AGENT_UI_INSTANCE_ID",
        "CODEX_UI_THREAD_ID",
    ] {
        child.env(name, "synthetic-not-a-real-credential");
    }
    child
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("PSModulePath", "synthetic-user-writable-module-path");
    let output = child.output().unwrap();
    assert!(
        output.status.success(),
        "fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(windows)]
#[test]
fn windows_machine_exec_identity_boundary() {
    machine_exec_does_not_inherit_credentials_or_inject_empty_identity_variables();
}

#[tokio::test]
#[ignore = "isolated subprocess fixture, invoked by the polluted environment test"]
async fn machine_exec_polluted_environment_fixture() {
    let root = test_root();
    let (runtime, host) = runtime_for_exec(&root).await;
    let response = runtime
        .exec_machine(MachineExecRequest {
            cwd: ".".into(),
            command: std::env::current_exe().unwrap().display().to_string(),
            args: vec![
                "--exact".into(),
                "controller::exec_environment_tests::machine_exec_child_fixture".into(),
                "--ignored".into(),
                "--nocapture".into(),
            ],
            timeout_ms: 30_000,
        })
        .await
        .unwrap();
    assert_eq!(
        response.exit_code,
        Some(0),
        "child environment assertions failed: {} {}",
        response.stdout,
        response.stderr
    );
    for kind in ["agent", "app"] {
        let env = runtime.process_environment(Some((kind, "synthetic-fresh-workload-credential")));
        for name in [
            "TREER_MACHINE_TOKEN",
            "TREER_OPERATOR_CREDENTIAL",
            "TREER_ENROLLMENT_KEY",
        ] {
            assert!(!env.contains_key(name), "{kind} contains {name}");
        }
        assert_eq!(
            env.get("TREER_WORKLOAD_CREDENTIAL").map(String::as_str),
            Some("synthetic-fresh-workload-credential")
        );
        assert_eq!(env.get("TREER_AGENT_ID").map(String::as_str), Some(kind));
    }
    host.abort();
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "child of the actual Controller exec path"]
fn machine_exec_child_fixture() {
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy().to_ascii_uppercase();
        assert!(
            !name.starts_with("TREER_")
                && !name.starts_with("AGENT_UI_")
                && !name.starts_with("CODEX_UI_"),
            "unexpected identity variable name {name}"
        );
        assert!(
            !["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NODE_OPTIONS"].contains(&name.as_str()),
            "unexpected ambient control variable {name}"
        );
    }
    #[cfg(windows)]
    for name in [
        "SystemRoot",
        "windir",
        "USERPROFILE",
        "USERNAME",
        "APPDATA",
        "LOCALAPPDATA",
        "ProgramData",
        "ProgramFiles",
        "COMPUTERNAME",
    ] {
        assert!(
            std::env::var_os(name).is_some(),
            "missing Windows baseline {name}"
        );
    }
    #[cfg(windows)]
    {
        let root = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let expected = root
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("Modules");
        assert!(
            std::env::var_os("PSModulePath") == Some(expected.into_os_string()),
            "Controller child module path is not the fixed system directory"
        );
    }
}

#[cfg(windows)]
#[tokio::test]
async fn windows_machine_exec_node_and_powershell_keep_os_baseline() {
    let root = test_root();
    let (runtime, host) = runtime_for_exec(&root).await;
    let environment = runtime.machine_exec_environment();
    let system = PathBuf::from(environment.get(std::ffi::OsStr::new("SystemRoot")).unwrap());
    let modules = system
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("Modules");
    assert!(
        environment.get(std::ffi::OsStr::new("PSModulePath")) == Some(&modules.into_os_string()),
        "initial PSModulePath must equal the fixed system directory"
    );
    eprintln!(
        "windows-exec child-variable-names={:?}",
        environment.keys().collect::<Vec<_>>()
    );
    for (index, (command,args,expected)) in [
        ("node",vec!["-e","const assert=require('node:assert'); for(const k of ['SystemRoot','windir','USERPROFILE','APPDATA','LOCALAPPDATA']) assert.ok(process.env[k],k); assert.ok(!Object.keys(process.env).some(k=>k.toUpperCase().startsWith('TREER_'))); console.log(1+1)"],"2"),
        ("powershell.exe",vec!["-NoProfile","-NonInteractive","-Command","if (!$PSVersionTable.PSVersion) { exit 2 }; Write-Output 'powershell-version-ok'; exit 0"],"powershell-version-ok"),
        ("powershell.exe",vec!["-NoProfile","-NonInteractive","-Command","if (!$env:SystemRoot -or !$env:USERPROFILE -or !$env:PSModulePath) { exit 2 }; Write-Output 'powershell-baseline-ok'; exit 0"],"powershell-baseline-ok"),
        ("node",vec!["-e","const a=require('node:assert'); const p=require('node:path'); a.equal(process.env.PSModulePath,p.win32.join(process.env.SystemRoot,'System32','WindowsPowerShell','v1.0','Modules')); console.log('node-module-path-equal=True')"],"node-module-path-equal=True"),
        ("powershell.exe",vec!["-NoProfile","-NonInteractive","-Command","$expected=[IO.Path]::Combine($env:SystemRoot,'System32\\WindowsPowerShell\\v1.0\\Modules'); $contains=@($env:PSModulePath -split ';' | Where-Object { [string]::Equals($_,$expected,[StringComparison]::OrdinalIgnoreCase) }).Count -gt 0; Write-Output ('powershell-system-module-present=' + $contains); if (!$contains) { exit 3 }; exit 0"],"powershell-system-module-present=True"),
        ("powershell.exe",vec!["-NoProfile","-NonInteractive","-Command","if (!$env:SystemRoot -or !$env:USERPROFILE) { exit 2 }; [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes((New-Object byte[] 16)); Write-Output 'powershell-ok'"],"powershell-ok"),
        ("tasklist.exe",vec!["/FI","PID eq 0","/NH"],""),
    ].into_iter().enumerate() {
        let response = runtime.exec_machine(MachineExecRequest { cwd:".".into(),command:command.into(),args:args.into_iter().map(str::to_owned).collect(),timeout_ms:30_000 }).await.unwrap();
        eprintln!("windows-exec probe={index} marker={expected} duration_ms={} timeout={} exit={:?}", response.duration_ms, response.timed_out, response.exit_code);
        assert!(!response.timed_out, "probe={index} marker={expected} {command} timed out after {}ms: exit={:?} stdout={:?} stderr={:?}",response.duration_ms,response.exit_code,response.stdout,response.stderr);
        assert_eq!(response.exit_code,Some(0),"{command} failed after {}ms: stdout={:?} stderr={:?}",response.duration_ms,response.stdout,response.stderr);
        assert!(response.stdout.contains(expected),"{command} did not complete its smoke check");
    }
    host.abort();
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}

/// Diagnostic only: run on a fresh native runner with the artifact job's exact
/// preparation steps. Never substitutes for the unchanged strict 30s gate.
#[cfg(windows)]
#[tokio::test]
#[ignore = "read-only native artifact environment diagnosis; not a release gate"]
async fn windows_artifact_powershell_diagnostics() {
    let root = test_root();
    let (runtime, host) = runtime_for_exec(&root).await;
    let names = |environment: BTreeMap<std::ffi::OsString, std::ffi::OsString>| {
        environment.into_keys().collect::<Vec<_>>()
    };
    eprintln!(
        "artifact-diagnostic parent-variable-names={:?}",
        names(std::env::vars_os().collect())
    );
    eprintln!(
        "artifact-diagnostic child-variable-names={:?}",
        names(runtime.machine_exec_environment())
    );
    let environment = runtime.machine_exec_environment();
    let system = PathBuf::from(environment.get(std::ffi::OsStr::new("SystemRoot")).unwrap());
    let absolute = system
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    // Only synthetic flags/markers and numeric PS version are printed. Do not
    // print environment values, paths, $Error or exception messages.
    for (label, command, args) in [
        ("initial-environment", "node".to_string(), vec!["-e".into(), "const p=require('node:path'); console.log('child-variable-names='+JSON.stringify(Object.keys(process.env).sort())); console.log('fixed-module-equal='+(process.env.PSModulePath===p.win32.join(process.env.SystemRoot,'System32','WindowsPowerShell','v1.0','Modules')));".into()]),
        ("utility-version", "powershell.exe".into(), vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), "Write-Output 'utility-version-ok'; exit 0".into()]),
        ("startup-exit", "powershell.exe".into(), vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), "exit 0".into()]),
        ("console-version", "powershell.exe".into(), vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), "[Console]::WriteLine($PSVersionTable.PSVersion.ToString()); exit 0".into()]),
        ("autoload-disabled", "powershell.exe".into(), vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), "$PSModuleAutoLoadingPreference='None'; [Console]::WriteLine($PSVersionTable.PSVersion.ToString()); exit 0".into()]),
        ("absolute-console-version", absolute.to_string_lossy().into_owned(), vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), "[Console]::WriteLine($PSVersionTable.PSVersion.ToString()); exit 0".into()]),
        ("rng", "powershell.exe".into(), vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), "try { [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes((New-Object byte[] 16)); [Console]::WriteLine('rng-ok'); exit 0 } catch { [Console]::WriteLine($_.Exception.GetType().Name); exit 2 }".into()]),
    ] {
        let response = runtime.exec_machine(MachineExecRequest { cwd: ".".into(), command, args, timeout_ms: 5_000 }).await.unwrap();
        // stderr is intentionally not printed: native errors can include real
        // profile paths. Probe stdout is restricted to the synthetic output above.
        eprintln!("artifact-diagnostic probe={label} duration_ms={} timeout={} exit={:?} stdout={:?} stderr_present={}", response.duration_ms, response.timed_out, response.exit_code, response.stdout, !response.stderr.is_empty());
    }
    host.abort();
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}

/// Full-parent inheritance is intentionally confined to a disposable GitHub
/// runner. Do not run this diagnostic on an enrolled machine or managed Agent.
#[cfg(windows)]
#[tokio::test]
#[ignore = "isolated CI-only parent/child environment comparison; not a release gate"]
async fn windows_artifact_powershell_environment_matrix() {
    assert!(std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true"));
    assert!(std::env::var("CI").as_deref() == Ok("true"));
    let root = test_root();
    let (runtime, host) = runtime_for_exec(&root).await;
    let baseline = runtime.machine_exec_environment();
    for name in ["USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        eprintln!(
            "environment-matrix variable={name} parent_equals_child={}",
            baseline.get(std::ffi::OsStr::new(name)) == std::env::var_os(name).as_ref()
        );
    }
    let mut variants = vec![
        ("full-parent", None),
        ("controller-baseline", Some(baseline.clone())),
    ];
    for (label, keys) in [
        ("analysis-cache-only", vec!["PSModuleAnalysisCachePath"]),
        ("module-path-only", vec!["PSModulePath"]),
        (
            "ps-group",
            vec![
                "PSModuleAnalysisCachePath",
                "PSModulePath",
                "PSDisableModuleAnalysisCacheCleanup",
                "POWERSHELL_DISTRIBUTION_CHANNEL",
                "POWERSHELL_UPDATECHECK",
            ],
        ),
        (
            "os-extra-group",
            vec![
                "PUBLIC",
                "PROCESSOR_IDENTIFIER",
                "SESSIONNAME",
                "LOGONSERVER",
                "DriverData",
            ],
        ),
        (
            "combined-extra-group",
            vec![
                "PSModuleAnalysisCachePath",
                "PSModulePath",
                "PSDisableModuleAnalysisCacheCleanup",
                "POWERSHELL_DISTRIBUTION_CHANNEL",
                "POWERSHELL_UPDATECHECK",
                "PUBLIC",
                "PROCESSOR_IDENTIFIER",
                "SESSIONNAME",
                "LOGONSERVER",
                "DriverData",
            ],
        ),
    ] {
        let mut environment = baseline.clone();
        let mut added = vec![];
        for key in keys {
            if let Some(value) = std::env::var_os(key) {
                // This is a diagnostic opt-in, not a production allowlist.
                // Names are case insensitive on Windows: replace the old key.
                environment.retain(|name, _| !name.to_string_lossy().eq_ignore_ascii_case(key));
                environment.insert(key.into(), value);
                added.push(key);
            }
        }
        eprintln!("environment-matrix variant={label} added-variable-names={added:?}");
        variants.push((label, Some(environment)));
    }
    let mut disabled_cache = baseline.clone();
    disabled_cache.insert("PSModuleAnalysisCachePath".into(), "NUL".into());
    variants.push(("analysis-cache-disabled", Some(disabled_cache)));
    let mut previous_size = None;
    let mut cache_state = || {
        let default_cache = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|path| {
                path.join("Microsoft")
                    .join("Windows")
                    .join("PowerShell")
                    .join("ModuleAnalysisCache")
            });
        let configured_cache = std::env::var_os("PSModuleAnalysisCachePath").map(PathBuf::from);
        let cryptnet = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .map(|path| {
                path.join("AppData")
                    .join("LocalLow")
                    .join("Microsoft")
                    .join("CryptnetUrlCache")
            });
        let size = default_cache
            .as_ref()
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|metadata| metadata.len());
        eprintln!("environment-matrix default-cache-exists={} default-cache-parent-exists={} configured-cache-exists={} default-cache-size-increased={} cryptnet-cache-exists={} cryptnet-cache-has-entries={}",
            default_cache.as_ref().is_some_and(|path| path.is_file()),
            default_cache.as_ref().and_then(|path| path.parent()).is_some_and(|path| path.is_dir()),
            configured_cache.as_ref().is_some_and(|path| path.is_file()),
            size.zip(previous_size).is_some_and(|(current, previous)| current > previous),
            cryptnet.as_ref().is_some_and(|path| path.is_dir()),
            cryptnet.as_ref().and_then(|path| std::fs::read_dir(path).ok()).is_some_and(|mut entries| entries.next().is_some()));
        previous_size = size;
    };
    // Same argv/cwd/NUL/pipes/deadline for direct Rust and Controller. Output is
    // captured only to test synthetic markers, never printed (even on failure).
    let script = "Write-Output 'utility-matrix-ok'; [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes((New-Object byte[] 16)); [Console]::WriteLine('rng-matrix-ok'); exit 0";
    for (label, environment) in variants {
        for repetition in 1..=2 {
            cache_state();
            let mut command = tokio::process::Command::new("powershell.exe");
            command
                .args(["-NoProfile", "-NonInteractive", "-Command", script])
                .current_dir(&root)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            if let Some(environment) = &environment {
                command.env_clear().envs(environment);
            }
            let started = Instant::now();
            match command.spawn() {
                Ok(child) => {
                    match tokio::time::timeout(Duration::from_millis(30_000), child.wait_with_output()).await {
                        Ok(Ok(output)) => {
                            let stdout = String::from_utf8_lossy(&output.stdout);
                            eprintln!("environment-matrix variant={label} repetition={repetition} duration_ms={} timeout=false exit={:?} utility_ok={} rng_ok={} stderr_present={}", started.elapsed().as_millis(), output.status.code(), stdout.contains("utility-matrix-ok"), stdout.contains("rng-matrix-ok"), !output.stderr.is_empty());
                        }
                        Ok(Err(_)) => eprintln!("environment-matrix variant={label} repetition={repetition} wait_failed=true"),
                        Err(_) => eprintln!("environment-matrix variant={label} repetition={repetition} duration_ms={} timeout=true", started.elapsed().as_millis()),
                    }
                }
                Err(_) => eprintln!(
                    "environment-matrix variant={label} repetition={repetition} spawn_failed=true"
                ),
            }
            cache_state();
        }
    }
    // Direct baseline and real Controller are separate controls; neither the
    // full-parent diagnostic nor optional variables modify Controller behavior.
    for repetition in 1..=2 {
        let response = runtime
            .exec_machine(MachineExecRequest {
                cwd: ".".into(),
                command: "powershell.exe".into(),
                args: vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    script.into(),
                ],
                timeout_ms: 30_000,
            })
            .await
            .unwrap();
        eprintln!("environment-matrix variant=actual-controller repetition={repetition} duration_ms={} timeout={} exit={:?} utility_ok={} rng_ok={} stderr_present={}", response.duration_ms, response.timed_out, response.exit_code, response.stdout.contains("utility-matrix-ok"), response.stdout.contains("rng-matrix-ok"), !response.stderr.is_empty());
    }
    host.abort();
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "isolated CI-only fresh private cache feasibility; not a release gate"]
async fn windows_artifact_powershell_fresh_private_cache() {
    use std::os::windows::fs::MetadataExt;
    assert!(std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true"));
    assert!(std::env::var("CI").as_deref() == Ok("true"));
    fn reject_reparse(path: &Path) {
        assert!(path.is_absolute(), "diagnostic directory must be absolute");
        let mut current = PathBuf::new();
        for component in path.components() {
            assert!(!matches!(component, std::path::Component::ParentDir));
            current.push(component);
            if matches!(component, std::path::Component::Prefix(_)) {
                continue;
            }
            if let Ok(metadata) = std::fs::symlink_metadata(&current) {
                assert!(
                    !metadata.is_symlink() && metadata.file_attributes() & 0x400 == 0,
                    "diagnostic path must not contain reparse points"
                );
            }
        }
    }
    let root = test_root();
    let (runtime, host) = runtime_for_exec(&root).await;
    let mut environment = runtime.machine_exec_environment();
    let state = treer_platform::config_directory()
        .unwrap()
        .parent()
        .unwrap()
        .join("state");
    let private = state.join(format!("ps-cache-diagnostic-{}", Uuid::new_v4().simple()));
    reject_reparse(&private);
    treer_platform::secure_directory(&private).unwrap();
    reject_reparse(&private);
    let cache = private.join("ModuleAnalysisCache");
    assert!(!cache.exists(), "fresh diagnostic cache must not exist");
    environment.retain(|key, _| {
        !key.to_string_lossy()
            .eq_ignore_ascii_case("PSModuleAnalysisCachePath")
    });
    environment.insert(
        "PSModuleAnalysisCachePath".into(),
        cache.clone().into_os_string(),
    );
    let default_parent = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap()
        .join("Microsoft")
        .join("Windows")
        .join("PowerShell");
    let default_parent_existed = default_parent.is_dir();
    reject_reparse(&default_parent);
    let default_parent_creatable = std::fs::create_dir_all(&default_parent).is_ok();
    reject_reparse(&default_parent);
    let probe = default_parent.join(format!("treer-write-probe-{}", Uuid::new_v4().simple()));
    let default_parent_writable = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            std::fs::remove_file(&probe).unwrap();
            true
        }
        Err(_) => false,
    };
    eprintln!("fresh-private-cache default-parent-existed={default_parent_existed} default-parent-creatable={default_parent_creatable} default-parent-writable={default_parent_writable} initial-file-exists=false");
    let mut previous_size = None;
    let script = "Write-Output 'private-utility-ok'; [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes((New-Object byte[] 16)); [Console]::WriteLine('private-rng-ok'); exit 0";
    for repetition in 1..=3 {
        let before = std::fs::metadata(&cache)
            .ok()
            .map(|metadata| metadata.len());
        let mut command = tokio::process::Command::new("powershell.exe");
        command
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .current_dir(&root)
            .env_clear()
            .envs(&environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let started = Instant::now();
        match tokio::time::timeout(
            Duration::from_millis(30_000),
            command.spawn().unwrap().wait_with_output(),
        )
        .await
        {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                eprintln!("fresh-private-cache repetition={repetition} duration_ms={} timeout=false exit={:?} utility_ok={} rng_ok={} stderr_present={}", started.elapsed().as_millis(), output.status.code(), stdout.contains("private-utility-ok"), stdout.contains("private-rng-ok"), !output.stderr.is_empty());
            }
            Ok(Err(_)) => eprintln!("fresh-private-cache repetition={repetition} wait_failed=true"),
            Err(_) => eprintln!(
                "fresh-private-cache repetition={repetition} duration_ms={} timeout=true",
                started.elapsed().as_millis()
            ),
        }
        let after = std::fs::metadata(&cache)
            .ok()
            .map(|metadata| metadata.len());
        eprintln!("fresh-private-cache repetition={repetition} file-existed-before={} file-exists-after={} size-increased={} grew-since-last={}", before.is_some(), after.is_some(), after.zip(before).is_some_and(|(a,b)| a>b), after.zip(previous_size).is_some_and(|(a,b)| a>b));
        previous_size = after;
    }
    // Check DACL only after feasibility probes, avoiding a PS warm-up first.
    // The existing helper grants only current token SID + SYSTEM. No SID/path
    // or original stdout/stderr is printed, including on assertion failure.
    let acl_script = "$acl=[IO.Directory]::GetAccessControl($env:DIAGNOSTIC_CACHE_DIRECTORY); $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value; $ok=$acl.AreAccessRulesProtected; $rules=$acl.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier]); foreach($rule in $rules) { if($rule.IdentityReference.Value -ne $sid -and $rule.IdentityReference.Value -ne 'S-1-5-18') {$ok=$false} }; [Console]::WriteLine('private-acl-only-owner-system='+$ok); if(!$ok){exit 2}; exit 0";
    let acl = tokio::time::timeout(
        Duration::from_millis(30_000),
        tokio::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", acl_script])
            .env_clear()
            .envs(&environment)
            .env("DIAGNOSTIC_CACHE_DIRECTORY", &private)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("private ACL diagnostic timed out")
    .expect("private ACL diagnostic failed");
    assert!(
        acl.status.success()
            && String::from_utf8_lossy(&acl.stdout).contains("private-acl-only-owner-system=True"),
        "private cache ACL assertion failed"
    );
    eprintln!("fresh-private-cache private-acl-only-owner-system=true");
    host.abort();
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(private).unwrap();
}
