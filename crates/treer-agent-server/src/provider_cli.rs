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
#[cfg(unix)]
use sha2::Sha256;
use sha2::{Digest, Sha512};
use tokio::io::AsyncReadExt;
use tracing::warn;
use treer_protocol::{
    plan_argv_hash, provider_cli_expected_node_arch, provider_cli_version_from_output,
    provider_cli_version_text, provider_cli_version_token_matches, NativeBinaryPin, ProtocolError,
    ProviderCheck, ProviderCliAction, ProviderCliCheckResponse, ProviderCliConsent,
    ProviderCliInstallAccepted, ProviderCliLifecycleScripts, ProviderCliObservation,
    ProviderCliObservationSummary, ProviderCliObservedState, ProviderCliOperationReport,
    ProviderCliOperationStatus, ProviderCliPackage, ProviderCliPlanStep, ProviderCliProbe,
    ProviderCliRecipeRef, ProviderCliRecipeView, ProviderCliRecipesResponse, ProviderCliStepResult,
    ProviderCliVerifiedFile, ValidatedProviderRecipe, ValidatedProviderTable,
    PROVIDER_CLI_CHECK_TIMEOUT_SECONDS, PROVIDER_CLI_OFFLINE_REGISTRY,
    PROVIDER_CLI_OPERATION_DEADLINE_SECONDS, PROVIDER_CLI_OUTPUT_TAIL_BYTES,
    PROVIDER_CLI_RETAINED_OPERATIONS, PROVIDER_CLI_RETENTION_SECONDS,
    PROVIDER_CLI_SPACE_FLOOR_BYTES,
};
use uuid::Uuid;

/// Directory below the Treer state dir that owns job directories and results.
pub const ROOT_DIRECTORY_NAME: &str = "provider-cli";
const STORE_FILE_NAME: &str = "operations.json";
const STORE_VERSION: u32 = 1;
const JOBS_DIRECTORY: &str = "jobs";
const CHECKS_DIRECTORY: &str = "checks";
const PACK_DIRECTORY: &str = "pack";
/// Native-binary entries (#131) pack the platform package into its own
/// directory so each integrity step still hashes exactly one tarball.
const PACK_NATIVE_DIRECTORY: &str = "pack-native";
const USER_CONFIG: &str = "npmrc";
const GLOBAL_CONFIG: &str = "npmrc-global";
const PROJECT_MANIFEST: &str = "package.json";
/// Largest tarball the integrity step will hash; the registry is trusted but
/// the read stays bounded.
const MAX_TARBALL_BYTES: u64 = 256 * 1024 * 1024;
/// How long readers may lag after the step's own process exited before the
/// surviving pipe holder is treated as an unconfirmed descendant.
const DESCENDANT_GRACE: Duration = Duration::from_secs(10);

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
///
/// Plain entries expand to five steps. A native-binary entry (#131) adds a
/// Node architecture probe first, packs and verifies the platform's native
/// package tarball too, installs both verified local tarballs in one
/// `--offline` command pinned to a registry that answers nothing (so the
/// reviewed postinstall can only use what was verified), and proves the
/// placed binary against the pinned bytes before the version probe.
pub(crate) fn expand_plan(
    recipe: &ValidatedProviderRecipe,
    npm: &str,
    node: Option<&str>,
    platform: &str,
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
    let native = match &recipe.recipe.native_binaries {
        Some(pins) => Some(pins.get(platform).ok_or_else(|| {
            ProtocolError::new(
                "provider_cli_recipe_unsupported",
                "recipe pins no native binary for this platform",
            )
        })?),
        None => None,
    };
    let mut steps = Vec::new();
    if native.is_some() {
        let node = node.ok_or_else(|| {
            ProtocolError::new(
                "provider_cli_node_unavailable",
                "node was not found on this machine's PATH",
            )
        })?;
        if provider_cli_expected_node_arch(platform).is_none() {
            return Err(ProtocolError::new(
                "provider_cli_recipe_unsupported",
                "platform has no Node architecture rule",
            ));
        }
        steps.push(ProviderCliPlanStep::Command {
            step: 1,
            purpose: "node-arch".into(),
            executable: node.to_owned(),
            args: vec!["-p".to_owned(), "process.arch".to_owned()],
        });
    }
    let pack_args = |package: &str, version: &str, destination: &str| {
        let mut pack = vec!["pack".to_owned(), format!("{package}@{version}")];
        pack.extend(registry_flags(package, &install.registry));
        pack.extend(config_flags(job));
        pack.push("--ignore-scripts".to_owned());
        pack.extend(QUIET_FLAGS.iter().map(|flag| (*flag).to_owned()));
        pack.push(format!(
            "--pack-destination={}",
            job.join(destination).display()
        ));
        pack
    };
    steps.push(ProviderCliPlanStep::Command {
        step: steps.len() as u32 + 1,
        purpose: "pack".into(),
        executable: npm.to_owned(),
        args: pack_args(&install.package, &install.version, PACK_DIRECTORY),
    });
    if let Some(pin) = native {
        steps.push(ProviderCliPlanStep::Command {
            step: steps.len() as u32 + 1,
            purpose: "pack-native".into(),
            executable: npm.to_owned(),
            args: pack_args(&pin.package, &pin.version, PACK_NATIVE_DIRECTORY),
        });
    }
    steps.push(ProviderCliPlanStep::Verify {
        step: steps.len() as u32 + 1,
        purpose: "integrity".into(),
        algorithm: "sha512".into(),
        expected: install.integrity.clone(),
    });
    if let Some(pin) = native {
        steps.push(ProviderCliPlanStep::Verify {
            step: steps.len() as u32 + 1,
            purpose: "integrity-native".into(),
            algorithm: "sha512".into(),
            expected: pin.integrity.clone(),
        });
    }
    let mut install_args = vec![
        "install".to_owned(),
        "--global".to_owned(),
        job.join(PACK_DIRECTORY)
            .join(tarball_name(&install.package, &install.version))
            .display()
            .to_string(),
    ];
    if let Some(pin) = native {
        install_args.push(
            job.join(PACK_NATIVE_DIRECTORY)
                .join(tarball_name(&pin.package, &pin.version))
                .display()
                .to_string(),
        );
        // Nothing may be fetched while scripts run: every registry points at
        // a closed port and npm is told it is offline.
        install_args.extend(registry_flags(
            &install.package,
            PROVIDER_CLI_OFFLINE_REGISTRY,
        ));
        install_args.push("--offline".to_owned());
    } else {
        install_args.extend(registry_flags(&install.package, &install.registry));
    }
    install_args.extend(config_flags(job));
    if install.omit_optional == Some(true) {
        install_args.push("--omit=optional".to_owned());
    }
    if install.ignore_scripts {
        install_args.push("--ignore-scripts".to_owned());
    }
    install_args.extend(QUIET_FLAGS.iter().map(|flag| (*flag).to_owned()));
    steps.push(ProviderCliPlanStep::Command {
        step: steps.len() as u32 + 1,
        purpose: "install".into(),
        executable: npm.to_owned(),
        args: install_args,
    });
    let mut prefix = vec!["prefix".to_owned(), "--global".to_owned()];
    prefix.extend(config_flags(job).into_iter().take(2));
    let prefix_step = steps.len() as u32 + 1;
    steps.push(ProviderCliPlanStep::Command {
        step: prefix_step,
        purpose: "prefix".into(),
        executable: npm.to_owned(),
        args: prefix,
    });
    if let Some(pin) = native {
        let relative_path = format!(
            "lib/node_modules/{}/{}",
            install.package,
            recipe.recipe.binary_path.as_deref().unwrap_or_default()
        );
        steps.push(ProviderCliPlanStep::VerifyFile {
            step: steps.len() as u32 + 1,
            purpose: "verify-file".into(),
            rule: format!(
                "<npm prefix --global>/{relative_path} is a regular file of {} bytes with sha256 {}; <npm prefix --global>/bin/{} is a symlink to it",
                pin.size, pin.sha256, check.executable
            ),
            relative_path,
            entry: check.executable.clone(),
            expected_size: pin.size,
            expected_sha256: pin.sha256.clone(),
            prefix_from_step: prefix_step,
        });
    }
    steps.push(ProviderCliPlanStep::Resolve {
        step: steps.len() as u32 + 1,
        purpose: "check".into(),
        rule: format!(
            "<npm prefix --global>/bin/{} {}",
            check.executable,
            check.args.join(" ")
        ),
        binary: check.executable.clone(),
        args: check.args.clone(),
        prefix_from_step: prefix_step,
    });
    Ok(steps)
}

