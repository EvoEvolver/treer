//! Controller-side provider CLI tests. A fixture `npm` shell script (no network,
//! no real package) records argv and environment, copies a fixture tarball on
//! `pack`, succeeds or misbehaves on `install` per marker files, and prints a
//! fixture global prefix. The recipe table under test is the embedded release
//! table with the Codex integrity replaced by the fixture tarball's SHA-512.
use super::*;
use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use treer_protocol::{
    decide_provider_cli_action, ProviderCliObservedState, ProviderCliVersionRelation,
    PROVIDER_CLI_RETAINED_OPERATIONS, PROVIDER_RECIPES_TABLE,
};

const TARBALL: &[u8] = b"fixture tarball bytes: not a real package\n";
/// The embedded Codex recipe's pinned version; the fixture install writes a
/// binary reporting it, like a real global install would.
const PINNED: &str = "0.163.0";

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("treer-provider-cli-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
const CODEX: &str = "codex-npm-posix";

struct Fixture {
    _dir: TempDir,
    root: PathBuf,
    fixture: PathBuf,
    search_path: OsString,
    table: ValidatedProviderTable,
}

#[cfg(unix)]
fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn integrity_of(bytes: &[u8]) -> String {
    use base64::Engine;
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes))
    )
}

fn table_with_codex_integrity(integrity: &str) -> ValidatedProviderTable {
    let mut table: Value = serde_json::from_str(PROVIDER_RECIPES_TABLE).unwrap();
    let codex = table["recipes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|recipe| recipe["recipe_id"] == CODEX)
        .unwrap();
    codex["install"]["integrity"] = Value::String(integrity.to_owned());
    ValidatedProviderTable::parse(&table.to_string()).unwrap()
}

#[cfg(unix)]
fn fixture(table: ValidatedProviderTable) -> Fixture {
    let dir = TempDir::new();
    let fixture = dir.path().join("fixture");
    for sub in ["bin", "log", "prefix/bin", "prefix/lib"] {
        fs::create_dir_all(fixture.join(sub)).unwrap();
    }
    fs::write(fixture.join("tarball.tgz"), TARBALL).unwrap();
    write_executable(
        &fixture.join("bin/npm"),
        r#"#!/bin/sh
dir=$(cd "$(dirname "$0")/.." && pwd)
env > "$dir/log/env-$1"
cat "$PWD/package.json" > "$dir/log/package-$1" 2>/dev/null
ls -A "$PWD" > "$dir/log/cwd-$1"
printf '%s\n' "$@" > "$dir/log/argv-$1"
case "$1" in
  pack)
    for a in "$@"; do case "$a" in --pack-destination=*) dest=${a#--pack-destination=};; esac; done
    cp "$dir/tarball.tgz" "$dest/openai-codex-0.163.0.tgz"
    if [ -f "$dir/slow-pack" ]; then sleep 2; fi
    if [ -f "$dir/plant-on-pack" ]; then
      printf '#!/bin/sh\necho codex-cli 9.9.9\n' > "$dir/prefix/bin/codex"; chmod 755 "$dir/prefix/bin/codex"
    fi
    if [ -f "$dir/plant-on-pack-detach" ]; then
      printf '#!/bin/sh\nperl -MPOSIX -e '"'"'POSIX::setsid(); exec "sleep", "20"'"'"' &\necho codex-cli 0.162.0\n' > "$dir/prefix/bin/codex"; chmod 755 "$dir/prefix/bin/codex"
    fi
    ;;
  install)
    if [ -f "$dir/slow-install" ]; then sleep 3; fi
    if [ -f "$dir/detach-install" ]; then
      perl -MPOSIX -e 'POSIX::setsid(); exec "sleep", "20"' &
    fi
    if [ -f "$dir/fail-install" ]; then echo "EACCES fixture" >&2; exit 7; fi
    printf '#!/bin/sh\necho codex-cli 0.163.0\n' > "$dir/prefix/bin/codex"; chmod 755 "$dir/prefix/bin/codex"
    ;;
  prefix)
    if [ -f "$dir/break-prefix" ]; then echo "prefix fixture broken" >&2; exit 1; fi
    if [ -f "$dir/slow-prefix-after-pack" ] && [ -f "$dir/log/argv-pack" ]; then sleep 3; fi
    printf '%s\n' "$dir/prefix"
    ;;
esac
exit 0
"#,
    );
    // A same-named binary on PATH must never satisfy the check. The prefix
    // starts empty: tests that need an installation call `install_binary`.
    write_executable(
        &fixture.join("bin/codex"),
        "#!/bin/sh\necho decoy-on-path\n",
    );
    Fixture {
        root: dir.path().join("state/provider-cli"),
        // Fixture bin first (the decoy codex), then the system tools the
        // fixture script itself needs; production uses the allowlist PATH.
        search_path: OsString::from(format!("{}:/usr/bin:/bin", fixture.join("bin").display())),
        fixture,
        table,
        _dir: dir,
    }
}

impl Fixture {
    fn service(&self) -> ProviderCliService {
        ProviderCliService::for_test(
            self.root.clone(),
            "srv_fixture",
            self.search_path.clone(),
            self.table.clone(),
        )
    }

    fn log(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.fixture.join("log").join(name)).ok()
    }

    fn codex_ref(&self) -> ProviderCliRecipeRef {
        let entry = self
            .table
            .recipes
            .iter()
            .find(|entry| entry.recipe.recipe_id == CODEX)
            .unwrap();
        ProviderCliRecipeRef {
            recipe_id: CODEX.into(),
            recipe_version: 1,
            recipe_hash: entry.recipe_hash.clone(),
        }
    }

    fn prefix_binary(&self) -> PathBuf {
        self.fixture.join("prefix/bin/codex")
    }

    /// Simulates an existing global installation whose probe prints `line`.
    fn install_binary(&self, line: &str) {
        let _ = fs::remove_file(self.prefix_binary());
        write_executable(&self.prefix_binary(), &format!("#!/bin/sh\necho {line}\n"));
    }

    /// What the Proxy would bind after `prepare`: the Controller's own
    /// observation turned into the one action it allows.
    async fn consent(&self, service: &ProviderCliService) -> ProviderCliConsent {
        let observation = service
            .check(&self.codex_ref())
            .await
            .unwrap()
            .observation
            .expect("v2 check carries an observation");
        let decision =
            decide_provider_cli_action(&observation, PINNED).expect("prefix is observable");
        ProviderCliConsent {
            action: decision.action,
            version_relation: decision.version_relation,
            observation_hash: observation.hash(&self.codex_ref().recipe_hash, PINNED),
        }
    }
}

fn operation_id(n: u32) -> String {
    format!("pco_{n:032x}")
}

