//! Provider CLI installation on the machine (Issue #123 part 2).
//!
//! One expansion function turns a validated, embedded recipe into the plan
//! that is both displayed to the human and executed here; the Proxy binds the
//! hash of that plan into the confirmation token and this module refuses to
//! run a plan whose fresh expansion differs. Execution is a bounded, private,
//! shell-free sequence: `npm pack` into a Controller-owned directory, SHA-512
//! verification of the single tarball against the recipe's `integrity`, a
//! global install of that verified local tarball, global-prefix resolution,
//! and the recipe's version probe resolved only from that prefix's `bin`.
//!
//! Ownership is honest: every step starts in its own Unix process group and
//! is killed as a group at the deadline; descendants that leave the group but
//! still hold the step's output pipes are detected and turn the operation
//! `unknown` (`cleanup_unconfirmed`); descendants that both detach and close
//! those pipes are outside what this module can prove. Windows entries are all
//! unsupported in this iteration and nothing is executed there.
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use tokio::io::AsyncReadExt;
use tracing::warn;
use treer_protocol::{
    plan_argv_hash, ProtocolError, ProviderCliCheckResponse, ProviderCliInstallAccepted,
    ProviderCliLifecycleScripts, ProviderCliOperationReport, ProviderCliOperationStatus,
    ProviderCliPackage, ProviderCliPlanStep, ProviderCliRecipeRef, ProviderCliRecipeView,
    ProviderCliRecipesResponse, ProviderCliStepResult, ValidatedProviderRecipe,
    ValidatedProviderTable, PROVIDER_CLI_CHECK_TIMEOUT_SECONDS,
    PROVIDER_CLI_OPERATION_DEADLINE_SECONDS, PROVIDER_CLI_OUTPUT_TAIL_BYTES,
    PROVIDER_CLI_RETAINED_OPERATIONS, PROVIDER_CLI_RETENTION_SECONDS,
};
use uuid::Uuid;

/// Directory below the Treer state dir that owns job directories and results.
pub const ROOT_DIRECTORY_NAME: &str = "provider-cli";
const STORE_FILE_NAME: &str = "operations.json";
const STORE_VERSION: u32 = 1;
const JOBS_DIRECTORY: &str = "jobs";
const CHECKS_DIRECTORY: &str = "checks";
const PACK_DIRECTORY: &str = "pack";
const USER_CONFIG: &str = "npmrc";
const GLOBAL_CONFIG: &str = "npmrc-global";
/// How long readers may lag after the step's own process exited before the
/// surviving pipe holder is treated as an unconfirmed descendant.
const DESCENDANT_GRACE: Duration = Duration::from_secs(10);
const MAX_VERSION_TEXT: usize = 200;

pub fn release_platform() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "x86_64") => "darwin-x86_64",
        ("macos", "aarch64") => "darwin-aarch64",
        ("windows", "x86_64") => "windows-x86_64",
        _ => return None,
    })
}

/// Variables npm, Node or a lifecycle script could use to redirect the
/// installation; removed even though the machine exec allowlist excludes them.
fn npm_isolation_forbids(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.starts_with("NPM_CONFIG_")
        || upper.starts_with("NPM_")
        || upper.ends_with("_PROXY")
        || matches!(
            upper.as_str(),
            "NODE_OPTIONS"
                | "NODE_PATH"
                | "NODE_EXTRA_CA_CERTS"
                | "NODE_TLS_REJECT_UNAUTHORIZED"
                | "SSL_CERT_FILE"
                | "SSL_CERT_DIR"
                | "COREPACK_HOME"
                | "NVM_DIR"
        )
}

/// Minimal cleared environment for every step: the machine exec allowlist
/// minus npm/Node/proxy controls, HOME and temp inside the job directory.
pub(crate) fn step_environment(
    base: BTreeMap<OsString, OsString>,
    job: &Path,
    search_path: Option<&OsStr>,
) -> BTreeMap<OsString, OsString> {
    let mut environment: BTreeMap<OsString, OsString> = base
        .into_iter()
        .filter(|(name, _)| !npm_isolation_forbids(&name.to_string_lossy()))
        .collect();
    if let Some(path) = search_path {
        environment.insert("PATH".into(), path.to_owned());
    }
    environment.insert("HOME".into(), job.join("home").into_os_string());
    for name in ["TMPDIR", "TEMP", "TMP"] {
        environment.insert(name.into(), job.join("tmp").into_os_string());
    }
    environment.insert("LC_ALL".into(), "C.UTF-8".into());
    environment
}

fn scope_of(package: &str) -> Option<&str> {
    package
        .strip_prefix('@')
        .and_then(|rest| rest.split_once('/'))
        .map(|(scope, _)| scope)
}

fn config_flags(job: &Path) -> Vec<String> {
    vec![
        format!("--userconfig={}", job.join(USER_CONFIG).display()),
        format!("--globalconfig={}", job.join(GLOBAL_CONFIG).display()),
        format!("--cache={}", job.join("cache").display()),
    ]
}

fn registry_flags(package: &str, registry: &str) -> Vec<String> {
    let mut flags = vec![format!("--registry={registry}")];
    if let Some(scope) = scope_of(package) {
        flags.push(format!("--@{scope}:registry={registry}"));
    }
    flags
}