fn check_step(prefix: &Path, executable: &str, args: &[String], step: u32) -> ProviderCliPlanStep {
    ProviderCliPlanStep::Command {
        step,
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
    /// Consent the operation was dispatched with (#134); absent on records
    /// written before the Controller observed installations.
    #[serde(default)]
    consent: Option<ProviderCliConsent>,
    /// Latest observation compared against the consent, path-free.
    #[serde(default)]
    observed_before: Option<ProviderCliObservationSummary>,
    /// #131: outcome of the plan's verify-file step, when it has one.
    #[serde(default)]
    verified_file: Option<ProviderCliVerifiedFile>,
    /// #131: the install step started and the operation did not succeed.
    #[serde(default)]
    prefix_modified: bool,
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
            action: self.consent.as_ref().map(|consent| consent.action),
            version_relation: self
                .consent
                .as_ref()
                .and_then(|consent| consent.version_relation),
            observation_hash: self
                .consent
                .as_ref()
                .map(|consent| consent.observation_hash.clone()),
            observed_before: self.observed_before.clone(),
            // Step data, like `steps` and `plan`, is reported once terminal.
            verified_file: terminal.then(|| self.verified_file.clone()).flatten(),
            prefix_modified: self.prefix_modified,
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
    /// Operation budget override; production derives the budget from the
    /// recipe (#131), tests shrink it to prove that nothing spawns once it
    /// is spent.
    operation_budget: Mutex<Option<Duration>>,
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
    #[cfg(all(test, unix))]
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
                operation_budget: Mutex::new(None),
            }),
        }
    }

    /// The recipe's own `deadline_seconds` (validated within the protocol
    /// bounds) or the default; a test override wins.
    fn operation_budget(&self, recipe: &ValidatedProviderRecipe) -> Duration {
        let override_budget = *self
            .inner
            .operation_budget
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        override_budget.unwrap_or_else(|| {
            Duration::from_secs(
                recipe
                    .recipe
                    .deadline_seconds
                    .unwrap_or(PROVIDER_CLI_OPERATION_DEADLINE_SECONDS),
            )
        })
    }

    #[cfg(all(test, unix))]
    pub(crate) fn set_operation_budget(&self, budget: Duration) {
        *self
            .inner
            .operation_budget
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(budget);
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

    /// Absolute node from the same PATH directories as npm (#131); required
    /// only by native-binary entries, whose first step reads its architecture.
    fn resolve_node(&self) -> Result<String, ProtocolError> {
        let mut environment = BTreeMap::new();
        if let Some(path) = self.search_path() {
            environment.insert("PATH".to_owned(), path.to_string_lossy().into_owned());
        }
        crate::controller::resolve_machine_command("node", &environment).map_err(|_| {
            ProtocolError::new(
                "provider_cli_node_unavailable",
                "node was not found on this machine's PATH",
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
        node: Option<&str>,
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
            expected_version: None,
            deadline_seconds: None,
            node_arch: None,
            native_binary: None,
        };
        if !recipe.supported {
            return view;
        }
        let Some(npm) = npm else {
            view.unsupported_reason = Some("npm_unavailable".into());
            return view;
        };
        let job = self.job_directory(JOBS_DIRECTORY, &recipe.recipe_id);
        match expand_plan(entry, npm, node, platform, &job) {
            Ok(plan) => {
                let install = recipe.install.as_ref();
                view.supported = true;
                view.expected_version = recipe.expected_version.clone();
                view.deadline_seconds = recipe.deadline_seconds;
                if let Some(pins) = &recipe.native_binaries {
                    view.native_binary = pins.get(platform).cloned();
                    view.node_arch = provider_cli_expected_node_arch(platform).map(str::to_owned);
                }
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
        let node = self.resolve_node().ok();
        Ok(ProviderCliRecipesResponse {
            schema_version: treer_protocol::PROVIDER_RECIPES_SCHEMA_VERSION,
            machine_id: self.inner.server_id.clone(),
            platform: platform.to_owned(),
            controller_source_sha: crate::controller::build_commit(),
            table_hash: table.table_hash.clone(),
            recipes: table
                .for_platform(platform)
                .map(|entry| self.view(entry, platform, npm.as_deref(), node.as_deref()))
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
        // Checks have no token and may run concurrently: each gets its own
        // directory so one cannot remove another's.
        let job = self.job_directory(
            CHECKS_DIRECTORY,
            &format!("{}-{}", entry.recipe.recipe_id, Uuid::new_v4().simple()),
        );
        let node = self.resolve_node().ok();
        let plan = expand_plan(entry, &npm, node.as_deref(), platform, &job)?;
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
        let check = entry
            .recipe
            .check
            .as_ref()
            .expect("supported entry has check");
        let observed = observe(
            &plan,
            check,
            &environment,
            &job,
            Instant::now() + Duration::from_secs(PROVIDER_CLI_CHECK_TIMEOUT_SECONDS * 2),
        )
        .await;
        let outcome = observed.outcome;
        let observation = observed.observation;
        let probe_succeeded = observation.probe.exit_code == Some(0)
            && observation.probe.cleanup_confirmed
            && observed.probed;
        let response = ProviderCliCheckResponse {
            installed: probe_succeeded,
            found_in_prefix: observation.found_in_prefix,
            version: probe_succeeded
                .then(|| version_text(&outcome.stdout))
                .filter(|text| !text.is_empty()),
            exit_code: outcome.exit_code,
            timed_out: outcome.timed_out,
            stdout: if observed.probed {
                outcome.stdout
            } else {
                String::new()
            },
            stderr: outcome.stderr,
            truncated: outcome.truncated,
            error_code: observation.probe.error_code.clone(),
            executable: observed
                .probed
                .then(|| observation.executable.clone())
                .flatten(),
            observation: Some(observation),
        };
        drop(job_dir);
        Ok(response)
    }

    /// Records and starts an installation. Returns once the record is durable
    /// and the plan is fixed; the Proxy polls `operation` for the result.
    pub fn install(
        &self,
        operation_id: &str,
        reference: &ProviderCliRecipeRef,
        table_hash: &str,
        argv_hash: &str,
        consent: &ProviderCliConsent,
    ) -> Result<ProviderCliInstallAccepted, ProtocolError> {
        if !operation_id_ok(operation_id) {
            return Err(ProtocolError::new(
                "provider_cli_recipe_invalid",
                "operation ID is invalid",
            ));
        }
        if !consent_shape_ok(consent) {
            return Err(ProtocolError::new(
                "provider_cli_consent_invalid",
                "the confirmed action cannot be installed",
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
        let node = self.resolve_node().ok();
        let job = self.job_directory(JOBS_DIRECTORY, &entry.recipe.recipe_id);
        let plan = expand_plan(entry, &npm, node.as_deref(), platform, &job)?;
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
                consent: Some(consent.clone()),
                observed_before: None,
                verified_file: None,
                prefix_modified: false,
            });
            if let Err(error) = store.persist() {
                store.operations.pop();
                return Err(store_error(error));
            }
        }
        let service = self.clone();
        let spawned_id = operation_id.to_owned();
        let spec = OperationSpec {
            check: entry
                .recipe
                .check
                .clone()
                .expect("supported entry has check"),
            expected_version: entry.recipe.expected_version.clone().unwrap_or_default(),
            native: entry
                .recipe
                .native_binaries
                .as_ref()
                .and_then(|pins| pins.get(platform).cloned()),
            expected_node_arch: provider_cli_expected_node_arch(platform),
            budget: self.operation_budget(entry),
        };
        let target = ConsentTarget {
            consent: consent.clone(),
            recipe_hash: entry.recipe_hash.clone(),
            target_version: entry
                .recipe
                .install
                .as_ref()
                .map(|install| install.version.clone())
                .unwrap_or_default(),
        };
        tokio::spawn(async move {
            service
                .run_operation(&spawned_id, plan, job, spec, target)
                .await;
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

    /// Re-observes the prefix under the operation's own deadline and decides
    /// whether the next step may run. Runs before any mutation and again
    /// immediately before the install step. Process facts come first: an
    /// observation whose descendants were not confirmed gone stops the
    /// operation as `unknown` (nothing after it may spawn), an exhausted
    /// budget stops it as timed out, and only then is the consent compared.
    /// The remaining window is a TOCTOU between this observation and npm
    /// writing the prefix, bounded by the operation budget, not by a
    /// millisecond promise.
    async fn consent_gate(
        &self,
        operation_id: &str,
        gate: &GateContext<'_>,
    ) -> (ConsentGate, Option<PathBuf>) {
        let GateContext {
            plan,
            check,
            environment,
            job,
            target,
            deadline,
        } = *gate;
        if Instant::now() >= deadline {
            return (ConsentGate::Expired, None);
        }
        let observed = observe(plan, check, environment, job, deadline).await;
        let summary = observed
            .observation
            .summary(&target.recipe_hash, &target.target_version);
        self.update(operation_id, |record| {
            record.observed_before = Some(summary);
        });
        // The observed global prefix (resolved whether or not anything is
        // installed there) lets a native entry check its volume before the
        // install step writes it.
        let prefix = observed.prefix.clone();
        if !observed.observation.probe.cleanup_confirmed {
            return (ConsentGate::Unconfirmed, prefix);
        }
        // Only the operation budget expires the operation; a single probe
        // that hung within budget is an unknown observation and fails the
        // comparison below as a state change.
        if Instant::now() >= deadline {
            return (ConsentGate::Expired, prefix);
        }
        let gate = if target.consent.matches(
            &observed.observation,
            &target.recipe_hash,
            &target.target_version,
        ) {
            ConsentGate::Holds
        } else {
            ConsentGate::Changed
        };
        (gate, prefix)
    }

    async fn run_operation(
        &self,
        operation_id: &str,
        plan: Vec<ProviderCliPlanStep>,
        job: PathBuf,
        spec: OperationSpec,
        target: ConsentTarget,
    ) {
        let started = Instant::now();
        let deadline = started + spec.budget;
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
        let first_command = plan.iter().find_map(|step| match step {
            ProviderCliPlanStep::Command { step, .. } => Some(*step),
            _ => None,
        });
        // Native entries need room for the tarballs, the unpacked package and
        // the placed copy: a floor plus twice the unpacked native size.
        let required_space = spec.native.as_ref().map(|pin| {
            PROVIDER_CLI_SPACE_FLOOR_BYTES.saturating_add(pin.unpacked_size.saturating_mul(2))
        });
        let mut prefix: Option<PathBuf> = None;
        let mut final_status = ProviderCliOperationStatus::Succeeded;
        let mut error_code: Option<&'static str> = None;
        let mut last_exit: Option<i32> = None;
        let mut timed_out = false;
        let mut install_started = false;
        for step in &plan {
            let number = step.step();
            self.update(operation_id, |record| record.step = Some(number));
            match step {
                ProviderCliPlanStep::Verify {
                    purpose, expected, ..
                } => {
                    let directory = if purpose == "integrity-native" {
                        PACK_NATIVE_DIRECTORY
                    } else {
                        PACK_DIRECTORY
                    };
                    if !verify_pack(&job.join(directory), expected) {
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
                    // The consent gate: once before anything runs and once
                    // right before the step that rewrites the global prefix.
                    if Some(number) == first_command || purpose == "install" {
                        let (gate, observed_prefix) = self
                            .consent_gate(
                                operation_id,
                                &GateContext {
                                    plan: &plan,
                                    check: &spec.check,
                                    environment: &environment,
                                    job: &job,
                                    target: &target,
                                    deadline,
                                },
                            )
                            .await;
                        let stop = match gate {
                            ConsentGate::Holds => None,
                            ConsentGate::Unconfirmed => {
                                Some((ProviderCliOperationStatus::Unknown, "cleanup_unconfirmed"))
                            }
                            ConsentGate::Expired => Some((
                                ProviderCliOperationStatus::TimedOut,
                                "provider_cli_deadline",
                            )),
                            ConsentGate::Changed => Some((
                                ProviderCliOperationStatus::Failed,
                                "provider_cli_state_changed",
                            )),
                        };
                        if let Some((status, code)) = stop {
                            final_status = status;
                            error_code = Some(code);
                            timed_out |= status == ProviderCliOperationStatus::TimedOut;
                            break;
                        }
                        // The prefix volume is checked before it is written.
                        if purpose == "install"
                            && required_space.is_some_and(|required| {
                                observed_prefix
                                    .as_deref()
                                    .is_some_and(|prefix| !space_available(prefix, required))
                            })
                        {
                            final_status = ProviderCliOperationStatus::Failed;
                            error_code = Some("provider_cli_space_insufficient");
                            break;
                        }
                    }
                    if purpose == "pack"
                        && required_space.is_some_and(|required| !space_available(&job, required))
                    {
                        final_status = ProviderCliOperationStatus::Failed;
                        error_code = Some("provider_cli_space_insufficient");
                        break;
                    }
                    if purpose == "install" {
                        install_started = true;
                    }
                    let outcome = run_step(step, &environment, &job, deadline).await;
                    let (status, code) = classify(&outcome, purpose);
                    last_exit = outcome.exit_code;
                    timed_out |= outcome.timed_out;
                    let result = outcome.result(number);
                    if purpose == "prefix" {
                        prefix = parse_prefix(&outcome);
                    }
                    // The Node that will run the postinstall must be the
                    // platform's native architecture, or npm would select a
                    // different native package than the one pinned.
                    let arch_ok = purpose != "node-arch"
                        || single_token(&outcome).as_deref() == spec.expected_node_arch;
                    self.update(operation_id, |record| record.steps.push(result));
                    if status != ProviderCliOperationStatus::Succeeded {
                        final_status = status;
                        error_code = code;
                        break;
                    }
                    if !arch_ok {
                        final_status = ProviderCliOperationStatus::Failed;
                        error_code = Some("provider_cli_node_arch_mismatch");
                        break;
                    }
                    if purpose == "prefix" {
                        let Some(prefix_path) = prefix.as_deref() else {
                            final_status = ProviderCliOperationStatus::Failed;
                            error_code = Some("provider_cli_prefix_unresolved");
                            break;
                        };
                        if required_space
                            .is_some_and(|required| !space_available(prefix_path, required))
                        {
                            final_status = ProviderCliOperationStatus::Failed;
                            error_code = Some("provider_cli_space_insufficient");
                            break;
                        }
                    }
                }
                ProviderCliPlanStep::VerifyFile {
                    relative_path,
                    entry,
                    expected_size,
                    expected_sha256,
                    ..
                } => {
                    let Some(prefix_path) = prefix.clone() else {
                        final_status = ProviderCliOperationStatus::Failed;
                        error_code = Some("provider_cli_prefix_unresolved");
                        break;
                    };
                    let (relative, entry, size, digest) = (
                        relative_path.clone(),
                        entry.clone(),
                        *expected_size,
                        expected_sha256.clone(),
                    );
                    let verified = tokio::task::spawn_blocking(move || {
                        verify_file(&prefix_path, &relative, &entry, size, &digest)
                    })
                    .await
                    .unwrap_or(Err("path_refused"));
                    match verified {
                        Ok((size, sha256)) => self.update(operation_id, |record| {
                            record.verified_file = Some(ProviderCliVerifiedFile {
                                size: Some(size),
                                sha256: Some(sha256),
                                failure: None,
                            });
                            record.steps.push(ProviderCliStepResult {
                                step: number,
                                exit_code: Some(0),
                                stdout: String::new(),
                                stderr: String::new(),
                                truncated: false,
                                duration_ms: 0,
                            });
                        }),
                        Err(failure) => {
                            self.update(operation_id, |record| {
                                record.verified_file = Some(ProviderCliVerifiedFile {
                                    size: None,
                                    sha256: None,
                                    failure: Some(failure.to_owned()),
                                });
                            });
                            final_status = ProviderCliOperationStatus::Failed;
                            error_code = Some("provider_cli_binary_mismatch");
                            break;
                        }
                    }
                }
                ProviderCliPlanStep::Resolve { .. } => {
                    let Some(prefix_path) = prefix.as_deref() else {
                        final_status = ProviderCliOperationStatus::Failed;
                        error_code = Some("provider_cli_prefix_unresolved");
                        break;
                    };
                    let Some(check_command) = resolve_check(
                        prefix_path,
                        &spec.check.executable,
                        &spec.check.args,
                        number,
                    ) else {
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
                    // Exit 0 is not success: the probe must print the pinned
                    // version as a complete token.
                    if outcome.truncated
                        || !provider_cli_version_token_matches(
                            &outcome.stdout,
                            &spec.expected_version,
                        )
                    {
                        final_status = ProviderCliOperationStatus::Failed;
                        error_code = Some("provider_cli_version_mismatch");
                        break;
                    }
                }
            }
        }
        drop(job_dir);
        let prefix_modified =
            install_started && final_status != ProviderCliOperationStatus::Succeeded;
        self.update(operation_id, |record| {
            record.prefix_modified = prefix_modified
        });
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
    provider_cli_version_text(stdout).unwrap_or_default()
}

fn consent_shape_ok(consent: &ProviderCliConsent) -> bool {
    consent.action != ProviderCliAction::Noop
        && consent.observation_hash.len() == 64
        && consent
            .observation_hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Recipe facts one operation runs with besides its plan (#131).
struct OperationSpec {
    check: ProviderCheck,
    /// Complete token the probe's first line must carry.
    expected_version: String,
    /// This platform's pinned native member for native-binary entries.
    native: Option<NativeBinaryPin>,
    /// Node `process.arch` the native entry's first step must print.
    expected_node_arch: Option<&'static str>,
    budget: Duration,
}

/// What a running operation compares fresh observations against.
struct ConsentTarget {
    consent: ProviderCliConsent,
    recipe_hash: String,
    target_version: String,
}

/// Everything one consent gate needs besides the operation ID.
#[derive(Clone, Copy)]
struct GateContext<'a> {
    plan: &'a [ProviderCliPlanStep],
    check: &'a ProviderCheck,
    environment: &'a BTreeMap<OsString, OsString>,
    job: &'a Path,
    target: &'a ConsentTarget,
    deadline: Instant,
}

/// Outcome of one consent gate, in the order it is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsentGate {
    Holds,
    /// The observation's own processes were not confirmed gone.
    Unconfirmed,
    /// The operation budget is spent; nothing more may spawn.
    Expired,
    /// A fresh observation no longer supports the consent.
    Changed,
}

struct Observed {
    observation: ProviderCliObservation,
    /// Probe outcome when the probe ran, else the prefix step's outcome.
    outcome: StepOutcome,
    probed: bool,
    /// The resolved global prefix when `npm prefix --global` answered, also
    /// for an empty prefix (#131: the install gate checks its volume).
    prefix: Option<PathBuf>,
}

fn probe_of(outcome: &StepOutcome, error_code: Option<&str>) -> ProviderCliProbe {
    ProviderCliProbe {
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        cleanup_confirmed: outcome.cleanup_confirmed,
        error_code: error_code.map(str::to_owned),
    }
}

/// One observation of the global prefix for the recipe's binary (#134):
/// `npm prefix --global` under the private job environment, an `lstat` of
/// `<prefix>/bin/<binary>`, and the recipe's version probe only when a file
/// inside the prefix is there. Every classification names its evidence; an
/// error is never read as absence.
async fn observe(
    plan: &[ProviderCliPlanStep],
    check: &ProviderCheck,
    environment: &BTreeMap<OsString, OsString>,
    job: &Path,
    deadline: Instant,
) -> Observed {
    let prefix_step = plan
        .iter()
        .find(|step| matches!(step, ProviderCliPlanStep::Command { purpose, .. } if purpose == "prefix"))
        .cloned()
        .expect("plan has prefix step");
    let probe_timeout = Duration::from_secs(PROVIDER_CLI_CHECK_TIMEOUT_SECONDS);
    // Each process gets at most the probe budget and never outlives the
    // caller's deadline; `run_step` refuses to spawn once that has passed.
    let step_deadline = || deadline.min(Instant::now() + probe_timeout);
    // Native entries (#131) also report the selected Node's architecture so
    // the Proxy can refuse `prepare` before anything is fetched.
    let mut node_arch = None;
    if let Some(arch_step) = plan
        .iter()
        .find(|step| matches!(step, ProviderCliPlanStep::Command { purpose, .. } if purpose == "node-arch"))
    {
        node_arch = single_token(&run_step(arch_step, environment, job, step_deadline()).await);
    }
    let check_number = check_step_number(plan);
    let prefix = run_step(&prefix_step, environment, job, step_deadline()).await;
    // A truncated prefix answer has no trustworthy first line.
    let prefix_path = (!prefix.truncated && prefix.cleanup_confirmed)
        .then(|| parse_prefix(&prefix))
        .flatten();
    let Some(prefix_path) = prefix_path else {
        let code = if !prefix.cleanup_confirmed {
            "cleanup_unconfirmed"
        } else {
            "provider_cli_prefix_unresolved"
        };
        return Observed {
            observation: ProviderCliObservation {
                state: ProviderCliObservedState::Unknown,
                current_version: None,
                version_text: None,
                executable: None,
                found_in_prefix: false,
                probe: probe_of(&prefix, Some(code)),
                node_arch: node_arch.clone(),
            },
            outcome: StepOutcome {
                stdout: String::new(),
                ..prefix
            },
            probed: false,
            prefix: None,
        };
    };
    let candidate = prefix_path.join("bin").join(&check.executable);
    let executable = Some(candidate.display().to_string());
    let unprobed = |state, found_in_prefix, code: &str, outcome: StepOutcome| Observed {
        observation: ProviderCliObservation {
            state,
            current_version: None,
            version_text: None,
            executable: executable.clone(),
            found_in_prefix,
            probe: ProviderCliProbe {
                exit_code: None,
                timed_out: false,
                cleanup_confirmed: true,
                error_code: Some(code.to_owned()),
            },
            node_arch: node_arch.clone(),
        },
        outcome,
        probed: false,
        prefix: Some(prefix_path.clone()),
    };
    let silent = StepOutcome {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: None,
        ..prefix.clone()
    };
    match fs::symlink_metadata(&candidate) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return unprobed(
                ProviderCliObservedState::Absent,
                false,
                "provider_cli_binary_not_found",
                silent,
            );
        }
        Err(_) => {
            return unprobed(
                ProviderCliObservedState::Unknown,
                false,
                "provider_cli_stat_failed",
                silent,
            );
        }
        Ok(_) => {}
    }
    let Some(step) = resolve_check(&prefix_path, &check.executable, &check.args, check_number)
    else {
        // Something occupies the slot but is not a regular file inside the
        // prefix (an escaping symlink, a directory): never executed.
        return unprobed(
            ProviderCliObservedState::Unknown,
            true,
            "provider_cli_binary_outside_prefix",
            silent,
        );
    };
    let probe = run_step(&step, environment, job, step_deadline()).await;
    // Only an untruncated tail has a first line; a truncated one could hide
    // the real first line behind a crafted tail.
    let current_version = (!probe.truncated)
        .then(|| provider_cli_version_from_output(&probe.stdout))
        .flatten();
    let error_code = if !probe.cleanup_confirmed {
        Some("cleanup_unconfirmed")
    } else if probe.timed_out {
        Some("provider_cli_probe_timed_out")
    } else if probe.exit_code != Some(0) {
        Some("provider_cli_probe_failed")
    } else if probe.truncated {
        Some("provider_cli_probe_output_truncated")
    } else if current_version.is_none() {
        Some("provider_cli_version_unparsed")
    } else {
        None
    };
    let state = if error_code.is_none() {
        ProviderCliObservedState::Installed
    } else {
        ProviderCliObservedState::Unknown
    };
    Observed {
        observation: ProviderCliObservation {
            state,
            current_version: current_version.filter(|_| error_code.is_none()),
            version_text: provider_cli_version_text(&probe.stdout),
            executable,
            found_in_prefix: true,
            probe: probe_of(&probe, error_code),
            node_arch,
        },
        outcome: probe,
        probed: true,
        prefix: Some(prefix_path),
    }
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
fn resolve_check(
    prefix: &Path,
    executable: &str,
    args: &[String],
    step: u32,
) -> Option<ProviderCliPlanStep> {
    let candidate = prefix.join("bin").join(executable);
    let target = fs::canonicalize(&candidate).ok()?;
    let prefix_real = fs::canonicalize(prefix).ok()?;
    if !target.starts_with(&prefix_real) || !target.is_file() {
        return None;
    }
    Some(check_step(prefix, executable, args, step))
}

/// Step number of the plan's resolve (probe) step.
fn check_step_number(plan: &[ProviderCliPlanStep]) -> u32 {
    plan.iter()
        .find_map(|step| match step {
            ProviderCliPlanStep::Resolve { step, .. } => Some(*step),
            _ => None,
        })
        .unwrap_or(5)
}

/// First non-empty line of an untruncated, successful step, trimmed.
fn single_token(outcome: &StepOutcome) -> Option<String> {
    if outcome.exit_code != Some(0) || outcome.truncated || !outcome.cleanup_confirmed {
        return None;
    }
    outcome
        .stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .filter(|line| line.len() <= treer_protocol::PROVIDER_CLI_NODE_ARCH_MAX)
        .map(str::to_owned)
}

/// An unknown answer fails closed where a native pin requires space.
fn space_available(path: &Path, required: u64) -> bool {
    treer_platform::available_disk_bytes(path).is_ok_and(|free| free >= required)
}

/// Read-only proof that `<prefix>/<relative>` is exactly the pinned native
/// binary (#131): the canonical prefix is opened once, every component below
/// it is opened with `O_NOFOLLOW`, each directory and the file are owned by
/// root or this account and not group/world-writable, the file is regular,
/// its size and streaming SHA-256 equal the pin, and `<prefix>/bin/<entry>`
/// is a symlink that resolves to that same inode. The hard link the package's
/// postinstall prefers (nlink 2) is expected and not checked. Returns the
/// measured size and digest, or the failed check's category.
#[cfg(unix)]
fn verify_file(
    prefix: &Path,
    relative: &str,
    entry: &str,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(u64, String), &'static str> {
    use rustix::fs::{self as sys, AtFlags, FileType, Mode, OFlags};
    const REFUSED: &str = "path_refused";
    let prefix_real = fs::canonicalize(prefix).map_err(|_| REFUSED)?;
    let components: Vec<&str> = relative.split('/').collect();
    let (last, dirs) = components.split_last().ok_or(REFUSED)?;
    if components
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err(REFUSED);
    }
    let euid = rustix::process::geteuid().as_raw();
    let owned_and_private =
        |stat: &sys::Stat| (stat.st_uid == 0 || stat.st_uid == euid) && (stat.st_mode & 0o022) == 0;
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    // The prefix root may itself be reached through symlinks (canonicalized
    // above); everything below it is opened without following.
    let root: fs::File = sys::open(
        &prefix_real,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| REFUSED)?
    .into();
    if !owned_and_private(&sys::fstat(&root).map_err(|_| REFUSED)?) {
        return Err(REFUSED);
    }
    let mut dir = root.try_clone().map_err(|_| REFUSED)?;
    for name in dirs {
        let next: fs::File = sys::openat(&dir, *name, directory_flags, Mode::empty())
            .map_err(|_| REFUSED)?
            .into();
        if !owned_and_private(&sys::fstat(&next).map_err(|_| REFUSED)?) {
            return Err(REFUSED);
        }
        dir = next;
    }
    let mut file: fs::File = sys::openat(
        &dir,
        *last,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| REFUSED)?
    .into();
    let stat = sys::fstat(&file).map_err(|_| REFUSED)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err("not_regular");
    }
    if !owned_and_private(&stat) {
        return Err(REFUSED);
    }
    let size = u64::try_from(stat.st_size).map_err(|_| "native_content")?;
    if size != expected_size {
        return Err("native_content");
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = io::Read::read(&mut file, &mut buffer).map_err(|_| "native_content")?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > expected_size {
            return Err("native_content");
        }
        hasher.update(&buffer[..read]);
    }
    let digest = format!("{:x}", hasher.finalize());
    if total != expected_size || digest != expected_sha256 {
        return Err("native_content");
    }
    // The user-facing entry must be a symlink that leads to this very file:
    // `bin` is opened below the prefix under the same ownership rules (a
    // symlinked or group-writable `bin` is refused), the link is read without
    // following, its target must normalize against the prefix to exactly the
    // verified path, and opening through it must reach the same inode.
    const ENTRY: &str = "entry_link";
    let bin: fs::File = sys::openat(&root, "bin", directory_flags, Mode::empty())
        .map_err(|_| ENTRY)?
        .into();
    if !owned_and_private(&sys::fstat(&bin).map_err(|_| ENTRY)?) {
        return Err(ENTRY);
    }
    let link = sys::statat(&bin, entry, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| ENTRY)?;
    if FileType::from_raw_mode(link.st_mode) != FileType::Symlink {
        return Err(ENTRY);
    }
    let target = sys::readlinkat(&bin, entry, Vec::new()).map_err(|_| ENTRY)?;
    let target = target.to_str().map_err(|_| ENTRY)?;
    if entry_target_within(target, &prefix_real).as_deref() != Some(relative) {
        return Err(ENTRY);
    }
    let through: fs::File = sys::openat(
        &bin,
        entry,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| ENTRY)?
    .into();
    let target_stat = sys::fstat(&through).map_err(|_| ENTRY)?;
    if target_stat.st_dev != stat.st_dev || target_stat.st_ino != stat.st_ino {
        return Err(ENTRY);
    }
    Ok((size, digest))
}

/// Lexically resolves a symlink target read from `<prefix>/bin` to a path
/// relative to the prefix: `..` pops, nothing may escape the prefix, and an
/// absolute target must start inside it. `None` when it escapes.
#[cfg(unix)]
fn entry_target_within(target: &str, prefix_real: &Path) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    let rest = if let Some(stripped) = target.strip_prefix('/') {
        let prefix = prefix_real.to_str()?.trim_end_matches('/');
        let inside = format!("/{stripped}");
        inside
            .strip_prefix(prefix)?
            .strip_prefix('/')
            .map(str::to_owned)?
    } else {
        parts.push("bin");
        target.to_owned()
    };
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(not(unix))]
fn verify_file(
    _prefix: &Path,
    _relative: &str,
    _entry: &str,
    _expected_size: u64,
    _expected_sha256: &str,
) -> Result<(u64, String), &'static str> {
    Err("path_refused")
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
    let Ok(metadata) = fs::metadata(tarball) else {
        return false;
    };
    if metadata.len() > MAX_TARBALL_BYTES {
        return false;
    }
    let Ok(mut file) = fs::File::open(tarball) else {
        return false;
    };
    let mut hasher = Sha512::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut remaining = MAX_TARBALL_BYTES;
    loop {
        let read = match io::Read::read(&mut file, &mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => return false,
        };
        remaining = match remaining.checked_sub(read as u64) {
            Some(remaining) => remaining,
            None => return false,
        };
        hasher.update(&buffer[..read]);
    }
    hasher.finalize().as_slice() == expected.as_slice()
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
        for sub in [
            "home",
            "tmp",
            "cache",
            PACK_DIRECTORY,
            PACK_NATIVE_DIRECTORY,
        ] {
            fs::create_dir(path.join(sub))?;
            treer_platform::secure_directory(&path.join(sub))?;
        }
        // npm walks up from cwd looking for a project (package.json or
        // node_modules) and would read that project's .npmrc. A private
        // package.json pins the local prefix to this directory, which has none.
        for (file, contents) in [
            (USER_CONFIG, &b""[..]),
            (GLOBAL_CONFIG, &b""[..]),
            (PROJECT_MANIFEST, &b"{\"private\":true}\n"[..]),
        ] {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file_handle = options.open(path.join(file))?;
            io::Write::write_all(&mut file_handle, contents)?;
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

#[derive(Debug, Clone)]
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

    /// The deadline had passed before anything was spawned.
    fn expired(started: Instant) -> Self {
        Self {
            timed_out: true,
            spawn_failed: false,
            ..Self::spawn_failure(started)
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
    if started >= deadline {
        // The budget is spent: nothing new is started.
        return StepOutcome::expired(started);
    }
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
