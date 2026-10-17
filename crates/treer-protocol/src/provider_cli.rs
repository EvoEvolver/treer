//! Provider CLI installation contract (Issue #123 part 2).
//!
//! Shared models for the launcher-owned recipe table embedded in every Core
//! release, its strict validation and hashing, the human-session Proxy routes
//! and the Controller commands behind them. The table is release data: Core
//! never reads an installed or user-editable copy, never accepts browser argv,
//! and withholds the capability when the embedded bytes do not validate.
//!
//! Hashing rules are shared with `launchers/agent/scripts/provider-recipes.mjs`:
//! `table_hash` is SHA-256 of the exact embedded UTF-8 bytes; `recipe_hash` is
//! SHA-256 of the compact JSON of one recipe with object keys sorted
//! recursively, array order kept and the top-level `recipe_hash` excluded.
use std::collections::{BTreeMap, BTreeSet};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ProtocolError;

/// Advertised by the Proxy health document and by Controllers whose embedded
/// table validates. Old peers fail explicitly with 501.
pub const CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL: &str = "machine.provider-cli-install.v1";
pub const PROVIDER_RECIPES_SCHEMA: &str = "treer.provider-recipes/v1";
pub const PROVIDER_RECIPES_SCHEMA_VERSION: u64 = 2;
/// Advertised next to the install capabilities by Controllers whose plans may
/// carry `verify_file` steps and whose recipes carry `expected_version` (#131).
/// A Web validator that only knows the five-step plan fails closed on them.
pub const CAPABILITY_MACHINE_PROVIDER_CLI_PLAN_V2: &str = "machine.provider-cli-plan.v2";
/// Exact release bytes; the launcher generates them and ships golden hashes.
pub const PROVIDER_RECIPES_TABLE: &str =
    include_str!("../../../launchers/agent/provider-recipes.json");
pub const PROVIDER_CLI_RELEASE_PLATFORMS: [&str; 5] = [
    "darwin-aarch64",
    "darwin-x86_64",
    "linux-aarch64",
    "linux-x86_64",
    "windows-x86_64",
];
/// Registries a table may name. Extending this list is a Core code change.
pub const PROVIDER_CLI_NPM_REGISTRIES: [&str; 1] = ["https://registry.npmjs.org/"];
/// The only version-probe argument lists a recipe may carry.
pub const PROVIDER_CLI_CHECK_ARGS: [&str; 2] = ["--version", "version"];
pub const PROVIDER_CLI_NODE_MIN: u64 = 22;
pub const PROVIDER_CLI_TOKEN_TTL_SECONDS: i64 = 120;
pub const PROVIDER_CLI_MAX_LIVE_TOKENS: i64 = 4;
pub const PROVIDER_CLI_PREPARES_PER_MINUTE: i64 = 10;
pub const PROVIDER_CLI_OPERATION_DEADLINE_SECONDS: u64 = 300;
/// Bounds for a recipe's own `deadline_seconds` (#131).
pub const PROVIDER_CLI_OPERATION_DEADLINE_MIN_SECONDS: u64 = 60;
pub const PROVIDER_CLI_OPERATION_DEADLINE_MAX_SECONDS: u64 = 900;
/// Largest native binary a recipe may pin, and the free-space floor the
/// Controller requires on top of twice the unpacked native size.
pub const PROVIDER_CLI_NATIVE_MAX_BYTES: u64 = 1024 * 1024 * 1024;
pub const PROVIDER_CLI_SPACE_FLOOR_BYTES: u64 = 1024 * 1024 * 1024;
/// Registry the scripted install step is pinned to: nothing answers there, so
/// the step can only use the verified local tarballs and any fetch npm
/// attempts fails closed instead of silently succeeding.
pub const PROVIDER_CLI_OFFLINE_REGISTRY: &str = "http://127.0.0.1:1/";
pub const PROVIDER_CLI_CHECK_TIMEOUT_SECONDS: u64 = 10;
pub const PROVIDER_CLI_OUTPUT_TAIL_BYTES: usize = 32 * 1024;
pub const PROVIDER_CLI_RETENTION_SECONDS: i64 = 24 * 60 * 60;
pub const PROVIDER_CLI_RETAINED_OPERATIONS: usize = 64;
pub const PROVIDER_CLI_POLL_AFTER_MS: u64 = 1000;
/// A dispatched operation whose Controller has not reported for this long is
/// marked `unknown`; the job is not assumed dead or alive.
pub const PROVIDER_CLI_CONTROLLER_GRACE_SECONDS: i64 = 600;
pub const PROVIDER_CLI_TABLE_INVALID: &str = "provider_cli_table_invalid";

struct KnownProvider {
    package: Option<&'static str>,
    names: &'static [&'static str],
}

fn known_provider(provider_id: &str) -> Option<KnownProvider> {
    Some(match provider_id {
        "codex" => KnownProvider {
            package: Some("@openai/codex"),
            names: &["codex"],
        },
        "claude" => KnownProvider {
            package: Some("@anthropic-ai/claude-code"),
            names: &["claude"],
        },
        "gemini" => KnownProvider {
            package: Some("@google/gemini-cli"),
            names: &["gemini"],
        },
        "opencode" => KnownProvider {
            package: Some("opencode-ai"),
            names: &["opencode"],
        },
        "cursor" => KnownProvider {
            package: None,
            names: &["agent", "cursor-agent"],
        },
        "grok" => KnownProvider {
            package: None,
            names: &["grok"],
        },
        _ => return None,
    })
}