async fn wait_terminal(service: &ProviderCliService, id: &str) -> ProviderCliOperationReport {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let report = service.operation(id).unwrap();
        if report.status.is_terminal() {
            return report;
        }
        assert!(
            Instant::now() < deadline,
            "operation did not finish: {report:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn argv_of(step: &ProviderCliPlanStep) -> (&str, &[String]) {
    match step {
        ProviderCliPlanStep::Command {
            executable, args, ..
        } => (executable, args),
        other => panic!("not a command step: {other:?}"),
    }
}

#[test]
fn step_environment_removes_npm_node_and_proxy_controls() {
    let mut base = BTreeMap::new();
    for (name, value) in [
        ("PATH", "/usr/bin"),
        ("HOME", "/Users/someone"),
        ("TMPDIR", "/var/tmp"),
        ("LANG", "en_US.UTF-8"),
        ("npm_config_registry", "https://evil.example/"),
        ("NPM_CONFIG_PREFIX", "/tmp/x"),
        ("NODE_OPTIONS", "--require=/tmp/hook.js"),
        ("NODE_PATH", "/tmp"),
        ("HTTP_PROXY", "http://proxy.example"),
        ("https_proxy", "http://proxy.example"),
        ("NO_PROXY", "localhost"),
        ("SSL_CERT_FILE", "/tmp/ca.pem"),
        ("NODE_EXTRA_CA_CERTS", "/tmp/ca.pem"),
    ] {
        base.insert(OsString::from(name), OsString::from(value));
    }
    let job = Path::new("/private/job");
    let environment = step_environment(base, job, Some(OsStr::new("/fixture/bin")));
    let keys: Vec<String> = environment
        .keys()
        .map(|key| key.to_string_lossy().into_owned())
        .collect();
    for forbidden in [
        "npm_config_registry",
        "NPM_CONFIG_PREFIX",
        "NODE_OPTIONS",
        "NODE_PATH",
        "HTTP_PROXY",
        "https_proxy",
        "NO_PROXY",
        "SSL_CERT_FILE",
        "NODE_EXTRA_CA_CERTS",
    ] {
        assert!(
            !keys.contains(&forbidden.to_owned()),
            "{forbidden} leaked: {keys:?}"
        );
    }
    assert_eq!(
        environment[OsStr::new("HOME")],
        job.join("home").into_os_string()
    );
    assert_eq!(
        environment[OsStr::new("TMPDIR")],
        job.join("tmp").into_os_string()
    );
    assert_eq!(
        environment[OsStr::new("TEMP")],
        job.join("tmp").into_os_string()
    );
    assert_eq!(
        environment[OsStr::new("PATH")],
        OsString::from("/fixture/bin")
    );
    assert_eq!(
        environment[OsStr::new("LANG")],
        OsString::from("en_US.UTF-8")
    );
    assert_eq!(environment[OsStr::new("LC_ALL")], OsString::from("C.UTF-8"));
}

#[test]
fn expansion_is_typed_isolated_and_identical_for_display_and_execution() {
    let table = ValidatedProviderTable::embedded().unwrap();
    let codex = table
        .recipes
        .iter()
        .find(|entry| entry.recipe.recipe_id == CODEX)
        .unwrap();
    let job = Path::new("/state/provider-cli/jobs/codex-npm-posix");
    let plan = expand_plan(codex, "/opt/node/bin/npm", job).unwrap();
    assert_eq!(expand_plan(codex, "/opt/node/bin/npm", job).unwrap(), plan);
    assert_eq!(plan.len(), 5);
    let (npm, pack) = argv_of(&plan[0]);
    assert_eq!(npm, "/opt/node/bin/npm");
    assert_eq!(pack[0], "pack");
    assert_eq!(pack[1], "@openai/codex@0.163.0");
    assert!(pack.contains(&"--registry=https://registry.npmjs.org/".to_owned()));
    assert!(pack.contains(&"--@openai:registry=https://registry.npmjs.org/".to_owned()));
    assert!(pack.contains(&format!("--userconfig={}", job.join("npmrc").display())));
    assert!(pack.contains(&format!(
        "--globalconfig={}",
        job.join("npmrc-global").display()
    )));
    assert!(pack.contains(&format!("--cache={}", job.join("cache").display())));
    assert!(pack.contains(&"--ignore-scripts".to_owned()));
    assert!(pack.contains(&format!(
        "--pack-destination={}",
        job.join("pack").display()
    )));
    assert!(
        matches!(&plan[1], ProviderCliPlanStep::Verify { algorithm, expected, .. } if algorithm == "sha512" && expected.starts_with("sha512-"))
    );
    let (_, install) = argv_of(&plan[2]);
    assert_eq!(install[0], "install");
    assert_eq!(install[1], "--global");
    assert_eq!(
        install[2],
        job.join("pack/openai-codex-0.163.0.tgz")
            .display()
            .to_string()
    );
    assert!(install.contains(&"--ignore-scripts".to_owned()));
    assert!(install.iter().all(|arg| !arg.contains("--prefix")));
    let (_, prefix) = argv_of(&plan[3]);
    assert_eq!(prefix[..2], ["prefix", "--global"]);
    assert!(
        matches!(&plan[4], ProviderCliPlanStep::Resolve { rule, .. } if rule == "<npm prefix --global>/bin/codex --version")
    );
    let hash = plan_argv_hash(&plan);
    let other = expand_plan(codex, "/usr/local/bin/npm", job).unwrap();
    assert_ne!(
        plan_argv_hash(&other),
        hash,
        "a different npm is a different plan"
    );
    // A lifecycle-enabled entry omits --ignore-scripts from install only.
    let mut lifecycle: Value = serde_json::from_str(PROVIDER_RECIPES_TABLE).unwrap();
    let recipe = lifecycle["recipes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|recipe| recipe["recipe_id"] == CODEX)
        .unwrap();
    recipe["install"]["ignore_scripts"] = Value::Bool(false);
    recipe["lifecycle_review"] = serde_json::json!({"reviewed_at":"2026-10-16","reviewer":"security","files":[{"member":"package/install.cjs","sha256":"a".repeat(64)}]});
    let lifecycle = ValidatedProviderTable::parse(&lifecycle.to_string()).unwrap();
    let entry = lifecycle
        .recipes
        .iter()
        .find(|entry| entry.recipe.recipe_id == CODEX)
        .unwrap();
    let plan = expand_plan(entry, "/opt/node/bin/npm", job).unwrap();
    assert!(argv_of(&plan[0]).1.contains(&"--ignore-scripts".to_owned()));
    assert!(!argv_of(&plan[2]).1.contains(&"--ignore-scripts".to_owned()));
}

#[cfg(unix)]
#[tokio::test]
async fn install_runs_the_displayed_plan_privately_and_checks_from_the_prefix() {
    let fixture = fixture(table_with_codex_integrity(&integrity_of(TARBALL)));
    let service = fixture.service();
    assert!(service.capability_available());
    let recipes = service.recipes().unwrap();
    assert_eq!(recipes.platform, release_platform().unwrap());
    assert_eq!(recipes.table_hash, fixture.table.table_hash);
    let codex = recipes
        .recipes
        .iter()
        .find(|view| view.recipe_id == CODEX)
        .unwrap();
    assert!(codex.supported);
    assert_eq!(codex.binary_names, vec!["codex".to_owned()]);
    assert!(!codex.lifecycle_scripts.as_ref().unwrap().enabled);
    let plan = codex.plan.clone().unwrap();
    let argv_hash = plan_argv_hash(&plan);
    assert!(recipes
        .recipes
        .iter()
        .filter(|view| !view.supported)
        .all(|view| view.plan.is_none() && view.unsupported_reason.is_some()));

    let reference = fixture.codex_ref();
    let consent = fixture.consent(&service).await;
    assert_eq!(consent.action, ProviderCliAction::Install, "empty prefix");
    let id = operation_id(1);
    let accepted = service
        .install(&id, &reference, &recipes.table_hash, &argv_hash, &consent)
        .unwrap();
    assert_eq!(accepted.argv_hash, argv_hash);
    assert_eq!(accepted.status, ProviderCliOperationStatus::Running);
    // Redelivery is idempotent and a second job is refused while active.
    assert_eq!(
        service
            .install(&id, &reference, &recipes.table_hash, &argv_hash, &consent)
            .unwrap()
            .operation_id,
        id
    );
    let busy = service
        .install(
            &operation_id(2),
            &reference,
            &recipes.table_hash,
            &argv_hash,
            &consent,
        )
        .unwrap_err();
    assert_eq!(busy.code, "provider_cli_machine_busy");

    let report = wait_terminal(&service, &id).await;
    assert_eq!(
        report.status,
        ProviderCliOperationStatus::Succeeded,
        "{report:?}"
    );
    assert_eq!(report.exit_code, Some(0));
    assert!(!report.timed_out);
    assert_eq!(report.argv_hash, argv_hash);
    assert_eq!(report.action, Some(ProviderCliAction::Install));
    assert_eq!(report.version_relation, None);
    assert_eq!(
        report.observation_hash.as_deref(),
        Some(consent.observation_hash.as_str())
    );
    let before = report.observed_before.clone().unwrap();
    assert_eq!(before.state, ProviderCliObservedState::Absent);
    assert_eq!(before.observation_hash, consent.observation_hash);
    assert!(!serde_json::to_string(&before)
        .unwrap()
        .contains("prefix/bin"));
    let steps = report.steps.unwrap();
    assert_eq!(
        steps.iter().map(|step| step.step).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    assert!(report.stdout.contains("codex-cli 0.163.0"));

    // Executed argv equals the displayed plan item for item.
    for (index, purpose) in [(0, "pack"), (2, "install"), (3, "prefix")] {
        let (_, args) = argv_of(&plan[index]);
        let recorded: Vec<String> = fixture
            .log(&format!("argv-{purpose}"))
            .unwrap_or_else(|| panic!("{purpose} not executed"))
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(recorded, args, "{purpose}");
    }
    // The environment the fixture saw is the isolated one.
    let env = fixture.log("env-install").unwrap();
    let job = fixture.root.join("jobs").join(CODEX);
    assert!(env.contains(&format!("HOME={}", job.join("home").display())));
    assert!(env.contains(&format!("TMPDIR={}", job.join("tmp").display())));
    for forbidden in [
        "npm_config_",
        "NODE_OPTIONS",
        "NODE_PATH",
        "_PROXY=",
        "_proxy=",
        "TREER_",
    ] {
        assert!(
            !env.contains(forbidden),
            "{forbidden} leaked into step env:\n{env}"
        );
    }
    // npm's project lookup is pinned to the private directory.
    assert_eq!(
        fixture.log("package-install").unwrap().trim(),
        "{\"private\":true}"
    );
    let cwd_listing = fixture.log("cwd-install").unwrap();
    for entry in [
        "npmrc",
        "npmrc-global",
        "package.json",
        "home",
        "tmp",
        "cache",
        "pack",
    ] {
        assert!(
            cwd_listing.lines().any(|line| line == entry),
            "{entry} missing: {cwd_listing}"
        );
    }
    // The check ran the prefix's binary, not the decoy on PATH.
    assert!(!report.stdout.contains("decoy-on-path"));
    let record_plan = service
        .inner
        .store
        .lock()
        .unwrap()
        .get(&id)
        .unwrap()
        .plan
        .clone();
    let (check_executable, check_args) = argv_of(&record_plan[4]);
    assert_eq!(
        check_executable,
        fixture
            .fixture
            .join("prefix/bin/codex")
            .display()
            .to_string()
    );
    assert_eq!(check_args, ["--version"]);
    // The job directory is gone; only the private result remains.
    assert!(!job.exists());
    let store = fixture.root.join("operations.json");
    assert!(store.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&store).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&fixture.root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    // Persisted results survive a restart; a running job would not be assumed done.
    let reloaded = fixture.service();
    assert_eq!(
        reloaded.operation(&id).unwrap().status,
        ProviderCliOperationStatus::Succeeded
    );
}

#[cfg(unix)]
#[tokio::test]
async fn integrity_mismatch_aborts_before_install() {
    // Real registry integrity, fixture tarball: step 2 must refuse.
    let fixture = fixture(ValidatedProviderTable::embedded().unwrap());
    let service = fixture.service();
    let recipes = service.recipes().unwrap();
    let codex = recipes
        .recipes
        .iter()
        .find(|view| view.recipe_id == CODEX)
        .unwrap();
    let argv_hash = plan_argv_hash(codex.plan.as_ref().unwrap());
    let id = operation_id(3);
    let consent = fixture.consent(&service).await;
    service
        .install(
            &id,
            &fixture.codex_ref(),
            &recipes.table_hash,
            &argv_hash,
            &consent,
        )
        .unwrap();
    let report = wait_terminal(&service, &id).await;
    assert_eq!(report.status, ProviderCliOperationStatus::Failed);
    assert_eq!(
        report.error_code.as_deref(),
        Some("provider_cli_integrity_mismatch")
    );
    assert_eq!(report.step, Some(2));
    assert!(fixture.log("argv-pack").is_some());
    assert!(
        fixture.log("argv-install").is_none(),
        "install must not run"
    );
    assert!(!fixture.root.join("jobs").join(CODEX).exists());
}

#[cfg(unix)]
#[tokio::test]
async fn install_refuses_changed_plan_table_unsupported_and_capped_storage() {
    let fixture = fixture(table_with_codex_integrity(&integrity_of(TARBALL)));
    let service = fixture.service();
    let recipes = service.recipes().unwrap();
    let codex = recipes
        .recipes
        .iter()
        .find(|view| view.recipe_id == CODEX)
        .unwrap();
    let argv_hash = plan_argv_hash(codex.plan.as_ref().unwrap());
    let reference = fixture.codex_ref();
    let table_hash = recipes.table_hash.clone();
    let consent = fixture.consent(&service).await;
    let refuse = |id: &str, reference: &ProviderCliRecipeRef, table: &str, hash: &str| {
        service
            .install(id, reference, table, hash, &consent)
            .unwrap_err()
            .code
    };
    assert_eq!(
        refuse(&operation_id(4), &reference, &table_hash, &"0".repeat(64)),
        "provider_cli_plan_changed"
    );
    assert_eq!(
        refuse(&operation_id(4), &reference, &"0".repeat(64), &argv_hash),
        "provider_cli_table_mismatch"
    );
    assert_eq!(
        refuse("not-an-id", &reference, &table_hash, &argv_hash),
        "provider_cli_recipe_invalid"
    );
    let mut wrong = reference.clone();
    wrong.recipe_hash = "0".repeat(64);
    assert_eq!(
        refuse(&operation_id(4), &wrong, &table_hash, &argv_hash),
        "provider_cli_recipe_mismatch"
    );
    let claude = recipes
        .recipes
        .iter()
        .find(|view| view.recipe_id == "claude-native-npm")
        .unwrap();
    let claude_ref = ProviderCliRecipeRef {
        recipe_id: claude.recipe_id.clone(),
        recipe_version: claude.recipe_version,
        recipe_hash: claude.recipe_hash.clone(),
    };
    assert_eq!(
        refuse(&operation_id(4), &claude_ref, &table_hash, &argv_hash),
        "provider_cli_recipe_unsupported"
    );
    assert_eq!(
        service.check(&claude_ref).await.unwrap_err().code,
        "provider_cli_recipe_unsupported"
    );
    // Consent shapes that can never run: noop and a malformed hash.
    let mut noop = consent.clone();
    noop.action = ProviderCliAction::Noop;
    assert_eq!(
        service
            .install(&operation_id(4), &reference, &table_hash, &argv_hash, &noop)
            .unwrap_err()
            .code,
        "provider_cli_consent_invalid"
    );
    let mut malformed = consent.clone();
    malformed.observation_hash = "zz".into();
    assert_eq!(
        service
            .install(
                &operation_id(4),
                &reference,
                &table_hash,
                &argv_hash,
                &malformed
            )
            .unwrap_err()
            .code,
        "provider_cli_consent_invalid"
    );
    assert!(
        fixture.log("argv-pack").is_none(),
        "refusals execute nothing"
    );

    // Retention cap: 64 retained results refuse a new job instead of evicting.
    {
        let mut store = service.inner.store.lock().unwrap();
        for n in 0..PROVIDER_CLI_RETAINED_OPERATIONS as u32 {
            store.operations.push(StoredOperation {
                operation_id: operation_id(1000 + n),
                recipe: reference.clone(),
                table_hash: table_hash.clone(),
                argv_hash: argv_hash.clone(),
                status: ProviderCliOperationStatus::Failed,
                step: Some(1),
                created_at_ms: now_ms(),
                finished_at_ms: Some(now_ms()),
                elapsed_ms: 1,
                exit_code: Some(1),
                timed_out: false,
                error_code: Some("provider_cli_pack_failed".into()),
                steps: Vec::new(),
                plan: Vec::new(),
                consent: None,
                observed_before: None,
            });
        }
        store.persist().unwrap();
    }
    assert_eq!(
        refuse(&operation_id(5), &reference, &table_hash, &argv_hash),
        "provider_cli_storage_exhausted"
    );
    assert_eq!(
        service.operation(&operation_id(1000)).unwrap().status,
        ProviderCliOperationStatus::Failed
    );
    assert_eq!(
        service.operation(&operation_id(5)).unwrap_err().code,
        "provider_cli_operation_not_found"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn check_probes_only_the_installed_prefix() {
    let fixture = fixture(table_with_codex_integrity(&integrity_of(TARBALL)));
    fixture.install_binary("codex-cli 0.163.0");
    let service = fixture.service();
    let reference = fixture.codex_ref();
    let response = service.check(&reference).await.unwrap();
    assert!(response.installed, "{response:?}");
    assert!(response.found_in_prefix);
    assert_eq!(response.version.as_deref(), Some("codex-cli 0.163.0"));
    assert_eq!(response.exit_code, Some(0));
    assert_eq!(
        response.executable.as_deref(),
        Some(fixture.prefix_binary().display().to_string().as_str())
    );
    assert!(!fixture.root.join("checks").join(CODEX).exists());
    // Only the prefix binary is consulted: the PATH decoy never answers.
    fs::remove_file(fixture.prefix_binary()).unwrap();
    let response = service.check(&reference).await.unwrap();
    assert!(!response.installed);
    assert!(!response.found_in_prefix);
    assert_eq!(
        response.error_code.as_deref(),
        Some("provider_cli_binary_not_found")
    );
    assert!(!response.stdout.contains("decoy"));
    assert_eq!(response.executable, None, "nothing probed, no path");
    // A symlink escaping the prefix occupies the slot but is never executed.
    std::os::unix::fs::symlink(fixture.fixture.join("bin/codex"), fixture.prefix_binary()).unwrap();
    let response = service.check(&reference).await.unwrap();
    assert!(!response.installed && response.found_in_prefix);
    assert_eq!(
        response.error_code.as_deref(),
        Some("provider_cli_binary_outside_prefix")
    );
    assert!(!response.stdout.contains("decoy"));
    // A failing probe is "not installed" with its exit code, not an error.
    fixture.install_binary("broken >&2; exit 3");
    let response = service.check(&reference).await.unwrap();
    assert!(!response.installed && response.found_in_prefix);
    assert_eq!(response.exit_code, Some(3));
    assert!(response.stderr.contains("broken"));
}

/// The observation (#134) names its evidence for every state and never
/// reads an error as absence; the decision table follows the contract.
#[cfg(unix)]
#[tokio::test]
async fn observation_classifies_the_prefix_from_evidence_only() {
    let fixture = fixture(table_with_codex_integrity(&integrity_of(TARBALL)));
    let service = fixture.service();
    let reference = fixture.codex_ref();
    let observe = |service: &ProviderCliService| {
        let reference = reference.clone();
        let service = service.clone();
        async move {
            let observation = service
                .check(&reference)
                .await
                .unwrap()
                .observation
                .unwrap();
            // Every observation this Controller emits passes the Proxy's
            // shape and consistency validation.
            assert_eq!(observation.validate(), Ok(()), "{observation:?}");
            observation
        }
    };
    let expected_path = fixture.prefix_binary().display().to_string();

    // Empty prefix: absent, with the path the install would write.
    let absent = observe(&service).await;
    assert_eq!(absent.state, ProviderCliObservedState::Absent);
    assert!(!absent.found_in_prefix);
    assert_eq!(absent.current_version, None);
    assert_eq!(absent.executable.as_deref(), Some(expected_path.as_str()));
    assert_eq!(
        absent.probe.error_code.as_deref(),
        Some("provider_cli_binary_not_found")
    );
    assert_eq!(
        decide_provider_cli_action(&absent, PINNED).unwrap().action,
        ProviderCliAction::Install
    );

    // Pinned version present: installed, and the only decision is noop.
    fixture.install_binary("codex-cli 0.163.0");
    let same = observe(&service).await;
    assert_eq!(same.state, ProviderCliObservedState::Installed);
    assert_eq!(same.current_version.as_deref(), Some("0.163.0"));
    assert_eq!(same.version_text.as_deref(), Some("codex-cli 0.163.0"));
    assert!(same.found_in_prefix);
    assert_eq!(
        decide_provider_cli_action(&same, PINNED).unwrap().action,
        ProviderCliAction::Noop
    );

    fixture.install_binary("codex-cli 0.162.0");
    let older = decide_provider_cli_action(&observe(&service).await, PINNED).unwrap();
    assert_eq!(older.action, ProviderCliAction::Replace);
    assert_eq!(
        older.version_relation,
        Some(ProviderCliVersionRelation::Upgrade)
    );

    fixture.install_binary("codex-cli 1.0.0");
    let newer = decide_provider_cli_action(&observe(&service).await, PINNED).unwrap();
    assert_eq!(
        newer.version_relation,
        Some(ProviderCliVersionRelation::Downgrade)
    );

    // A probe whose output carries no strict version is unknown-but-found.
    fixture.install_binary("codex-cli dev-build");
    let unparsed = observe(&service).await;
    assert_eq!(unparsed.state, ProviderCliObservedState::Unknown);
    assert!(unparsed.found_in_prefix);
    assert_eq!(unparsed.current_version, None);
    assert_eq!(
        unparsed.probe.error_code.as_deref(),
        Some("provider_cli_version_unparsed")
    );
    assert_eq!(
        decide_provider_cli_action(&unparsed, PINNED)
            .unwrap()
            .version_relation,
        Some(ProviderCliVersionRelation::Unknown)
    );

    // A failing probe is unknown-but-found with its exit code.
    fixture.install_binary("broken >&2; exit 3");
    let failing = observe(&service).await;
    assert_eq!(failing.state, ProviderCliObservedState::Unknown);
    assert_eq!(failing.probe.exit_code, Some(3));
    assert_eq!(
        failing.probe.error_code.as_deref(),
        Some("provider_cli_probe_failed")
    );

    // Output that pretends to be two versions yields the first token only.
    fixture.install_binary("0.164.0 0.163.0");
    let spoof = observe(&service).await;
    assert_eq!(spoof.current_version.as_deref(), Some("0.164.0"));

    // An unresolved prefix is unobservable: no state, no action.
    fs::write(fixture.fixture.join("break-prefix"), b"").unwrap();
    let unresolved = observe(&service).await;
    assert_eq!(unresolved.state, ProviderCliObservedState::Unknown);
    assert!(!unresolved.found_in_prefix);
    assert_eq!(unresolved.executable, None);
    assert_eq!(
        unresolved.probe.error_code.as_deref(),
        Some("provider_cli_prefix_unresolved")
    );
    assert!(unresolved.unobservable());
    assert_eq!(decide_provider_cli_action(&unresolved, PINNED), None);
    fs::remove_file(fixture.fixture.join("break-prefix")).unwrap();

    // The hash binds the path: a moved prefix is a different observation.
    fixture.install_binary("codex-cli 0.162.0");
    let before = observe(&service).await;
    let hash = before.hash(&reference.recipe_hash, PINNED);
    assert_eq!(
        observe(&service).await.hash(&reference.recipe_hash, PINNED),
        hash
    );
    let summary = before.summary(&reference.recipe_hash, PINNED);
    assert_eq!(summary.observation_hash, hash);
    assert!(!serde_json::to_string(&summary)
        .unwrap()
        .contains("prefix/bin"));
}

/// Consent is re-checked before anything runs: an installation that appeared
/// (or changed) after `prepare` fails closed without packing.
#[cfg(unix)]
#[tokio::test]
async fn install_refuses_when_the_prefix_changed_after_consent() {
    let fixture = fixture(table_with_codex_integrity(&integrity_of(TARBALL)));
    let service = fixture.service();
    let recipes = service.recipes().unwrap();
    let codex = recipes
        .recipes
        .iter()
        .find(|view| view.recipe_id == CODEX)
        .unwrap();
    let argv_hash = plan_argv_hash(codex.plan.as_ref().unwrap());
    let reference = fixture.codex_ref();

    // Consented to a fresh install; a newer CLI shows up before dispatch.
    let install_consent = fixture.consent(&service).await;
    assert_eq!(install_consent.action, ProviderCliAction::Install);
    fixture.install_binary("codex-cli 1.0.0");
    let id = operation_id(8);
    service
        .install(
            &id,
            &reference,
            &recipes.table_hash,
            &argv_hash,
            &install_consent,
        )
        .unwrap();
    let report = wait_terminal(&service, &id).await;
    assert_eq!(
        report.status,
        ProviderCliOperationStatus::Failed,
        "{report:?}"
    );
    assert_eq!(
        report.error_code.as_deref(),
        Some("provider_cli_state_changed")
    );
    assert!(fixture.log("argv-pack").is_none(), "nothing ran");
    assert!(fixture.log("argv-install").is_none());
    let observed = report.observed_before.clone().unwrap();
    assert_eq!(observed.state, ProviderCliObservedState::Installed);
    assert_eq!(observed.current_version.as_deref(), Some("1.0.0"));
    assert_ne!(observed.observation_hash, install_consent.observation_hash);
    assert_eq!(
        report.observation_hash.as_deref(),
        Some(install_consent.observation_hash.as_str())
    );
    assert!(!fixture.root.join("jobs").join(CODEX).exists());
    // The existing installation was not touched.
    assert!(fs::read_to_string(fixture.prefix_binary())
        .unwrap()
        .contains("1.0.0"));

    // Consented to replace 0.162.0 (upgrade); by dispatch the pinned version
    // is already there, so the action became noop: refused, not "succeeded".
    fixture.install_binary("codex-cli 0.162.0");
    let replace_consent = fixture.consent(&service).await;
    assert_eq!(replace_consent.action, ProviderCliAction::Replace);
    assert_eq!(
        replace_consent.version_relation,
        Some(ProviderCliVersionRelation::Upgrade)
    );
    fixture.install_binary("codex-cli 0.163.0");
    let id = operation_id(9);
    service
        .install(
            &id,
            &reference,
            &recipes.table_hash,
            &argv_hash,
            &replace_consent,
        )
        .unwrap();
    let report = wait_terminal(&service, &id).await;
    assert_eq!(
        report.error_code.as_deref(),
        Some("provider_cli_state_changed")
    );
    assert_eq!(
        report.observed_before.unwrap().current_version.as_deref(),
        Some("0.163.0")
    );
    assert!(fixture.log("argv-pack").is_none());
}

/// The second gate runs right before the install step: a change between pack
/// and install is refused with the tarball already verified and the prefix
/// untouched.
#[cfg(unix)]
#[tokio::test]
async fn install_refuses_when_the_prefix_changes_after_pack() {
    let fixture = fixture(table_with_codex_integrity(&integrity_of(TARBALL)));
    fs::write(fixture.fixture.join("plant-on-pack"), b"").unwrap();
    let service = fixture.service();
    let recipes = service.recipes().unwrap();
    let codex = recipes
        .recipes
        .iter()
        .find(|view| view.recipe_id == CODEX)
        .unwrap();
    let argv_hash = plan_argv_hash(codex.plan.as_ref().unwrap());
    let consent = fixture.consent(&service).await;
    assert_eq!(consent.action, ProviderCliAction::Install);
    let id = operation_id(10);
    service
        .install(
            &id,
            &fixture.codex_ref(),
            &recipes.table_hash,
            &argv_hash,
            &consent,
        )
        .unwrap();
    let report = wait_terminal(&service, &id).await;
    assert_eq!(
        report.status,
        ProviderCliOperationStatus::Failed,
        "{report:?}"
    );
    assert_eq!(
        report.error_code.as_deref(),
        Some("provider_cli_state_changed")
    );
    assert_eq!(report.step, Some(3));
    assert!(fixture.log("argv-pack").is_some(), "pack ran");
    assert!(
        fixture.log("argv-install").is_none(),
        "install must not run"
    );
    let steps: Vec<u32> = report
        .steps
        .as_ref()
        .unwrap()
        .iter()
        .map(|step| step.step)
        .collect();
    assert_eq!(steps, [1, 2], "pack and integrity recorded, nothing after");
    let observed = report.observed_before.unwrap();
    assert_eq!(observed.state, ProviderCliObservedState::Installed);
    assert_eq!(observed.current_version.as_deref(), Some("9.9.9"));
    assert!(!fixture.root.join("jobs").join(CODEX).exists());
    assert!(fs::read_to_string(fixture.prefix_binary())
        .unwrap()
        .contains("9.9.9"));
}

/// A consented replacement runs the plan and reports both the consent and the
/// observation it was re-verified against.
#[cfg(unix)]
#[tokio::test]
async fn replace_runs_when_the_observed_installation_is_unchanged() {
    let fixture = fixture(table_with_codex_integrity(&integrity_of(TARBALL)));
    fixture.install_binary("codex-cli 0.162.0");
    let service = fixture.service();
    let recipes = service.recipes().unwrap();
    let codex = recipes
        .recipes
        .iter()
        .find(|view| view.recipe_id == CODEX)
        .unwrap();
    let argv_hash = plan_argv_hash(codex.plan.as_ref().unwrap());
    let consent = fixture.consent(&service).await;
    assert_eq!(consent.action, ProviderCliAction::Replace);
    assert_eq!(
        consent.version_relation,
        Some(ProviderCliVersionRelation::Upgrade)
    );
    let id = operation_id(11);
    service
        .install(
            &id,
            &fixture.codex_ref(),
            &recipes.table_hash,
            &argv_hash,
            &consent,
        )
        .unwrap();
    let report = wait_terminal(&service, &id).await;
    assert_eq!(
        report.status,
        ProviderCliOperationStatus::Succeeded,
        "{report:?}"
    );
    assert_eq!(report.action, Some(ProviderCliAction::Replace));
    assert_eq!(
        report.version_relation,
        Some(ProviderCliVersionRelation::Upgrade)
    );
    let observed = report.observed_before.unwrap();
    assert_eq!(observed.state, ProviderCliObservedState::Installed);
    assert_eq!(observed.current_version.as_deref(), Some("0.162.0"));
    assert_eq!(observed.observation_hash, consent.observation_hash);
    assert!(report.stdout.contains("codex-cli 0.163.0"));
    // Persisted consent survives a reload of the private store.
    let reloaded = fixture.service();
    let again = reloaded.operation(&id).unwrap();
    assert_eq!(again.action, Some(ProviderCliAction::Replace));
    assert_eq!(
        again.observation_hash.as_deref(),
        Some(consent.observation_hash.as_str())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn failed_install_step_reports_exit_code_and_cleans_up() {
    let fixture = fixture(table_with_codex_integrity(&integrity_of(TARBALL)));
    fs::write(fixture.fixture.join("fail-install"), b"").unwrap();
    let service = fixture.service();
    let recipes = service.recipes().unwrap();
    let codex = recipes
        .recipes
        .iter()
        .find(|view| view.recipe_id == CODEX)
        .unwrap();
    let argv_hash = plan_argv_hash(codex.plan.as_ref().unwrap());
    let id = operation_id(6);
    let consent = fixture.consent(&service).await;
    service
        .install(
            &id,
            &fixture.codex_ref(),
            &recipes.table_hash,
            &argv_hash,
            &consent,
        )
        .unwrap();
    let report = wait_terminal(&service, &id).await;
    assert_eq!(report.status, ProviderCliOperationStatus::Failed);
    assert_eq!(
        report.error_code.as_deref(),
        Some("provider_cli_install_failed")
    );
    assert_eq!(report.exit_code, Some(7));
    assert_eq!(report.step, Some(3));
    assert!(report.stderr.contains("EACCES fixture"));
    // No plan step after the failure (the consent observation's own prefix
    // resolution is not a plan step and records no result).
    assert_eq!(
        report
            .steps
            .as_ref()
            .unwrap()
            .iter()
            .map(|step| step.step)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(!fixture.root.join("jobs").join(CODEX).exists());
}

/// A lifecycle-style descendant that leaves the process group but keeps the
/// step's output pipe is detected; the operation ends `unknown`, never
/// `succeeded`. Takes the descendant grace period (about ten seconds).
#[cfg(unix)]
#[tokio::test]
async fn detached_descendant_holding_output_turns_operation_unknown() {
    assert!(
        std::process::Command::new("perl")
            .arg("-MPOSIX")
            .arg("-e")
            .arg("1")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false),
        "this regression needs perl with POSIX to detach a descendant; do not skip it"
    );
    let fixture = fixture(table_with_codex_integrity(&integrity_of(TARBALL)));
    fs::write(fixture.fixture.join("detach-install"), b"").unwrap();
    let service = fixture.service();
    let recipes = service.recipes().unwrap();
    let codex = recipes
        .recipes
        .iter()
        .find(|view| view.recipe_id == CODEX)
        .unwrap();
    let argv_hash = plan_argv_hash(codex.plan.as_ref().unwrap());
    let id = operation_id(7);
    let consent = fixture.consent(&service).await;
    service
        .install(
            &id,
            &fixture.codex_ref(),
            &recipes.table_hash,
            &argv_hash,
            &consent,
        )
        .unwrap();
    let report = wait_terminal(&service, &id).await;
    assert_eq!(
        report.status,
        ProviderCliOperationStatus::Unknown,
        "{report:?}"
    );
    assert_eq!(report.error_code.as_deref(), Some("cleanup_unconfirmed"));
    assert_eq!(report.step, Some(3));
    assert_eq!(
        report.steps.as_ref().unwrap().len(),
        3,
        "an unconfirmed step stops the plan"
    );
}

/// A probe whose output exceeds the retained tail has no trustworthy first
/// line: a crafted tail must not turn a newer installation into an "upgrade"
/// or a "noop". Truncated output is unknown and needs acknowledgement.
#[cfg(unix)]
#[tokio::test]
async fn truncated_probe_output_never_yields_a_version() {
    let fixture = fixture(table_with_codex_integrity(&integrity_of(TARBALL)));
    // Real first line 9.0.0 (newer than the target); the retained 32 KiB tail
    // starts with a line claiming 0.1.0 (older) and then the target itself.
    write_executable(
        &fixture.prefix_binary(),
        "#!/bin/sh\nprintf 'codex-cli 9.0.0\\n'\nhead -c 40000 /dev/zero | tr '\\0' x\nprintf '\\ncodex-cli 0.1.0\\ncodex-cli 0.163.0\\n'\n",
    );
    let service = fixture.service();
    let response = service.check(&fixture.codex_ref()).await.unwrap();
    assert!(response.truncated, "{response:?}");
    assert_eq!(response.exit_code, Some(0));
    let observation = response.observation.unwrap();
    assert_eq!(observation.validate(), Ok(()));
    assert_eq!(observation.state, ProviderCliObservedState::Unknown);
    assert_eq!(observation.current_version, None);
    assert_eq!(
        observation.probe.error_code.as_deref(),
        Some("provider_cli_probe_output_truncated")
    );
    let decision = decide_provider_cli_action(&observation, PINNED).unwrap();
    assert_eq!(decision.action, ProviderCliAction::Replace);
    assert_eq!(
        decision.version_relation,
        Some(ProviderCliVersionRelation::Unknown),
        "never upgrade or noop from a truncated tail"
    );
}

/// Process facts come before consent: an observation whose probe left a
/// detached pipe holder is not evidence at prepare time, and at either gate
/// it stops the operation as `unknown` (`cleanup_unconfirmed`) with nothing
/// executed after it. Takes two descendant grace periods (about twenty
/// seconds).
#[cfg(unix)]
#[tokio::test]
async fn unconfirmed_probe_cleanup_issues_no_consent_and_stops_the_operation() {
    assert!(
        std::process::Command::new("perl")
            .arg("-MPOSIX")
            .arg("-e")
            .arg("1")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false),
        "this regression needs perl with POSIX to detach a descendant; do not skip it"
    );
    let fixture = fixture(table_with_codex_integrity(&integrity_of(TARBALL)));
    let service = fixture.service();
    let recipes = service.recipes().unwrap();
    let codex = recipes
        .recipes
        .iter()
        .find(|view| view.recipe_id == CODEX)
        .unwrap();
    let argv_hash = plan_argv_hash(codex.plan.as_ref().unwrap());
    // Consent given on an empty prefix.
    let consent = fixture.consent(&service).await;
    assert_eq!(consent.action, ProviderCliAction::Install);

    // Prepare-time: the probe detaches a holder of its stdout.
    fixture.install_binary(
        "codex-cli 0.162.0; perl -MPOSIX -e 'POSIX::setsid(); exec \"sleep\", \"20\"' &",
    );
    let response = service.check(&fixture.codex_ref()).await.unwrap();
    let observation = response.observation.unwrap();
    assert_eq!(observation.validate(), Ok(()));
    assert!(!observation.probe.cleanup_confirmed);
    assert_eq!(
        observation.probe.error_code.as_deref(),
        Some("cleanup_unconfirmed")
    );
    assert_eq!(observation.state, ProviderCliObservedState::Unknown);
    assert_eq!(
        observation.current_version, None,
        "its output is not evidence"
    );
    assert_eq!(
        decide_provider_cli_action(&observation, PINNED),
        None,
        "no consent can be issued on an unconfirmed probe"
    );
    assert!(!response.installed);

    // Second gate: the binary planted during pack detaches at probe time.
    fs::remove_file(fixture.prefix_binary()).unwrap();
    fs::write(fixture.fixture.join("plant-on-pack-detach"), b"").unwrap();
    let id = operation_id(12);
    service
        .install(
            &id,
            &fixture.codex_ref(),
            &recipes.table_hash,
            &argv_hash,
            &consent,
        )
        .unwrap();
    let report = wait_terminal(&service, &id).await;
    assert_eq!(
        report.status,
        ProviderCliOperationStatus::Unknown,
        "{report:?}"
    );
    assert_eq!(report.error_code.as_deref(), Some("cleanup_unconfirmed"));
    assert_eq!(report.step, Some(3));
    assert!(fixture.log("argv-pack").is_some());
    assert!(
        fixture.log("argv-install").is_none(),
        "nothing spawns after an unconfirmed observation"
    );
    assert_eq!(report.steps.as_ref().unwrap().len(), 2);
    let observed = report.observed_before.unwrap();
    assert_eq!(observed.state, ProviderCliObservedState::Unknown);
    assert_eq!(observed.current_version, None);
}

/// Observations inherit the operation's absolute deadline: when the budget is
/// spent during the second gate, the install step is never spawned and the
/// operation ends timed out, not changed or succeeded.
#[cfg(unix)]
#[tokio::test]
async fn consent_gate_inherits_the_operation_deadline() {
    let fixture = fixture(table_with_codex_integrity(&integrity_of(TARBALL)));
    fs::write(fixture.fixture.join("slow-pack"), b"").unwrap();
    fs::write(fixture.fixture.join("slow-prefix-after-pack"), b"").unwrap();
    let service = fixture.service();
    let recipes = service.recipes().unwrap();
    let codex = recipes
        .recipes
        .iter()
        .find(|view| view.recipe_id == CODEX)
        .unwrap();
    let argv_hash = plan_argv_hash(codex.plan.as_ref().unwrap());
    let consent = fixture.consent(&service).await;
    // Budget: the first gate and the two-second pack fit; the second gate's
    // three-second prefix resolution crosses the deadline.
    service.set_operation_budget(Duration::from_secs(4));
    let id = operation_id(13);
    service
        .install(
            &id,
            &fixture.codex_ref(),
            &recipes.table_hash,
            &argv_hash,
            &consent,
        )
        .unwrap();
    let report = wait_terminal(&service, &id).await;
    assert_eq!(
        report.status,
        ProviderCliOperationStatus::TimedOut,
        "{report:?}"
    );
    assert_eq!(report.error_code.as_deref(), Some("provider_cli_deadline"));
    assert!(report.timed_out);
    assert!(fixture.log("argv-pack").is_some());
    assert!(
        fixture.log("argv-install").is_none(),
        "an exhausted budget spawns nothing"
    );
    assert_eq!(report.steps.as_ref().unwrap().len(), 2);
    assert!(!fixture.root.join("jobs").join(CODEX).exists());

    // run_step itself refuses to spawn once the deadline has passed.
    let step = ProviderCliPlanStep::Command {
        step: 9,
        purpose: "prefix".into(),
        executable: fixture.fixture.join("bin/npm").display().to_string(),
        args: vec!["expired-probe".into()],
    };
    let environment = step_environment(
        BTreeMap::new(),
        fixture.root.as_path(),
        Some(fixture.search_path.as_os_str()),
    );
    let outcome = run_step(
        &step,
        &environment,
        fixture.fixture.as_path(),
        Instant::now() - Duration::from_millis(1),
    )
    .await;
    assert!(outcome.timed_out && outcome.cleanup_confirmed && !outcome.spawn_failed);
    assert!(
        fixture.log("argv-expired-probe").is_none(),
        "no process was started"
    );
}

#[test]
fn restart_marks_running_operations_unknown_and_prunes_expired_results() {
    let dir = TempDir::new();
    let root = dir.path().join("provider-cli");
    fs::create_dir_all(&root).unwrap();
    let recipe = ProviderCliRecipeRef {
        recipe_id: CODEX.into(),
        recipe_version: 1,
        recipe_hash: "a".repeat(64),
    };
    let make = |id: u32, status: ProviderCliOperationStatus, created_at_ms: i64| StoredOperation {
        operation_id: operation_id(id),
        recipe: recipe.clone(),
        table_hash: "t".repeat(64),
        argv_hash: "h".repeat(64),
        status,
        step: Some(3),
        created_at_ms,
        finished_at_ms: status.is_terminal().then_some(created_at_ms),
        elapsed_ms: 5,
        exit_code: None,
        timed_out: false,
        error_code: None,
        steps: Vec::new(),
        plan: Vec::new(),
        consent: None,
        observed_before: None,
    };
    let now = now_ms();
    let file = StoreFile {
        version: STORE_VERSION,
        operations: vec![
            make(1, ProviderCliOperationStatus::Running, now),
            make(
                2,
                ProviderCliOperationStatus::Succeeded,
                now - 2 * PROVIDER_CLI_RETENTION_SECONDS * 1000,
            ),
            make(3, ProviderCliOperationStatus::Failed, now),
        ],
    };
    fs::write(
        root.join(STORE_FILE_NAME),
        serde_json::to_vec(&file).unwrap(),
    )
    .unwrap();
    let store = Store::load(root.join(STORE_FILE_NAME)).unwrap();
    let running = store.get(&operation_id(1)).unwrap();
    assert_eq!(running.status, ProviderCliOperationStatus::Unknown);
    assert_eq!(running.error_code.as_deref(), Some("controller_restarted"));
    assert!(running.finished_at_ms.is_some());
    assert!(
        store.get(&operation_id(2)).is_none(),
        "expired result pruned"
    );
    assert_eq!(
        store.get(&operation_id(3)).unwrap().status,
        ProviderCliOperationStatus::Failed
    );
    assert!(store.active().is_none());
    // Non-terminal reports expose no output; terminal ones do.
    let report = running.report();
    assert!(report.steps.is_some() && report.stdout.is_empty());
    // An unreadable store is kept aside, never silently discarded.
    fs::write(root.join(STORE_FILE_NAME), b"not json").unwrap();
    let service = ProviderCliService::new(root.clone(), "srv_fixture");
    assert!(service.operation(&operation_id(3)).is_err());
    assert!(fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().contains("unreadable")));
}

#[test]
fn helpers_validate_ids_prefixes_and_version_text() {
    assert!(operation_id_ok(&operation_id(9)));
    assert!(!operation_id_ok("pco_short"));
    assert!(!operation_id_ok(&format!("op_{}", "0".repeat(32))));
    assert_eq!(
        version_text("\n  codex-cli 1.2.3\x1b[0m\nextra\n"),
        "codex-cli 1.2.3[0m"
    );
    assert_eq!(scope_of("@openai/codex"), Some("openai"));
    assert_eq!(scope_of("opencode-ai"), None);
    assert_eq!(
        tarball_name("@google/gemini-cli", "0.64.0"),
        "google-gemini-cli-0.64.0.tgz"
    );
    assert_eq!(
        tarball_name("opencode-ai", "1.19.3"),
        "opencode-ai-1.19.3.tgz"
    );
    let dir = TempDir::new();
    let pack = dir.path().join("pack");
    fs::create_dir(&pack).unwrap();
    fs::write(pack.join("a.tgz"), TARBALL).unwrap();
    assert!(verify_pack(&pack, &integrity_of(TARBALL)));
    assert!(!verify_pack(&pack, &integrity_of(b"other")));
    fs::write(pack.join("b.tgz"), TARBALL).unwrap();
    assert!(
        !verify_pack(&pack, &integrity_of(TARBALL)),
        "exactly one tarball"
    );
}