const QUIET_FLAGS: [&str; 3] = ["--no-audit", "--no-fund", "--loglevel=error"];

fn tarball_name(package: &str, version: &str) -> String {
    // npm names scoped tarballs `<scope>-<name>-<version>.tgz`.
    format!(
        "{}-{version}.tgz",
        package.trim_start_matches('@').replace('/', "-")
    )
}

/// The single expansion of a supported recipe for one job directory. Display
/// and execution call this same function; tests compare them item for item.
pub(crate) fn expand_plan(
    recipe: &ValidatedProviderRecipe,
    npm: &str,
    job: &Path,
) -> Result<Vec<ProviderCliPlanStep>, ProtocolError> {
    let install = recipe.recipe.install.as_ref().ok_or_else(|| {
        ProtocolError::new(
            "provider_cli_recipe_unsupported",
            "recipe has no installation data",
        )
    })?;
    let check = recipe.recipe.check.as_ref().ok_or_else(|| {
        ProtocolError::new("provider_cli_recipe_unsupported", "recipe has no check")
    })?;
    let spec = format!("{}@{}", install.package, install.version);
    let mut pack = vec!["pack".to_owned(), spec];
    pack.extend(registry_flags(&install.package, &install.registry));
    pack.extend(config_flags(job));
    pack.push("--ignore-scripts".to_owned());
    pack.extend(QUIET_FLAGS.iter().map(|flag| (*flag).to_owned()));
    pack.push(format!(
        "--pack-destination={}",
        job.join(PACK_DIRECTORY).display()
    ));
    let mut install_args = vec![
        "install".to_owned(),
        "--global".to_owned(),
        job.join(PACK_DIRECTORY)
            .join(tarball_name(&install.package, &install.version))
            .display()
            .to_string(),
    ];
    install_args.extend(registry_flags(&install.package, &install.registry));
    install_args.extend(config_flags(job));
    if install.ignore_scripts {
        install_args.push("--ignore-scripts".to_owned());
    }
    install_args.extend(QUIET_FLAGS.iter().map(|flag| (*flag).to_owned()));
    let mut prefix = vec!["prefix".to_owned(), "--global".to_owned()];
    prefix.extend(config_flags(job).into_iter().take(2));
    Ok(vec![
        ProviderCliPlanStep::Command {
            step: 1,
            purpose: "pack".into(),
            executable: npm.to_owned(),
            args: pack,
        },
        ProviderCliPlanStep::Verify {
            step: 2,
            purpose: "integrity".into(),
            algorithm: "sha512".into(),
            expected: install.integrity.clone(),
        },
        ProviderCliPlanStep::Command {
            step: 3,
            purpose: "install".into(),
            executable: npm.to_owned(),
            args: install_args,
        },
        ProviderCliPlanStep::Command {
            step: 4,
            purpose: "prefix".into(),
            executable: npm.to_owned(),
            args: prefix,
        },
        ProviderCliPlanStep::Resolve {
            step: 5,
            purpose: "check".into(),
            rule: format!(
                "<npm prefix --global>/bin/{} {}",
                check.executable,
                check.args.join(" ")
            ),
            binary: check.executable.clone(),
            args: check.args.clone(),
            prefix_from_step: 4,
        },
    ])
}