const KNOWN_PROVIDERS: [&str; 6] = ["codex", "claude", "gemini", "opencode", "cursor", "grok"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRecipeTable {
    pub schema: String,
    pub schema_version: u64,
    pub recipes: Vec<ProviderRecipe>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRecipe {
    pub provider_id: String,
    pub recipe_id: String,
    pub recipe_version: u64,
    pub platforms: Vec<String>,
    pub binary_names: Vec<String>,
    pub supported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_min: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<NpmGlobalInstall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<ProviderCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_review: Option<LifecycleReview>,
    /// Exact version the probe must print as a complete token (#131);
    /// required on supported entries and equal to `install.version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
    /// Operation budget override, bounded by the protocol constants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_seconds: Option<u64>,
    /// Native-binary entries only: the file the package's reviewed postinstall
    /// places, relative to the installed root package directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    /// Native-binary entries only: one pinned member per supported platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_binaries: Option<BTreeMap<String, NativeBinaryPin>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported_reason: Option<String>,
    pub official_docs: String,
    pub login_hint: String,
}

/// The platform-specific optional dependency a native-binary recipe installs
/// from a verified local tarball, and the bytes its postinstall must place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeBinaryPin {
    pub package: String,
    pub version: String,
    /// SHA-512 of the native package tarball, verified before any script runs.
    pub integrity: String,
    /// Tarball member the binary bytes come from (`package/...`).
    pub member: String,
    /// Exact size and SHA-256 the placed file must have.
    pub size: u64,
    pub sha256: String,
    /// npm's unpacked size of the native package; drives the space check.
    pub unpacked_size: u64,
}

/// Typed package data, never executable/argv input. Core alone composes the
/// npm invocation and its isolation flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NpmGlobalInstall {
    pub kind: String,
    pub package: String,
    pub version: String,
    pub integrity: String,
    pub registry: String,
    pub ignore_scripts: bool,
    /// Explicit on native-binary entries (always `false`): the optional
    /// native dependency is installed, never omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omit_optional: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCheck {
    pub executable: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleReview {
    pub reviewed_at: String,
    pub reviewer: String,
    pub files: Vec<LifecycleReviewFile>,
    /// Script name to the exact command from the pinned `package.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripts: Option<BTreeMap<String, String>>,
    /// The scripts npm runs on a tarball global install (`postinstall`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<Vec<String>>,
    /// Expected installed closure: the root plus one native package, and only
    /// the root carries scripts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closure: Option<LifecycleClosure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleClosure {
    pub packages: u64,
    pub with_scripts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleReviewFile {
    pub member: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedProviderRecipe {
    pub recipe: ProviderRecipe,
    pub recipe_hash: String,
}

/// A table that passed every rule; the only form Core expands commands from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedProviderTable {
    pub table_hash: String,
    pub recipes: Vec<ValidatedProviderRecipe>,
}

impl ValidatedProviderTable {
    /// Validates exact release bytes. Any violation invalidates the whole table.
    pub fn parse(bytes: &str) -> Result<Self, ProtocolError> {
        let raw: Value = serde_json::from_str(bytes).map_err(|_| invalid("table is not JSON"))?;
        let table: ProviderRecipeTable =
            serde_json::from_value(raw.clone()).map_err(|_| invalid("table shape"))?;
        if table.schema != PROVIDER_RECIPES_SCHEMA
            || table.schema_version != PROVIDER_RECIPES_SCHEMA_VERSION
        {
            return Err(invalid("schema"));
        }
        let raw_recipes = raw
            .get("recipes")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("recipes"))?;
        if table.recipes.is_empty() || raw_recipes.len() != table.recipes.len() {
            return Err(invalid("recipes"));
        }
        let mut ids = BTreeSet::new();
        let mut coverage = BTreeSet::new();
        let mut recipes = Vec::with_capacity(table.recipes.len());
        for (recipe, raw_recipe) in table.recipes.into_iter().zip(raw_recipes) {
            validate_recipe(&recipe, &mut ids, &mut coverage)?;
            let recipe_hash = recipe_hash(raw_recipe)?;
            recipes.push(ValidatedProviderRecipe {
                recipe,
                recipe_hash,
            });
        }
        for provider in KNOWN_PROVIDERS {
            for platform in PROVIDER_CLI_RELEASE_PLATFORMS {
                if !coverage.contains(&(provider.to_owned(), platform.to_owned())) {
                    return Err(invalid("coverage"));
                }
            }
        }
        Ok(Self {
            table_hash: sha256_hex(bytes.as_bytes()),
            recipes,
        })
    }

    pub fn embedded() -> Result<Self, ProtocolError> {
        Self::parse(PROVIDER_RECIPES_TABLE)
    }

    pub fn for_platform<'a>(
        &'a self,
        platform: &str,
    ) -> impl Iterator<Item = &'a ValidatedProviderRecipe> + 'a {
        let platform = platform.to_owned();
        self.recipes
            .iter()
            .filter(move |entry| entry.recipe.platforms.contains(&platform))
    }

    /// Exact triple match on one platform; a changed table never substitutes.
    pub fn find(
        &self,
        platform: &str,
        reference: &ProviderCliRecipeRef,
    ) -> Option<&ValidatedProviderRecipe> {
        self.for_platform(platform).find(|entry| {
            entry.recipe.recipe_id == reference.recipe_id
                && entry.recipe.recipe_version == reference.recipe_version
                && entry.recipe_hash == reference.recipe_hash
        })
    }
}

fn invalid(reason: &str) -> ProtocolError {
    ProtocolError::new(
        PROVIDER_CLI_TABLE_INVALID,
        format!("provider recipe table rejected: {reason}"),
    )
}

fn validate_recipe(
    recipe: &ProviderRecipe,
    ids: &mut BTreeSet<String>,
    coverage: &mut BTreeSet<(String, String)>,
) -> Result<(), ProtocolError> {
    let known = known_provider(&recipe.provider_id).ok_or_else(|| invalid("provider_id"))?;
    text(&recipe.recipe_id, 80)?;
    if !slug(&recipe.recipe_id, b'-') || !ids.insert(recipe.recipe_id.clone()) {
        return Err(invalid("recipe_id"));
    }
    if recipe.recipe_version < 1 || recipe.recipe_version > (1 << 53) {
        return Err(invalid("recipe_version"));
    }
    unique_non_empty(&recipe.platforms)?;
    unique_non_empty(&recipe.binary_names)?;
    if recipe
        .binary_names
        .iter()
        .any(|name| !known.names.contains(&name.as_str()))
    {
        return Err(invalid("binary_names"));
    }
    for platform in &recipe.platforms {
        if !PROVIDER_CLI_RELEASE_PLATFORMS.contains(&platform.as_str())
            || !coverage.insert((recipe.provider_id.clone(), platform.clone()))
        {
            return Err(invalid("platforms"));
        }
    }
    text(&recipe.official_docs, 1024)?;
    text(&recipe.login_hint, 1024)?;
    https_url(&recipe.official_docs)?;
    if !recipe.supported {
        let reason = recipe
            .unsupported_reason
            .as_deref()
            .ok_or_else(|| invalid("unsupported_reason"))?;
        text(reason, 80)?;
        if !slug(reason, b'_')
            || recipe.node_min.is_some()
            || recipe.install.is_some()
            || recipe.check.is_some()
            || recipe.lifecycle_review.is_some()
            || recipe.expected_version.is_some()
            || recipe.deadline_seconds.is_some()
            || recipe.binary_path.is_some()
            || recipe.native_binaries.is_some()
        {
            return Err(invalid("unsupported entry fields"));
        }
        return Ok(());
    }
    if recipe.unsupported_reason.is_some() {
        return Err(invalid("supported entry fields"));
    }
    if recipe.node_min != Some(PROVIDER_CLI_NODE_MIN)
        || recipe.platforms.iter().any(|p| p == "windows-x86_64")
    {
        return Err(invalid("supported platform"));
    }
    let install = recipe.install.as_ref().ok_or_else(|| invalid("install"))?;
    if install.kind != "npm-global"
        || known.package != Some(install.package.as_str())
        || !exact_semver(&install.version)
        || !PROVIDER_CLI_NPM_REGISTRIES.contains(&install.registry.as_str())
    {
        return Err(invalid("install"));
    }
    integrity(&install.integrity)?;
    let check = recipe.check.as_ref().ok_or_else(|| invalid("check"))?;
    if !recipe.binary_names.contains(&check.executable)
        || check.args.len() != 1
        || !PROVIDER_CLI_CHECK_ARGS.contains(&check.args[0].as_str())
    {
        return Err(invalid("check"));
    }
    // #131: the probe must print exactly this version; one canonical field.
    if recipe.expected_version.as_deref() != Some(install.version.as_str()) {
        return Err(invalid("expected_version"));
    }
    if let Some(deadline) = recipe.deadline_seconds {
        if !(PROVIDER_CLI_OPERATION_DEADLINE_MIN_SECONDS
            ..=PROVIDER_CLI_OPERATION_DEADLINE_MAX_SECONDS)
            .contains(&deadline)
        {
            return Err(invalid("deadline_seconds"));
        }
    }
    // Scripts run only to place a pinned native binary, and a pinned native
    // binary is placed only by reviewed scripts: the four fields come together.
    let native = recipe.native_binaries.is_some();
    if native != recipe.binary_path.is_some()
        || native == install.ignore_scripts
        || native != recipe.lifecycle_review.is_some()
        || (native && install.omit_optional != Some(false))
        || (!native && install.omit_optional.is_some())
    {
        return Err(invalid("native_binaries"));
    }
    if let Some(review) = &recipe.lifecycle_review {
        lifecycle_review(review, &install.package)?;
    }
    if let Some(pins) = &recipe.native_binaries {
        let path = recipe.binary_path.as_deref().unwrap_or_default();
        if path.len() > 128
            || !path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b))
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(invalid("binary_path"));
        }
        let platforms: BTreeSet<&str> = recipe.platforms.iter().map(String::as_str).collect();
        if pins.keys().map(String::as_str).collect::<BTreeSet<_>>() != platforms {
            return Err(invalid("native_binaries.platforms"));
        }
        for pin in pins.values() {
            native_binary_pin(pin, install)?;
        }
    }
    Ok(())
}

fn native_binary_pin(
    pin: &NativeBinaryPin,
    install: &NpmGlobalInstall,
) -> Result<(), ProtocolError> {
    text(&pin.package, 214)?;
    if !pin.package.starts_with(&format!("{}-", install.package))
        || pin.package[install.package.len() + 1..]
            .bytes()
            .any(|b| !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
        || pin.version != install.version
    {
        return Err(invalid("native_binaries.package"));
    }
    integrity(&pin.integrity)?;
    tarball_member(&pin.member).ok_or_else(|| invalid("native_binaries.member"))?;
    if !sha256_hex_ok(&pin.sha256) {
        return Err(invalid("native_binaries.sha256"));
    }
    if pin.size == 0
        || pin.size > pin.unpacked_size
        || pin.unpacked_size > PROVIDER_CLI_NATIVE_MAX_BYTES
    {
        return Err(invalid("native_binaries.size"));
    }
    Ok(())
}

fn sha256_hex_ok(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `package/<relative path>` with plain components; returns the relative part.
fn tarball_member(member: &str) -> Option<&str> {
    let relative = member.strip_prefix("package/")?;
    (!relative.is_empty()
        && relative
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b))
        && !relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".."))
    .then_some(relative)
}