fn check_step(prefix: &Path, executable: &str, args: &[String]) -> ProviderCliPlanStep {
    ProviderCliPlanStep::Command {
        step: 5,
        purpose: "check".into(),
        executable: prefix.join("bin").join(executable).display().to_string(),
        args: args.to_vec(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredOperation {
    operation_id: String,
    recipe: ProviderCliRecipeRef,
    table_hash: String,
    argv_hash: String,
    status: ProviderCliOperationStatus,
    #[serde(default)]
    step: Option<u32>,
    created_at_ms: i64,
    #[serde(default)]
    finished_at_ms: Option<i64>,
    elapsed_ms: u64,
    #[serde(default)]
    exit_code: Option<i32>,
    timed_out: bool,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    steps: Vec<ProviderCliStepResult>,
    plan: Vec<ProviderCliPlanStep>,
}

impl StoredOperation {
    fn expired(&self, now_ms: i64) -> bool {
        let reference = self.finished_at_ms.unwrap_or(self.created_at_ms);
        now_ms - reference > PROVIDER_CLI_RETENTION_SECONDS * 1000
    }

    fn report(&self) -> ProviderCliOperationReport {
        let terminal = self.status.is_terminal();
        let last = self.steps.last();
        ProviderCliOperationReport {
            operation_id: self.operation_id.clone(),
            recipe: self.recipe.clone(),
            status: self.status,
            step: self.step,
            elapsed_ms: self.elapsed_ms,
            exit_code: self.exit_code,
            timed_out: self.timed_out,
            stdout: last
                .filter(|_| terminal)
                .map(|step| step.stdout.clone())
                .unwrap_or_default(),
            stderr: last
                .filter(|_| terminal)
                .map(|step| step.stderr.clone())
                .unwrap_or_default(),
            truncated: terminal && last.is_some_and(|step| step.truncated),
            error_code: self.error_code.clone(),
            steps: terminal.then(|| self.steps.clone()),
            plan: terminal.then(|| self.plan.clone()),
            argv_hash: self.argv_hash.clone(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    operations: Vec<StoredOperation>,
}

struct Store {
    path: PathBuf,
    operations: Vec<StoredOperation>,
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

impl Store {
    /// Loads retained results; any operation that was still running when the
    /// previous Controller stopped is `unknown`, never assumed finished.
    fn load(path: PathBuf) -> io::Result<Self> {
        let operations = match fs::read(&path) {
            Ok(bytes) => {
                let file: StoreFile = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                if file.version != STORE_VERSION {
                    return Err(io::Error::other("unsupported provider CLI store version"));
                }
                file.operations
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        let now = now_ms();
        let mut store = Self { path, operations };
        for operation in &mut store.operations {
            if !operation.status.is_terminal() {
                operation.status = ProviderCliOperationStatus::Unknown;
                operation.error_code = Some("controller_restarted".into());
                operation.finished_at_ms = Some(now);
            }
        }
        store.prune(now);
        store.persist()?;
        Ok(store)
    }

    fn prune(&mut self, now: i64) {
        self.operations.retain(|operation| !operation.expired(now));
    }

    fn retained_terminal(&self) -> usize {
        self.operations
            .iter()
            .filter(|operation| operation.status.is_terminal())
            .count()
    }

    fn active(&self) -> Option<&StoredOperation> {
        self.operations
            .iter()
            .find(|operation| !operation.status.is_terminal())
    }

    fn get(&self, operation_id: &str) -> Option<&StoredOperation> {
        self.operations
            .iter()
            .find(|operation| operation.operation_id == operation_id)
    }

    fn update(
        &mut self,
        operation_id: &str,
        edit: impl FnOnce(&mut StoredOperation),
    ) -> io::Result<()> {
        if let Some(operation) = self
            .operations
            .iter_mut()
            .find(|operation| operation.operation_id == operation_id)
        {
            edit(operation);
        }
        self.persist()
    }

    fn persist(&self) -> io::Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        treer_platform::secure_directory(parent)?;
        let temporary = parent.join(format!(
            ".{STORE_FILE_NAME}.{}.tmp",
            Uuid::new_v4().simple()
        ));
        let bytes = serde_json::to_vec(&StoreFile {
            version: STORE_VERSION,
            operations: self.operations.clone(),
        })
        .map_err(io::Error::other)?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = options.open(&temporary)?;
            treer_platform::secure_file(&temporary)?;
            io::Write::write_all(&mut file, &bytes)?;
            file.sync_all()?;
            drop(file);
            treer_platform::replace_file(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

struct Inner {
    server_id: String,
    root: PathBuf,
    platform: Option<&'static str>,
    table: Result<ValidatedProviderTable, &'static str>,
    /// Fixed PATH for npm resolution and child processes; `None` uses the
    /// machine exec allowlist's PATH. Tests point it at a fixture npm.
    search_path: Option<OsString>,
    store: Mutex<Store>,
}

/// Controller-owned provider CLI service; cheap to clone, one per Controller.
#[derive(Clone)]
pub struct ProviderCliService {
    inner: Arc<Inner>,
}

fn store_error(error: io::Error) -> ProtocolError {
    warn!(%error, "provider CLI operation store unavailable");
    ProtocolError::new(
        "provider_cli_storage_unavailable",
        "installation results cannot be recorded on this machine",
    )
}

impl ProviderCliService {
    pub fn new(root: PathBuf, server_id: &str) -> Self {
        let table = ValidatedProviderTable::embedded().map_err(|error| {
            warn!(code = %error.code, message = %error.message, "embedded provider recipe table rejected; provider CLI installation disabled");
            "provider_cli_table_invalid"
        });
        Self::build(root, server_id, None, table)
    }

    /// Test constructor: a fixture PATH (fixture npm) and a table whose
    /// integrity matches fixture tarballs. Production always embeds.
    #[cfg(test)]
    pub(crate) fn for_test(
        root: PathBuf,
        server_id: &str,
        search_path: OsString,
        table: ValidatedProviderTable,
    ) -> Self {
        Self::build(root, server_id, Some(search_path), Ok(table))
    }

    fn build(
        root: PathBuf,
        server_id: &str,
        search_path: Option<OsString>,
        table: Result<ValidatedProviderTable, &'static str>,
    ) -> Self {
        let store_path = root.join(STORE_FILE_NAME);
        let store = match Store::load(store_path.clone()) {
            Ok(store) => store,
            Err(error) => {
                // Never silently discard results: keep the unreadable file.
                warn!(%error, "provider CLI operation store unreadable; starting empty");
                let aside = root.join(format!("{STORE_FILE_NAME}.unreadable-{}", now_ms()));
                let _ = fs::rename(&store_path, aside);
                Store {
                    path: store_path,
                    operations: Vec::new(),
                }
            }
        };
        Self {
            inner: Arc::new(Inner {
                server_id: server_id.to_owned(),
                root,
                platform: release_platform(),
                table,
                search_path,
                store: Mutex::new(store),
            }),
        }
    }

    /// Advertised only when the embedded table validated on a known platform.
    pub fn capability_available(&self) -> bool {
        self.inner.table.is_ok() && self.inner.platform.is_some()
    }

    fn table(&self) -> Result<(&ValidatedProviderTable, &'static str), ProtocolError> {
        let platform = self.inner.platform.ok_or_else(|| {
            ProtocolError::new(
                "provider_cli_platform_unsupported",
                "this machine's platform has no provider recipes",
            )
        })?;
        let table = self.inner.table.as_ref().map_err(|code| {
            ProtocolError::new(*code, "embedded provider recipe table is invalid")
        })?;
        Ok((table, platform))
    }

    fn search_path(&self) -> Option<OsString> {
        self.inner.search_path.clone().or_else(|| {
            treer_platform::machine_exec_environment()
                .get(OsStr::new("PATH"))
                .cloned()
        })
    }

    /// Absolute npm from absolute PATH directories only (the Agent command
    /// resolver); the same path is displayed and executed.
    fn resolve_npm(&self) -> Result<String, ProtocolError> {
        let mut environment = BTreeMap::new();
        if let Some(path) = self.search_path() {
            environment.insert("PATH".to_owned(), path.to_string_lossy().into_owned());
        }
        crate::controller::resolve_machine_command("npm", &environment).map_err(|_| {
            ProtocolError::new(
                "provider_cli_npm_unavailable",
                "npm was not found on this machine's PATH",
            )
        })
    }

    fn job_directory(&self, kind: &str, recipe_id: &str) -> PathBuf {
        self.inner.root.join(kind).join(recipe_id)
    }

    fn view(
        &self,
        entry: &ValidatedProviderRecipe,
        platform: &str,
        npm: Option<&str>,
    ) -> ProviderCliRecipeView {
        let recipe = &entry.recipe;
        let mut view = ProviderCliRecipeView {
            provider_id: recipe.provider_id.clone(),
            recipe_id: recipe.recipe_id.clone(),
            recipe_version: recipe.recipe_version,
            recipe_hash: entry.recipe_hash.clone(),
            platform: platform.to_owned(),
            binary_names: recipe.binary_names.clone(),
            supported: false,
            official_docs: recipe.official_docs.clone(),
            login_hint: recipe.login_hint.clone(),
            unsupported_reason: recipe.unsupported_reason.clone(),
            node_min: recipe.node_min,
            lifecycle_scripts: None,
            package: None,
            plan: None,
        };
        if !recipe.supported {
            return view;
        }
        let Some(npm) = npm else {
            view.unsupported_reason = Some("npm_unavailable".into());
            return view;
        };
        let job = self.job_directory(JOBS_DIRECTORY, &recipe.recipe_id);
        match expand_plan(entry, npm, &job) {
            Ok(plan) => {
                let install = recipe.install.as_ref();
                view.supported = true;
                view.lifecycle_scripts = Some(ProviderCliLifecycleScripts {
                    enabled: install.is_some_and(|install| !install.ignore_scripts),
                    review: recipe.lifecycle_review.clone(),
                });
                view.package = install.map(|install| ProviderCliPackage {
                    name: install.package.clone(),
                    version: install.version.clone(),
                    integrity: install.integrity.clone(),
                    registry: install.registry.clone(),
                });
                view.plan = Some(plan);
            }
            Err(error) => view.unsupported_reason = Some(error.code),
        }
        view
    }

    pub fn recipes(&self) -> Result<ProviderCliRecipesResponse, ProtocolError> {
        let (table, platform) = self.table()?;
        let npm = self.resolve_npm().ok();
        Ok(ProviderCliRecipesResponse {
            schema_version: treer_protocol::PROVIDER_RECIPES_SCHEMA_VERSION,
            machine_id: self.inner.server_id.clone(),
            platform: platform.to_owned(),
            controller_source_sha: crate::controller::build_commit(),
            table_hash: table.table_hash.clone(),
            recipes: table
                .for_platform(platform)
                .map(|entry| self.view(entry, platform, npm.as_deref()))
                .collect(),
        })
    }

    fn supported_entry<'a>(
        table: &'a ValidatedProviderTable,
        platform: &str,
        reference: &ProviderCliRecipeRef,
    ) -> Result<&'a ValidatedProviderRecipe, ProtocolError> {
        let entry = table.find(platform, reference).ok_or_else(|| {
            ProtocolError::new(
                "provider_cli_recipe_mismatch",
                "recipe is unknown or changed on this machine",
            )
        })?;
        if !entry.recipe.supported {
            return Err(ProtocolError::new(
                "provider_cli_recipe_unsupported",
                "recipe is not supported on this machine",
            ));
        }
        Ok(entry)
    }

    /// Runs only the vetted version probe from the installed global prefix.
    pub async fn check(
        &self,
        reference: &ProviderCliRecipeRef,
    ) -> Result<ProviderCliCheckResponse, ProtocolError> {
        let (table, platform) = self.table()?;
        let entry = Self::supported_entry(table, platform, reference)?;
        let npm = self.resolve_npm()?;
        let job = self.job_directory(CHECKS_DIRECTORY, &entry.recipe.recipe_id);
        let plan = expand_plan(entry, &npm, &job)?;
        let job_dir = JobDirectory::create(&job).map_err(|error| {
            warn!(%error, "provider CLI check directory unavailable");
            ProtocolError::new(
                "provider_cli_storage_unavailable",
                "private check directory could not be created",
            )
        })?;
        let environment = step_environment(
            treer_platform::machine_exec_environment(),
            &job,
            self.search_path().as_deref(),
        );
        let deadline = Instant::now() + Duration::from_secs(PROVIDER_CLI_CHECK_TIMEOUT_SECONDS * 2);
        let check = entry
            .recipe
            .check
            .as_ref()
            .expect("supported entry has check");
        let outcome = async {
            let prefix_step = plan
                .iter()
                .find(|step| matches!(step, ProviderCliPlanStep::Command { purpose, .. } if purpose == "prefix"))
                .cloned()
                .expect("plan has prefix step");
            let prefix = run_step(&prefix_step, &environment, &job, deadline).await;
            let Some(prefix_path) = parse_prefix(&prefix) else {
                return ProviderCliCheckResponse {
                    installed: false,
                    found_in_prefix: false,
                    version: None,
                    exit_code: prefix.exit_code,
                    timed_out: prefix.timed_out,
                    stdout: String::new(),
                    stderr: prefix.stderr,
                    truncated: prefix.truncated,
                    error_code: Some("provider_cli_prefix_unresolved".into()),
                    executable: None,
                };
            };
            let Some(step) = resolve_check(&prefix_path, &check.executable, &check.args) else {
                return ProviderCliCheckResponse {
                    installed: false,
                    found_in_prefix: false,
                    version: None,
                    exit_code: None,
                    timed_out: false,
                    stdout: String::new(),
                    stderr: String::new(),
                    truncated: false,
                    error_code: Some("provider_cli_binary_not_found".into()),
                    executable: None,
                };
            };
            let probe = run_step(
                &step,
                &environment,
                &job,
                Instant::now() + Duration::from_secs(PROVIDER_CLI_CHECK_TIMEOUT_SECONDS),
            )
            .await;
            let installed = probe.exit_code == Some(0) && probe.cleanup_confirmed;
            let executable = match &step {
                ProviderCliPlanStep::Command { executable, .. } => Some(executable.clone()),
                _ => None,
            };
            ProviderCliCheckResponse {
                installed,
                found_in_prefix: true,
                version: installed.then(|| version_text(&probe.stdout)),
                exit_code: probe.exit_code,
                timed_out: probe.timed_out,
                stdout: probe.stdout,
                stderr: probe.stderr,
                truncated: probe.truncated,
                error_code: (!probe.cleanup_confirmed).then(|| "cleanup_unconfirmed".to_owned()),
                executable,
            }
        }
        .await;
        drop(job_dir);
        Ok(outcome)
    }

    /// Records and starts an installation. Returns once the record is durable
    /// and the plan is fixed; the Proxy polls `operation` for the result.
    pub fn install(
        &self,
        operation_id: &str,
        reference: &ProviderCliRecipeRef,
        table_hash: &str,
        argv_hash: &str,
    ) -> Result<ProviderCliInstallAccepted, ProtocolError> {
        if !operation_id_ok(operation_id) {
            return Err(ProtocolError::new(
                "provider_cli_recipe_invalid",
                "operation ID is invalid",
            ));
        }
        let (table, platform) = self.table()?;
        if table.table_hash != table_hash {
            return Err(ProtocolError::new(
                "provider_cli_table_mismatch",
                "the Controller's recipe table changed since confirmation",
            ));
        }
        let entry = Self::supported_entry(table, platform, reference)?;
        if platform.starts_with("windows-") {
            return Err(ProtocolError::new(
                "provider_cli_platform_unsupported",
                "Windows provider installation is not available",
            ));
        }
        let npm = self.resolve_npm()?;
        let job = self.job_directory(JOBS_DIRECTORY, &entry.recipe.recipe_id);
        let plan = expand_plan(entry, &npm, &job)?;
        let fresh_hash = plan_argv_hash(&plan);
        if fresh_hash != argv_hash {
            return Err(ProtocolError::new(
                "provider_cli_plan_changed",
                "the installation plan changed since it was displayed; review it again",
            ));
        }
        {
            let mut store = self
                .inner
                .store
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if let Some(existing) = store.get(operation_id) {
                // Redelivery of the same accepted command is idempotent.
                return Ok(ProviderCliInstallAccepted {
                    operation_id: existing.operation_id.clone(),
                    status: existing.status,
                    argv_hash: existing.argv_hash.clone(),
                });
            }
            if store.active().is_some() {
                return Err(ProtocolError::new(
                    "provider_cli_machine_busy",
                    "another provider CLI installation is active on this machine",
                ));
            }
            let now = now_ms();
            store.prune(now);
            if store.retained_terminal() >= PROVIDER_CLI_RETAINED_OPERATIONS {
                return Err(ProtocolError::new(
                    "provider_cli_storage_exhausted",
                    "retained installation results are at their cap; wait for them to expire",
                ));
            }
            store.operations.push(StoredOperation {
                operation_id: operation_id.to_owned(),
                recipe: reference.clone(),
                table_hash: table_hash.to_owned(),
                argv_hash: fresh_hash.clone(),
                status: ProviderCliOperationStatus::Running,
                step: None,
                created_at_ms: now,
                finished_at_ms: None,
                elapsed_ms: 0,
                exit_code: None,
                timed_out: false,
                error_code: None,
                steps: Vec::new(),
                plan: plan.clone(),
            });
            if let Err(error) = store.persist() {
                store.operations.pop();
                return Err(store_error(error));
            }
        }
        let service = self.clone();
        let spawned_id = operation_id.to_owned();
        let check = entry
            .recipe
            .check
            .clone()
            .expect("supported entry has check");
        tokio::spawn(async move {
            service.run_operation(&spawned_id, plan, job, check).await;
        });
        Ok(ProviderCliInstallAccepted {
            operation_id: operation_id.to_owned(),
            status: ProviderCliOperationStatus::Running,
            argv_hash: fresh_hash,
        })
    }

    pub fn operation(
        &self,
        operation_id: &str,
    ) -> Result<ProviderCliOperationReport, ProtocolError> {
        let mut store = self
            .inner
            .store
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        store.prune(now_ms());
        store
            .get(operation_id)
            .map(StoredOperation::report)
            .ok_or_else(|| {
                ProtocolError::new(
                    "provider_cli_operation_not_found",
                    "installation operation is not retained on this machine",
                )
            })
    }

    fn update(&self, operation_id: &str, edit: impl FnOnce(&mut StoredOperation)) {
        let mut store = self
            .inner
            .store
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Err(error) = store.update(operation_id, edit) {
            warn!(%error, operation_id, "failed to persist provider CLI operation state");
        }
    }

    async fn run_operation(
        &self,
        operation_id: &str,
        plan: Vec<ProviderCliPlanStep>,
        job: PathBuf,
        check: treer_protocol::ProviderCheck,
    ) {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(PROVIDER_CLI_OPERATION_DEADLINE_SECONDS);
        let job_dir = match JobDirectory::create(&job) {
            Ok(dir) => dir,
            Err(error) => {
                warn!(%error, operation_id, "provider CLI job directory unavailable");
                self.finish(
                    operation_id,
                    started,
                    ProviderCliOperationStatus::Failed,
                    Some("provider_cli_storage_unavailable"),
                    None,
                    false,
                );
                return;
            }
        };
        let environment = step_environment(
            treer_platform::machine_exec_environment(),
            &job,
            self.search_path().as_deref(),
        );
        let mut prefix: Option<PathBuf> = None;
        let mut final_status = ProviderCliOperationStatus::Succeeded;
        let mut error_code: Option<&'static str> = None;
        let mut last_exit: Option<i32> = None;
        let mut timed_out = false;
        for step in &plan {
            let number = step.step();
            self.update(operation_id, |record| record.step = Some(number));
            match step {
                ProviderCliPlanStep::Verify { expected, .. } => {
                    if !verify_pack(&job.join(PACK_DIRECTORY), expected) {
                        final_status = ProviderCliOperationStatus::Failed;
                        error_code = Some("provider_cli_integrity_mismatch");
                        break;
                    }
                    self.update(operation_id, |record| {
                        record.steps.push(ProviderCliStepResult {
                            step: number,
                            exit_code: Some(0),
                            stdout: String::new(),
                            stderr: String::new(),
                            truncated: false,
                            duration_ms: 0,
                        });
                    });
                }
                ProviderCliPlanStep::Command { purpose, .. } => {
                    let outcome = run_step(step, &environment, &job, deadline).await;
                    let (status, code) = classify(&outcome, purpose);
                    last_exit = outcome.exit_code;
                    timed_out |= outcome.timed_out;
                    let result = outcome.result(number);
                    if purpose == "prefix" {
                        prefix = parse_prefix(&outcome);
                    }
                    self.update(operation_id, |record| record.steps.push(result));
                    if status != ProviderCliOperationStatus::Succeeded {
                        final_status = status;
                        error_code = code;
                        break;
                    }
                    if purpose == "prefix" && prefix.is_none() {
                        final_status = ProviderCliOperationStatus::Failed;
                        error_code = Some("provider_cli_prefix_unresolved");
                        break;
                    }
                }
                ProviderCliPlanStep::Resolve { .. } => {
                    let Some(prefix_path) = prefix.as_deref() else {
                        final_status = ProviderCliOperationStatus::Failed;
                        error_code = Some("provider_cli_prefix_unresolved");
                        break;
                    };
                    let Some(check_command) =
                        resolve_check(prefix_path, &check.executable, &check.args)
                    else {
                        final_status = ProviderCliOperationStatus::Failed;
                        error_code = Some("provider_cli_binary_not_found");
                        break;
                    };
                    let probe_deadline = deadline.min(
                        Instant::now() + Duration::from_secs(PROVIDER_CLI_CHECK_TIMEOUT_SECONDS),
                    );
                    let outcome =
                        run_step(&check_command, &environment, &job, probe_deadline).await;
                    let (status, code) = classify(&outcome, "check");
                    last_exit = outcome.exit_code;
                    timed_out |= outcome.timed_out;
                    let result = outcome.result(number);
                    self.update(operation_id, |record| {
                        record.steps.push(result);
                        // The literal check path replaces the resolution rule.
                        if let Some(slot) =
                            record.plan.iter_mut().find(|item| item.step() == number)
                        {
                            *slot = check_command.clone();
                        }
                    });
                    if status != ProviderCliOperationStatus::Succeeded {
                        final_status = status;
                        error_code = code;
                        break;
                    }
                }
            }
        }
        drop(job_dir);
        self.finish(
            operation_id,
            started,
            final_status,
            error_code,
            last_exit,
            timed_out,
        );
    }

    fn finish(
        &self,
        operation_id: &str,
        started: Instant,
        status: ProviderCliOperationStatus,
        error_code: Option<&str>,
        exit_code: Option<i32>,
        timed_out: bool,
    ) {
        let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let error_code = error_code.map(str::to_owned);
        self.update(operation_id, |record| {
            record.status = status;
            record.error_code = error_code;
            record.exit_code = exit_code;
            record.timed_out = timed_out;
            record.elapsed_ms = elapsed;
            record.finished_at_ms = Some(now_ms());
        });
    }
}

fn operation_id_ok(value: &str) -> bool {
    value
        .strip_prefix("pco_")
        .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn version_text(stdout: &str) -> String {
    let line = stdout
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let cleaned: String = line
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_VERSION_TEXT)
        .collect();
    cleaned.trim().to_owned()
}

/// Exactly one absolute existing directory on stdout.
fn parse_prefix(outcome: &StepOutcome) -> Option<PathBuf> {
    if outcome.exit_code != Some(0) || !outcome.cleanup_confirmed {
        return None;
    }
    let mut lines = outcome
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty());
    let line = lines.next()?.trim();
    if lines.next().is_some() {
        return None;
    }
    let path = PathBuf::from(line);
    (path.is_absolute() && path.is_dir()).then_some(path)
}

/// The probe binary must live in the installed prefix's `bin`; a symlink is
/// allowed only when its target stays inside that prefix. PATH is never used.
fn resolve_check(prefix: &Path, executable: &str, args: &[String]) -> Option<ProviderCliPlanStep> {
    let candidate = prefix.join("bin").join(executable);
    let target = fs::canonicalize(&candidate).ok()?;
    let prefix_real = fs::canonicalize(prefix).ok()?;
    if !target.starts_with(&prefix_real) || !target.is_file() {
        return None;
    }
    Some(check_step(prefix, executable, args))
}

/// SHA-512 of the single packed tarball must equal the recipe's integrity.
fn verify_pack(pack_dir: &Path, expected: &str) -> bool {
    let Some(expected) = treer_protocol::integrity_sha512_bytes(expected) else {
        return false;
    };
    let Ok(entries) = fs::read_dir(pack_dir) else {
        return false;
    };
    let files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    let [tarball] = files.as_slice() else {
        return false;
    };
    let Ok(bytes) = fs::read(tarball) else {
        return false;
    };
    Sha512::digest(&bytes).as_slice() == expected.as_slice()
}

fn classify(
    outcome: &StepOutcome,
    purpose: &str,
) -> (ProviderCliOperationStatus, Option<&'static str>) {
    if !outcome.cleanup_confirmed {
        return (
            ProviderCliOperationStatus::Unknown,
            Some("cleanup_unconfirmed"),
        );
    }
    if outcome.timed_out {
        return (
            ProviderCliOperationStatus::TimedOut,
            Some("provider_cli_deadline"),
        );
    }
    if outcome.spawn_failed {
        return (
            ProviderCliOperationStatus::Failed,
            Some("provider_cli_spawn_failed"),
        );
    }
    if outcome.exit_code == Some(0) {
        return (ProviderCliOperationStatus::Succeeded, None);
    }
    (
        ProviderCliOperationStatus::Failed,
        Some(match purpose {
            "pack" => "provider_cli_pack_failed",
            "install" => "provider_cli_install_failed",
            "prefix" => "provider_cli_prefix_unresolved",
            _ => "provider_cli_check_failed",
        }),
    )
}

/// Private per-recipe job directory (0700) with the empty npm config files;
/// removed when dropped so nothing from a job survives except the result.
struct JobDirectory {
    path: PathBuf,
}

impl JobDirectory {
    fn create(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            treer_platform::secure_directory(parent)?;
            if let Some(root) = parent.parent() {
                treer_platform::secure_directory(root)?;
            }
        }
        if path.symlink_metadata().is_ok() {
            // A stale directory from an interrupted job is Controller-owned.
            fs::remove_dir_all(path)?;
        }
        fs::create_dir(path)?;
        treer_platform::secure_directory(path)?;
        for sub in ["home", "tmp", "cache", PACK_DIRECTORY] {
            fs::create_dir(path.join(sub))?;
            treer_platform::secure_directory(&path.join(sub))?;
        }
        for file in [USER_CONFIG, GLOBAL_CONFIG] {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path.join(file))?;
            treer_platform::secure_file(&path.join(file))?;
        }
        Ok(Self {
            path: path.to_owned(),
        })
    }
}

impl Drop for JobDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            if error.kind() != io::ErrorKind::NotFound {
                warn!(%error, path = %self.path.display(), "provider CLI job directory not removed");
            }
        }
    }
}

#[derive(Debug)]
struct StepOutcome {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    truncated: bool,
    duration_ms: u64,
    timed_out: bool,
    spawn_failed: bool,
    /// True when the step's process group is gone and every pipe holder has
    /// exited. False means a descendant may still be running.
    cleanup_confirmed: bool,
}

impl StepOutcome {
    fn result(&self, step: u32) -> ProviderCliStepResult {
        ProviderCliStepResult {
            step,
            exit_code: self.exit_code,
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
            truncated: self.truncated,
            duration_ms: self.duration_ms,
        }
    }

    fn spawn_failure(started: Instant) -> Self {
        Self {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            truncated: false,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            timed_out: false,
            spawn_failed: true,
            cleanup_confirmed: true,
        }
    }
}

/// Keeps the last `limit` bytes of a stream.
async fn read_tail<R>(mut reader: R, limit: usize) -> io::Result<(Vec<u8>, bool)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut retained = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        retained.extend_from_slice(&buffer[..read]);
        if retained.len() > limit {
            let excess = retained.len() - limit;
            retained.drain(..excess);
            truncated = true;
        }
    }
    Ok((retained, truncated))
}

/// Spawns one plan step directly (no shell) in its own process group with the
/// isolated environment, stdin closed and bounded output tails, then kills the
/// group at the deadline and confirms that nothing still holds the pipes.
async fn run_step(
    step: &ProviderCliPlanStep,
    environment: &BTreeMap<OsString, OsString>,
    cwd: &Path,
    deadline: Instant,
) -> StepOutcome {
    let started = Instant::now();
    let ProviderCliPlanStep::Command {
        executable, args, ..
    } = step
    else {
        return StepOutcome::spawn_failure(started);
    };
    let mut command = tokio::process::Command::new(executable);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            warn!(%error, "provider CLI step failed to start");
            return StepOutcome::spawn_failure(started);
        }
    };
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut stdout_task = tokio::spawn(async move {
        match stdout {
            Some(stdout) => read_tail(stdout, PROVIDER_CLI_OUTPUT_TAIL_BYTES).await,
            None => Ok((Vec::new(), true)),
        }
    });
    let mut stderr_task = tokio::spawn(async move {
        match stderr {
            Some(stderr) => read_tail(stderr, PROVIDER_CLI_OUTPUT_TAIL_BYTES).await,
            None => Ok((Vec::new(), true)),
        }
    });
    let remaining = deadline.saturating_duration_since(Instant::now());
    let (exit_code, timed_out) = match tokio::time::timeout(remaining, child.wait()).await {
        Ok(Ok(status)) => (status.code(), false),
        Ok(Err(_)) => (None, false),
        Err(_) => {
            kill_group(pid, &mut child).await;
            (None, true)
        }
    };
    // Pipe holders are the step's descendants: they must be gone too.
    let readers = tokio::time::timeout(DESCENDANT_GRACE, async {
        let out = (&mut stdout_task).await;
        let err = (&mut stderr_task).await;
        (out, err)
    })
    .await;
    let readers = match readers {
        Ok(readers) => Some(readers),
        Err(_) => {
            // Something still holds the pipes: kill the group and give the
            // readers a short second chance. A detached holder outside the
            // group survives this and leaves the step unconfirmed.
            kill_group(pid, &mut child).await;
            tokio::time::timeout(Duration::from_secs(2), async {
                let out = (&mut stdout_task).await;
                let err = (&mut stderr_task).await;
                (out, err)
            })
            .await
            .ok()
        }
    };
    let (stdout, stderr, truncated, mut cleanup_confirmed) = match readers {
        Some((Ok(Ok((out, out_truncated))), Ok(Ok((err, err_truncated))))) => {
            (out, err, out_truncated || err_truncated, true)
        }
        Some(_) => (Vec::new(), Vec::new(), true, true),
        None => {
            stdout_task.abort();
            stderr_task.abort();
            (Vec::new(), Vec::new(), true, false)
        }
    };
    if cleanup_confirmed {
        cleanup_confirmed = group_gone(pid).await;
    }
    StepOutcome {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        truncated,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        timed_out,
        spawn_failed: false,
        cleanup_confirmed,
    }
}

async fn kill_group(pid: Option<u32>, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        if let Err(error) = treer_platform::kill_process_group(pid) {
            warn!(%error, pid, "provider CLI step process group kill failed");
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Polls the process group for up to the descendant grace; true when empty.
async fn group_gone(pid: Option<u32>) -> bool {
    #[cfg(unix)]
    {
        let Some(pid) = pid else {
            return false;
        };
        let deadline = Instant::now() + DESCENDANT_GRACE;
        loop {
            match treer_platform::process_group_exists(pid) {
                Ok(false) => return true,
                Ok(true) => {
                    let _ = treer_platform::kill_process_group(pid);
                }
                Err(_) => return false,
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    #[cfg(not(unix))]
    {
        pid.is_some()
    }
}

#[cfg(test)]
#[path = "provider_cli_tests.rs"]
mod tests;