fn lifecycle_review(review: &LifecycleReview, package: &str) -> Result<(), ProtocolError> {
    let date = review.reviewed_at.as_bytes();
    if date.len() != 10
        || !date.iter().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                *b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return Err(invalid("lifecycle_review.reviewed_at"));
    }
    text(&review.reviewer, 80)?;
    if review.files.is_empty() {
        return Err(invalid("lifecycle_review.files"));
    }
    let mut seen = BTreeSet::new();
    for file in &review.files {
        if tarball_member(&file.member).is_none() || !seen.insert(&file.member) {
            return Err(invalid("lifecycle_review.member"));
        }
        if !sha256_hex_ok(&file.sha256) {
            return Err(invalid("lifecycle_review.sha256"));
        }
    }
    // #131: the review names the scripts, which of them npm runs on a tarball
    // global install, and the closure the sealed acceptance asserts.
    let scripts = review
        .scripts
        .as_ref()
        .ok_or_else(|| invalid("lifecycle_review.scripts"))?;
    if scripts.is_empty() {
        return Err(invalid("lifecycle_review.scripts"));
    }
    for (name, command) in scripts {
        if name.len() > 32 || !slug(name, b'-') {
            return Err(invalid("lifecycle_review.scripts"));
        }
        text(command, 256)?;
    }
    let runs = review
        .runs
        .as_ref()
        .ok_or_else(|| invalid("lifecycle_review.runs"))?;
    if runs.is_empty() || runs.iter().any(|name| !scripts.contains_key(name)) {
        return Err(invalid("lifecycle_review.runs"));
    }
    let closure = review
        .closure
        .as_ref()
        .ok_or_else(|| invalid("lifecycle_review.closure"))?;
    if closure.packages != 2 || closure.with_scripts != [package.to_owned()] {
        return Err(invalid("lifecycle_review.closure"));
    }
    Ok(())
}

fn text(value: &str, max: usize) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.chars().count() > max
        || value
            .chars()
            .any(|c| c.is_control() || ('\u{7f}'..='\u{9f}').contains(&c))
    {
        return Err(invalid("text"));
    }
    Ok(())
}

fn slug(value: &str, separator: u8) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == separator)
}

fn unique_non_empty(values: &[String]) -> Result<(), ProtocolError> {
    let set: BTreeSet<&String> = values.iter().collect();
    if values.is_empty() || set.len() != values.len() {
        return Err(invalid("list"));
    }
    Ok(())
}

fn https_url(value: &str) -> Result<(), ProtocolError> {
    let rest = value
        .strip_prefix("https://")
        .ok_or_else(|| invalid("official_docs scheme"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty()
        || authority.contains('@')
        || authority.contains(char::is_whitespace)
        || value.contains(char::is_whitespace)
    {
        return Err(invalid("official_docs"));
    }
    Ok(())
}

fn exact_semver(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

fn integrity(value: &str) -> Result<(), ProtocolError> {
    let digest = value
        .strip_prefix("sha512-")
        .ok_or_else(|| invalid("integrity"))?;
    if digest.len() != 88
        || !digest.ends_with("==")
        || !digest[..86]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
    {
        return Err(invalid("integrity"));
    }
    let engine = base64::engine::general_purpose::STANDARD;
    let bytes = engine.decode(digest).map_err(|_| invalid("integrity"))?;
    if bytes.len() != 64 || engine.encode(&bytes) != digest {
        return Err(invalid("integrity"));
    }
    Ok(())
}

/// Hex SHA-256 of the integrity digest's raw bytes is not needed; the tarball
/// check compares base64 SHA-512. This decodes the expected digest bytes.
pub fn integrity_sha512_bytes(value: &str) -> Option<Vec<u8>> {
    let digest = value.strip_prefix("sha512-")?;
    base64::engine::general_purpose::STANDARD
        .decode(digest)
        .ok()
        .filter(|bytes| bytes.len() == 64)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Compact JSON with recursively sorted object keys and preserved array order.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            out.push('{');
            for (index, (key, item)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(item, out);
            }
            out.push('}');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn recipe_hash(raw_recipe: &Value) -> Result<String, ProtocolError> {
    let mut data = raw_recipe.clone();
    let object = data.as_object_mut().ok_or_else(|| invalid("recipe"))?;
    object.remove("recipe_hash");
    Ok(sha256_hex(canonical_json(&data).as_bytes()))
}

/// Exact recipe reference carried by prepare, check and the install token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCliRecipeRef {
    pub recipe_id: String,
    pub recipe_version: u64,
    pub recipe_hash: String,
}

/// One step of the execution plan. The displayed plan and the spawned plan are
/// the same value; nothing is composed by the browser or the Proxy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderCliPlanStep {
    Command {
        step: u32,
        purpose: String,
        executable: String,
        args: Vec<String>,
    },
    Verify {
        step: u32,
        purpose: String,
        algorithm: String,
        expected: String,
    },
    /// The only non-literal step: the probe runs `<prefix>/bin/<binary>`
    /// where `<prefix>` is step `prefix_from_step`'s single stdout line. The
    /// terminal operation report returns this step as a `command` with the
    /// literal path that ran. Never a PATH lookup.
    Resolve {
        step: u32,
        purpose: String,
        rule: String,
        binary: String,
        args: Vec<String>,
        prefix_from_step: u32,
    },
    /// Read-only (#131): `<prefix>/<relative_path>` must be a regular file of
    /// exactly `expected_size` bytes hashing to `expected_sha256`, reached by
    /// an O_NOFOLLOW chain, and `<prefix>/bin/<entry>` must be a symlink that
    /// resolves inside the prefix to that same file. Nothing is executed; a
    /// mismatch ends the operation as `provider_cli_binary_mismatch` before
    /// the probe.
    VerifyFile {
        step: u32,
        purpose: String,
        rule: String,
        relative_path: String,
        entry: String,
        expected_size: u64,
        expected_sha256: String,
        prefix_from_step: u32,
    },
}

impl ProviderCliPlanStep {
    pub fn step(&self) -> u32 {
        match self {
            Self::Command { step, .. }
            | Self::Verify { step, .. }
            | Self::Resolve { step, .. }
            | Self::VerifyFile { step, .. } => *step,
        }
    }
}

/// True when the first non-empty output line carries `expected` as a complete
/// token: `2.1.302` matches `claude 2.1.302 (Claude Code)` and `2.1.302`;
/// `2.1.3020`, `v2.1.302` and `02.1.302` do not. Tokens may be wrapped in
/// parentheses or followed by a comma.
pub fn provider_cli_version_token_matches(output: &str, expected: &str) -> bool {
    let Some(line) = output.lines().find(|line| !line.trim().is_empty()) else {
        return false;
    };
    !expected.is_empty()
        && line
            .split_whitespace()
            .any(|token| token.trim_start_matches('(').trim_end_matches([')', ',']) == expected)
}

/// SHA-256 over the compact canonical JSON of the complete plan (every step:
/// absolute executables, job-directory paths, all arguments, the integrity
/// digest and the resolution rule). The token binds it, the Controller
/// re-derives it before running, and the audit trail records it instead of
/// argv, output or paths.
pub fn plan_argv_hash(plan: &[ProviderCliPlanStep]) -> String {
    let value = serde_json::to_value(plan).unwrap_or(Value::Null);
    sha256_hex(canonical_json(&value).as_bytes())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliLifecycleScripts {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<LifecycleReview>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliPackage {
    pub name: String,
    pub version: String,
    pub integrity: String,
    pub registry: String,
}

/// Machine-scoped recipe view returned by recipes, prepare and operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliRecipeView {
    pub provider_id: String,
    pub recipe_id: String,
    pub recipe_version: u64,
    pub recipe_hash: String,
    pub platform: String,
    pub binary_names: Vec<String>,
    pub supported: bool,
    pub official_docs: String,
    pub login_hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_min: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_scripts: Option<ProviderCliLifecycleScripts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<ProviderCliPackage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Vec<ProviderCliPlanStep>>,
    /// #131: the version the probe must print as a complete token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_seconds: Option<u64>,
    /// Node `process.arch` token this platform's native entry requires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_arch: Option<String>,
    /// This platform's pinned native member for native-binary entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_binary: Option<NativeBinaryPin>,
}

/// Result of a `verify_file` step in the terminal report (#131): the measured
/// size and digest when the file was read, and which check failed otherwise
/// (`native_content`, `entry_link`, `not_regular`, `path_refused`). No path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliVerifiedFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliRecipesResponse {
    pub schema_version: u64,
    pub machine_id: String,
    pub platform: String,
    pub controller_source_sha: String,
    pub table_hash: String,
    pub recipes: Vec<ProviderCliRecipeView>,
}

/// Prepare result (#134): the observed installation state, the one action the
/// human may consent to and, unless the action is `noop`, the token bound to
/// exactly that observation, action and relation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliPrepareResponse {
    /// Absent for `noop`: nothing would change, so nothing can be confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub machine_id: String,
    pub table_hash: String,
    pub recipe: ProviderCliRecipeView,
    pub action: crate::ProviderCliAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_relation: Option<crate::ProviderCliVersionRelation>,
    /// The recipe's pinned package version, repeated for display.
    pub target_version: String,
    pub observation: crate::ProviderCliObservation,
    pub observation_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCliInstallRequest {
    pub confirmation_token: String,
    /// Required and equal to the prepared `version_relation` when that relation
    /// is `downgrade`, `incomparable` or `unknown`; forbidden otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_relation: Option<crate::ProviderCliVersionRelation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCliOperationStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Unknown,
}

impl ProviderCliOperationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "timed_out" => Self::TimedOut,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliInstallResponse {
    pub operation_id: String,
    pub machine_id: String,
    pub status: ProviderCliOperationStatus,
    pub poll_after_ms: u64,
}

/// Controller acknowledgement of a dispatched install: the record is durable
/// and the plan has been fixed before this is returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliInstallAccepted {
    pub operation_id: String,
    pub status: ProviderCliOperationStatus,
    pub argv_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliCheckResponse {
    pub installed: bool,
    /// False when the binary is absent from the installed global prefix; PATH
    /// is deliberately not consulted.
    pub found_in_prefix: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Absolute path of the probed binary inside the installed prefix, for a
    /// profile that wants the exact executable; Treer never edits PATH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    /// Evidence-based classification (#134); v1 Controllers omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<crate::ProviderCliObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliStepResult {
    pub step: u32,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub duration_ms: u64,
}

/// Operation record as the Controller reports it; the Proxy merges scope and
/// status and stores no output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliOperationReport {
    pub operation_id: String,
    pub recipe: ProviderCliRecipeRef,
    pub status: ProviderCliOperationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    pub elapsed_ms: u64,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<ProviderCliStepResult>>,
    /// Terminal only: the plan as it ran, with the probe's literal path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Vec<ProviderCliPlanStep>>,
    pub argv_hash: String,
    /// Consent the operation was dispatched with (#134; v1 records omit it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<crate::ProviderCliAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_relation: Option<crate::ProviderCliVersionRelation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_hash: Option<String>,
    /// The latest observation the Controller compared against the consent;
    /// on `provider_cli_state_changed` it is the one that differed. No path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_before: Option<crate::ProviderCliObservationSummary>,
    /// #131: outcome of the read-only file verification, when the plan has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_file: Option<ProviderCliVerifiedFile>,
    /// #131: true once the install step started and the operation did not
    /// succeed: the global prefix may have been modified; the user removes
    /// the package with the existing uninstall path, nothing retries.
    #[serde(default)]
    pub prefix_modified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliOperationResponse {
    pub operation_id: String,
    pub machine_id: String,
    pub recipe_id: String,
    pub recipe_version: u64,
    pub recipe_hash: String,
    pub status: ProviderCliOperationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    pub elapsed_ms: u64,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<ProviderCliStepResult>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Vec<ProviderCliPlanStep>>,
    pub poll_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<crate::ProviderCliAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_relation: Option<crate::ProviderCliVersionRelation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_hash: Option<String>,
    /// Observation the consent was given on (from the Proxy row, path-free).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_state: Option<crate::ProviderCliObservedState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_version: Option<String>,
    /// The Controller's latest comparison observation, when it reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_before: Option<crate::ProviderCliObservationSummary>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_cli_expected_node_arch;

    const GOLDEN: &str = include_str!("../../../launchers/agent/tests/provider-recipe-hashes.json");

    #[test]
    fn embedded_table_matches_launcher_golden_hashes() {
        let table = ValidatedProviderTable::embedded().expect("embedded table validates");
        let golden: Value = serde_json::from_str(GOLDEN).unwrap();
        assert_eq!(table.table_hash, golden["table_hash"].as_str().unwrap());
        let hashes = golden["recipe_hashes"].as_object().unwrap();
        assert_eq!(hashes.len(), table.recipes.len());
        for entry in &table.recipes {
            assert_eq!(
                entry.recipe_hash,
                hashes[&entry.recipe.recipe_id].as_str().unwrap(),
                "{}",
                entry.recipe.recipe_id
            );
        }
        let posix = table.for_platform("linux-x86_64").collect::<Vec<_>>();
        assert_eq!(posix.len(), KNOWN_PROVIDERS.len());
        assert!(table
            .for_platform("windows-x86_64")
            .all(|entry| !entry.recipe.supported));
        let codex = table
            .find(
                "darwin-aarch64",
                &ProviderCliRecipeRef {
                    recipe_id: "codex-npm-posix".into(),
                    recipe_version: 2,
                    recipe_hash: hashes["codex-npm-posix"].as_str().unwrap().into(),
                },
            )
            .unwrap();
        assert!(codex.recipe.install.as_ref().unwrap().ignore_scripts);
        assert!(table
            .find(
                "windows-x86_64",
                &ProviderCliRecipeRef {
                    recipe_id: "codex-npm-posix".into(),
                    recipe_version: 2,
                    recipe_hash: hashes["codex-npm-posix"].as_str().unwrap().into(),
                },
            )
            .is_none());
    }

    fn mutate(edit: impl FnOnce(&mut Value)) -> Result<ValidatedProviderTable, ProtocolError> {
        let mut table: Value = serde_json::from_str(PROVIDER_RECIPES_TABLE).unwrap();
        edit(&mut table);
        ValidatedProviderTable::parse(&table.to_string())
    }

    fn supported(table: &mut Value) -> &mut Value {
        table["recipes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|recipe| recipe["recipe_id"] == "codex-npm-posix")
            .unwrap()
    }

    #[test]
    fn canonical_hashing_matches_the_launcher_oracle_rules() {
        let value: Value =
            serde_json::from_str(r#"{"b":[2,{"z":1,"a":"x\"y"}],"a":true}"#).unwrap();
        assert_eq!(
            canonical_json(&value),
            r#"{"a":true,"b":[2,{"a":"x\"y","z":1}]}"#
        );
        let mut table: Value = serde_json::from_str(PROVIDER_RECIPES_TABLE).unwrap();
        let recipe = supported(&mut table);
        let before = recipe_hash(recipe).unwrap();
        recipe["recipe_hash"] = Value::String("ignored".into());
        assert_eq!(recipe_hash(recipe).unwrap(), before);
    }

    #[test]
    fn table_validation_rejects_every_unsafe_shape() {
        type Edit = Box<dyn FnOnce(&mut Value)>;
        let cases: Vec<(&str, Edit)> = vec![
            (
                "range version",
                Box::new(|t| supported(t)["install"]["version"] = "^0.163.0".into()),
            ),
            (
                "tag version",
                Box::new(|t| supported(t)["install"]["version"] = "latest".into()),
            ),
            (
                "missing integrity",
                Box::new(|t| {
                    supported(t)["install"]
                        .as_object_mut()
                        .unwrap()
                        .remove("integrity");
                }),
            ),
            (
                "bad integrity",
                Box::new(|t| supported(t)["install"]["integrity"] = "sha256-abc".into()),
            ),
            (
                "foreign registry",
                Box::new(|t| {
                    supported(t)["install"]["registry"] = "https://registry.example.test/".into()
                }),
            ),
            (
                "foreign package",
                Box::new(|t| supported(t)["install"]["package"] = "@openai/codex-evil".into()),
            ),
            (
                "extra install flag",
                Box::new(|t| supported(t)["install"]["userconfig"] = "/tmp/x".into()),
            ),
            (
                "extra recipe field",
                Box::new(|t| supported(t)["args"] = Value::Array(vec!["--prefix".into()])),
            ),
            (
                "check outside binary_names",
                Box::new(|t| supported(t)["check"]["executable"] = "npm".into()),
            ),
            (
                "check args outside allowlist",
                Box::new(|t| {
                    supported(t)["check"]["args"] = Value::Array(vec!["--registry=x".into()])
                }),
            ),
            (
                "check args count",
                Box::new(|t| {
                    supported(t)["check"]["args"] =
                        Value::Array(vec!["--version".into(), "x".into()])
                }),
            ),
            (
                "lifecycle without review",
                Box::new(|t| supported(t)["install"]["ignore_scripts"] = false.into()),
            ),
            (
                "review on ignore_scripts entry",
                Box::new(
                    |t| supported(t)["lifecycle_review"] = serde_json::json!({"reviewed_at":"2026-10-16","reviewer":"x","files":[{"member":"package/a.js","sha256":"0".repeat(64)}]}),
                ),
            ),
            (
                "windows supported",
                Box::new(|t| {
                    let r = supported(t);
                    r["platforms"]
                        .as_array_mut()
                        .unwrap()
                        .push("windows-x86_64".into());
                    let windows = t["recipes"]
                        .as_array_mut()
                        .unwrap()
                        .iter()
                        .position(|r| r["recipe_id"] == "codex-windows")
                        .unwrap();
                    t["recipes"].as_array_mut().unwrap().remove(windows);
                }),
            ),
            (
                "node_min",
                Box::new(|t| supported(t)["node_min"] = 20.into()),
            ),
            (
                "unknown provider",
                Box::new(|t| supported(t)["provider_id"] = "evil".into()),
            ),
            (
                "unknown platform",
                Box::new(|t| {
                    supported(t)["platforms"] = Value::Array(vec!["freebsd-x86_64".into()])
                }),
            ),
            (
                "duplicate coverage",
                Box::new(|t| {
                    let r = supported(t);
                    r["recipe_id"] = "codex-dup".into();
                    r["platforms"] = Value::Array(vec!["windows-x86_64".into()]);
                    r["supported"] = false.into();
                    r["unsupported_reason"] = "x_y".into();
                    for key in ["node_min", "install", "check"] {
                        r.as_object_mut().unwrap().remove(key);
                    }
                }),
            ),
            (
                "missing coverage",
                Box::new(|t| {
                    let a = t["recipes"].as_array_mut().unwrap();
                    a.remove(0);
                }),
            ),
            (
                "schema",
                Box::new(|t| t["schema"] = "treer.provider-recipes/v2".into()),
            ),
            (
                "unsupported with install",
                Box::new(|t| {
                    let r = supported(t);
                    r["supported"] = false.into();
                    r["unsupported_reason"] = "pending".into();
                }),
            ),
            (
                "http docs",
                Box::new(|t| supported(t)["official_docs"] = "http://example.test/".into()),
            ),
            (
                "userinfo docs",
                Box::new(|t| supported(t)["official_docs"] = "https://user@example.test/".into()),
            ),
            (
                "control char",
                Box::new(|t| supported(t)["login_hint"] = "a\u{7}b".into()),
            ),
            ("table extra field", Box::new(|t| t["source"] = "x".into())),
        ];
        for (name, edit) in cases {
            let error = mutate(edit)
                .err()
                .unwrap_or_else(|| panic!("{name} accepted"));
            assert_eq!(error.code, PROVIDER_CLI_TABLE_INVALID, "{name}");
        }
        // Scripts without a pinned native, or a pinned native without
        // scripts, are refused in both directions.
        for edit in [
            |r: &mut Value| r["install"]["ignore_scripts"] = false.into(),
            |r: &mut Value| r["install"]["omit_optional"] = false.into(),
        ] {
            let error = mutate(|t| edit(supported(t))).err().unwrap();
            assert_eq!(error.code, PROVIDER_CLI_TABLE_INVALID);
        }
    }

    fn claude(table: &mut Value) -> &mut Value {
        table["recipes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|recipe| recipe["recipe_id"] == "claude-native-npm")
            .unwrap()
    }

    #[test]
    fn native_binary_entries_pin_every_platform_script_and_version() {
        let table = ValidatedProviderTable::embedded().expect("embedded table validates");
        let entry = table
            .recipes
            .iter()
            .find(|entry| entry.recipe.recipe_id == "claude-native-npm")
            .unwrap();
        let recipe = &entry.recipe;
        assert!(recipe.supported);
        assert_eq!(recipe.expected_version.as_deref(), Some("2.1.302"));
        assert_eq!(recipe.binary_path.as_deref(), Some("bin/claude.exe"));
        let install = recipe.install.as_ref().unwrap();
        assert!(!install.ignore_scripts);
        assert_eq!(install.omit_optional, Some(false));
        let pins = recipe.native_binaries.as_ref().unwrap();
        assert_eq!(pins.len(), 3);
        for platform in &recipe.platforms {
            let pin = &pins[platform];
            assert_eq!(pin.version, install.version);
            assert!(pin.package.starts_with("@anthropic-ai/claude-code-"));
            assert!(pin.size <= pin.unpacked_size);
        }
        let review = recipe.lifecycle_review.as_ref().unwrap();
        assert_eq!(
            review.runs.as_deref(),
            Some(&["postinstall".to_owned()][..])
        );
        assert_eq!(
            review.closure.as_ref().unwrap().with_scripts,
            vec!["@anthropic-ai/claude-code".to_owned()]
        );
        // Every other supported entry keeps scripts off and names its version.
        for other in table.recipes.iter().filter(|e| e.recipe.supported) {
            let install = other.recipe.install.as_ref().unwrap();
            assert_eq!(
                other.recipe.expected_version.as_deref(),
                Some(install.version.as_str())
            );
            if other.recipe.native_binaries.is_none() {
                assert!(install.ignore_scripts);
                assert!(install.omit_optional.is_none());
            }
        }

        type Edit = Box<dyn FnOnce(&mut Value)>;
        let cases: Vec<(&str, Edit)> = vec![
            (
                "missing expected_version",
                Box::new(|t| {
                    claude(t)
                        .as_object_mut()
                        .unwrap()
                        .remove("expected_version");
                }),
            ),
            (
                "expected_version differs from install",
                Box::new(|t| claude(t)["expected_version"] = "2.1.303".into()),
            ),
            (
                "deadline below floor",
                Box::new(|t| claude(t)["deadline_seconds"] = 30.into()),
            ),
            (
                "deadline above cap",
                Box::new(|t| claude(t)["deadline_seconds"] = 901.into()),
            ),
            (
                "scripts on without native pins",
                Box::new(|t| {
                    let r = claude(t);
                    for key in ["native_binaries", "binary_path"] {
                        r.as_object_mut().unwrap().remove(key);
                    }
                }),
            ),
            (
                "native pins with scripts off",
                Box::new(|t| claude(t)["install"]["ignore_scripts"] = true.into()),
            ),
            (
                "omit_optional true",
                Box::new(|t| claude(t)["install"]["omit_optional"] = true.into()),
            ),
            (
                "omit_optional missing",
                Box::new(|t| {
                    claude(t)["install"]
                        .as_object_mut()
                        .unwrap()
                        .remove("omit_optional");
                }),
            ),
            (
                "binary_path escapes",
                Box::new(|t| claude(t)["binary_path"] = "../bin/claude.exe".into()),
            ),
            (
                "binary_path absolute",
                Box::new(|t| claude(t)["binary_path"] = "/bin/claude.exe".into()),
            ),
            (
                "platform without pin",
                Box::new(|t| {
                    claude(t)["native_binaries"]
                        .as_object_mut()
                        .unwrap()
                        .remove("linux-x86_64");
                }),
            ),
            (
                "pin for unsupported platform",
                Box::new(|t| {
                    let pin = claude(t)["native_binaries"]["linux-x86_64"].clone();
                    claude(t)["native_binaries"]["darwin-x86_64"] = pin;
                }),
            ),
            (
                "native version differs",
                Box::new(|t| {
                    claude(t)["native_binaries"]["linux-x86_64"]["version"] = "2.1.301".into()
                }),
            ),
            (
                "native package outside the root family",
                Box::new(|t| {
                    claude(t)["native_binaries"]["linux-x86_64"]["package"] =
                        "@evil/claude-code-linux-x64".into()
                }),
            ),
            (
                "native integrity not sha512",
                Box::new(|t| {
                    claude(t)["native_binaries"]["linux-x86_64"]["integrity"] = "sha256-abc".into()
                }),
            ),
            (
                "native sha256 uppercase",
                Box::new(|t| {
                    claude(t)["native_binaries"]["linux-x86_64"]["sha256"] = "A".repeat(64).into()
                }),
            ),
            (
                "native size zero",
                Box::new(|t| claude(t)["native_binaries"]["linux-x86_64"]["size"] = 0.into()),
            ),
            (
                "native size above unpacked",
                Box::new(|t| {
                    claude(t)["native_binaries"]["linux-x86_64"]["unpacked_size"] = 1.into()
                }),
            ),
            (
                "native unpacked above cap",
                Box::new(|t| {
                    claude(t)["native_binaries"]["linux-x86_64"]["unpacked_size"] =
                        (1_u64 << 31).into()
                }),
            ),
            (
                "native member escapes",
                Box::new(|t| {
                    claude(t)["native_binaries"]["linux-x86_64"]["member"] =
                        "package/../claude".into()
                }),
            ),
            (
                "review without scripts",
                Box::new(|t| {
                    claude(t)["lifecycle_review"]
                        .as_object_mut()
                        .unwrap()
                        .remove("scripts");
                }),
            ),
            (
                "runs names an unknown script",
                Box::new(|t| {
                    claude(t)["lifecycle_review"]["runs"] = Value::Array(vec!["preinstall".into()])
                }),
            ),
            (
                "closure with another scripted package",
                Box::new(|t| {
                    claude(t)["lifecycle_review"]["closure"]["with_scripts"] =
                        Value::Array(vec!["@anthropic-ai/claude-code-linux-x64".into()])
                }),
            ),
            (
                "closure larger than root plus one native",
                Box::new(|t| claude(t)["lifecycle_review"]["closure"]["packages"] = 3.into()),
            ),
            (
                "unsupported entry carrying expected_version",
                Box::new(|t| {
                    let r = t["recipes"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .find(|recipe| recipe["recipe_id"] == "claude-native-unavailable")
                        .unwrap();
                    r["expected_version"] = "2.1.302".into();
                }),
            ),
            (
                "schema_version 1",
                Box::new(|t| t["schema_version"] = 1.into()),
            ),
        ];
        for (name, edit) in cases {
            let error = mutate(edit)
                .err()
                .unwrap_or_else(|| panic!("{name} accepted"));
            assert_eq!(error.code, PROVIDER_CLI_TABLE_INVALID, "{name}");
        }
        // A bounded deadline is accepted and carried.
        let bounded = mutate(|t| claude(t)["deadline_seconds"] = 900.into()).unwrap();
        assert_eq!(
            bounded
                .recipes
                .iter()
                .find(|e| e.recipe.recipe_id == "claude-native-npm")
                .unwrap()
                .recipe
                .deadline_seconds,
            Some(900)
        );
    }

    #[test]
    fn version_token_match_is_exact_and_whitespace_bounded() {
        for (output, expected, matches) in [
            ("2.1.302\n", "2.1.302", true),
            ("claude 2.1.302 (Claude Code)\n", "2.1.302", true),
            ("\n  Claude Code (2.1.302)\n", "2.1.302", true),
            ("version 2.1.302, build 7\n", "2.1.302", true),
            ("2.1.3020\n", "2.1.302", false),
            ("v2.1.302\n", "2.1.302", false),
            ("02.1.302\n", "2.1.302", false),
            ("2.1.30\n", "2.1.302", false),
            ("codex-cli 0.163.0\n", "2.1.302", false),
            ("\n\n", "2.1.302", false),
            ("other\n2.1.302\n", "2.1.302", false),
            ("2.1.302", "", false),
        ] {
            assert_eq!(
                provider_cli_version_token_matches(output, expected),
                matches,
                "{output:?}"
            );
        }
        assert_eq!(
            provider_cli_expected_node_arch("darwin-aarch64"),
            Some("arm64")
        );
        assert_eq!(provider_cli_expected_node_arch("linux-x86_64"), Some("x64"));
        assert_eq!(provider_cli_expected_node_arch("windows-x86_64"), None);
    }

    #[test]
    fn plan_argv_hash_covers_the_whole_plan() {
        let plan = vec![
            ProviderCliPlanStep::Command {
                step: 1,
                purpose: "pack".into(),
                executable: "/usr/bin/npm".into(),
                args: vec!["pack".into()],
            },
            ProviderCliPlanStep::Verify {
                step: 2,
                purpose: "integrity".into(),
                algorithm: "sha512".into(),
                expected: "sha512-x".into(),
            },
        ];
        let mut other = plan.clone();
        if let ProviderCliPlanStep::Verify { expected, .. } = &mut other[1] {
            *expected = "sha512-y".into();
        }
        assert_ne!(
            plan_argv_hash(&plan),
            plan_argv_hash(&other),
            "the integrity digest is part of the confirmed plan"
        );
        let mut other = plan.clone();
        if let ProviderCliPlanStep::Command { args, .. } = &mut other[0] {
            args.push("--ignore-scripts".into());
        }
        assert_ne!(plan_argv_hash(&plan), plan_argv_hash(&other));
        assert_eq!(plan_argv_hash(&plan), plan_argv_hash(&plan.clone()));
        assert_eq!(
            ProviderCliOperationStatus::parse("timed_out"),
            Some(ProviderCliOperationStatus::TimedOut)
        );
        assert!(ProviderCliOperationStatus::Unknown.is_terminal());
        assert!(!ProviderCliOperationStatus::Running.is_terminal());
    }
}
