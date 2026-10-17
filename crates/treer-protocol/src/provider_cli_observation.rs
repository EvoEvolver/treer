//! Observed installation state and explicit consent action for provider CLI
//! installation (Issue #134, follow-up of #123).
//!
//! `prepare` no longer only shows the plan: the Controller first observes what
//! the recipe's install step would overwrite in the npm global prefix and
//! classifies it from evidence (`absent`, `installed`, `unknown`), never from
//! an error. The Proxy turns that observation into one explicit action
//! (`install`, `replace`, `noop`), binds the observation hash, the action and
//! the version relation into the single-use confirmation token, and the
//! Controller re-observes before any mutation and again immediately before the
//! install step; any difference refuses the operation as
//! `provider_cli_state_changed`. A same-version installation is `noop` and
//! never receives a token.
use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::provider_cli::canonical_json;

/// Advertised next to the v1 capability by Controllers that observe before
/// installing; the Proxy requires it for `prepare` and `install`.
pub const CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL_V2: &str = "machine.provider-cli-install.v2";
pub const PROVIDER_CLI_OBSERVATION_SCHEMA: &str = "treer.provider-cli-observation/v1";
/// Display-only probe text kept after control characters are stripped.
pub const PROVIDER_CLI_VERSION_TEXT_MAX: usize = 256;
/// A version token longer than this is not a version.
pub const PROVIDER_CLI_VERSION_TOKEN_MAX: usize = 64;
/// Numeric components are bounded before parsing so no output can overflow.
pub const PROVIDER_CLI_VERSION_SEGMENT_DIGITS: usize = 10;
pub const PROVIDER_CLI_VERSION_PRERELEASE_IDENTIFIERS: usize = 8;

/// What the Controller found in the global prefix, decided from evidence only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCliObservedState {
    /// The prefix resolved and `<prefix>/bin/<binary>` does not exist.
    Absent,
    /// The binary exists inside the prefix, the probe exited 0 with confirmed
    /// cleanup, and its first output line carries a strict version.
    Installed,
    /// Anything else; `found_in_prefix` says whether something is there.
    Unknown,
}

impl ProviderCliObservedState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Installed => "installed",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "absent" => Self::Absent,
            "installed" => Self::Installed,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// The action the human consents to; decided by Core, displayed by the Web.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCliAction {
    Install,
    Replace,
    /// Already at the pinned version: no token is issued and nothing runs.
    Noop,
}

impl ProviderCliAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Replace => "replace",
            Self::Noop => "noop",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "install" => Self::Install,
            "replace" => Self::Replace,
            "noop" => Self::Noop,
            _ => return None,
        })
    }
}

/// How the installed version relates to the pinned target for `replace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCliVersionRelation {
    /// Installed version is older than the target.
    Upgrade,
    /// Installed version is newer than the target: installing downgrades it.
    Downgrade,
    /// Both versions parsed but the strict comparator refuses to order them
    /// (equal precedence with different build metadata).
    Incomparable,
    /// Something exists in the prefix whose version could not be read.
    Unknown,
}

impl ProviderCliVersionRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upgrade => "upgrade",
            Self::Downgrade => "downgrade",
            Self::Incomparable => "incomparable",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "upgrade" => Self::Upgrade,
            "downgrade" => Self::Downgrade,
            "incomparable" => Self::Incomparable,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    /// Relations the human must acknowledge explicitly in the install request.
    pub fn requires_acknowledgement(self) -> bool {
        matches!(self, Self::Downgrade | Self::Incomparable | Self::Unknown)
    }
}

/// Probe outcome without output; the output stays in the check response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliProbe {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub cleanup_confirmed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// One observation of the global prefix for one recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliObservation {
    pub state: ProviderCliObservedState,
    /// Normalized strict version (`1.2.3`, `1.2.3-rc.1`), only for `installed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version: Option<String>,
    /// Cleaned first output line, display only, never compared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_text: Option<String>,
    /// Absolute `<prefix>/bin/<binary>` once the prefix resolved. Shown to the
    /// creator only; the Proxy persists and audits the summary without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    pub found_in_prefix: bool,
    pub probe: ProviderCliProbe,
}

/// Every probe error code a Controller may report; anything else is a
/// malformed response.
pub const PROVIDER_CLI_PROBE_ERROR_CODES: [&str; 9] = [
    "provider_cli_prefix_unresolved",
    "provider_cli_stat_failed",
    "provider_cli_binary_not_found",
    "provider_cli_binary_outside_prefix",
    "provider_cli_probe_failed",
    "provider_cli_probe_timed_out",
    "provider_cli_probe_output_truncated",
    "provider_cli_version_unparsed",
    "cleanup_unconfirmed",
];
/// Longest executable path a Controller may report.
pub const PROVIDER_CLI_EXECUTABLE_MAX: usize = 4096;

impl ProviderCliObservation {
    /// Shape and consistency rules the Proxy enforces before an observation
    /// is stored, audited or shown: bounded, control-free strings, a known
    /// probe code, a canonical strict version, and state/evidence agreement.
    /// A Controller response that fails is treated as invalid (fail closed).
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(version) = &self.current_version {
            match parse_strict_version(version) {
                Some(parsed) if parsed.normalized() == *version => {}
                _ => return Err("current_version is not a canonical strict version"),
            }
        }
        if let Some(text) = &self.version_text {
            if text.is_empty()
                || text.len() > PROVIDER_CLI_VERSION_TEXT_MAX
                || text.chars().any(char::is_control)
            {
                return Err("version_text is empty, too long or has control characters");
            }
        }
        if let Some(executable) = &self.executable {
            if executable.len() > PROVIDER_CLI_EXECUTABLE_MAX
                || !(executable.starts_with('/')
                    || executable
                        .as_bytes()
                        .get(1..3)
                        .is_some_and(|rest| rest == b":\\"))
                || executable.chars().any(char::is_control)
            {
                return Err("executable is not a bounded absolute path");
            }
        }
        if let Some(code) = &self.probe.error_code {
            if !PROVIDER_CLI_PROBE_ERROR_CODES.contains(&code.as_str()) {
                return Err("probe.error_code is unknown");
            }
        }
        // Process cleanup is a fact of its own: unconfirmed cleanup is always
        // reported as such, and only then.
        if self.probe.cleanup_confirmed
            == (self.probe.error_code.as_deref() == Some("cleanup_unconfirmed"))
        {
            return Err("probe.cleanup_confirmed disagrees with probe.error_code");
        }
        match self.state {
            ProviderCliObservedState::Absent => {
                if self.current_version.is_some()
                    || self.found_in_prefix
                    || self.executable.is_none()
                    || self.probe.error_code.as_deref() != Some("provider_cli_binary_not_found")
                {
                    return Err("absent requires a resolved prefix, no binary and no version");
                }
            }
            ProviderCliObservedState::Installed => {
                if self.current_version.is_none()
                    || !self.found_in_prefix
                    || self.executable.is_none()
                    || self.probe.exit_code != Some(0)
                    || self.probe.timed_out
                    || !self.probe.cleanup_confirmed
                    || self.probe.error_code.is_some()
                {
                    return Err("installed requires a found binary, a clean probe and a version");
                }
            }
            ProviderCliObservedState::Unknown => {
                if self.current_version.is_some() || self.probe.error_code.is_none() {
                    return Err("unknown carries no version and names its reason");
                }
                if !self.found_in_prefix
                    && self.executable.is_some()
                    && self.probe.error_code.as_deref() != Some("provider_cli_stat_failed")
                {
                    return Err(
                        "unknown with a resolved prefix but nothing found must be a stat failure",
                    );
                }
            }
        }
        Ok(())
    }

    /// Nothing can be said about the prefix (unresolved, unreadable): no
    /// action can be offered and the plan's own prefix step would fail too.
    pub fn unobservable(&self) -> bool {
        self.state == ProviderCliObservedState::Unknown && !self.found_in_prefix
    }

    /// Hash over the fields that decide the action, bound to the recipe and
    /// its pinned target. Probe output and exit codes are deliberately
    /// excluded so a noisy probe does not change the consent.
    pub fn hash(&self, recipe_hash: &str, target_version: &str) -> String {
        let value: Value = json!({
            "schema": PROVIDER_CLI_OBSERVATION_SCHEMA,
            "recipe_hash": recipe_hash,
            "target_version": target_version,
            "state": self.state.as_str(),
            "current_version": self.current_version,
            "executable": self.executable,
            "found_in_prefix": self.found_in_prefix,
        });
        format!("{:x}", Sha256::digest(canonical_json(&value).as_bytes()))
    }

    pub fn summary(
        &self,
        recipe_hash: &str,
        target_version: &str,
    ) -> ProviderCliObservationSummary {
        ProviderCliObservationSummary {
            state: self.state,
            current_version: self.current_version.clone(),
            found_in_prefix: self.found_in_prefix,
            observation_hash: self.hash(recipe_hash, target_version),
        }
    }
}

/// Path-free observation for operation reports, Proxy rows and audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliObservationSummary {
    pub state: ProviderCliObservedState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version: Option<String>,
    pub found_in_prefix: bool,
    pub observation_hash: String,
}

/// The consent the token carries and the Controller re-derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCliDecision {
    pub action: ProviderCliAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_relation: Option<ProviderCliVersionRelation>,
}

/// What the token binds and the install command carries: the decision plus
/// the hash of the observation it was made on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCliConsent {
    pub action: ProviderCliAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_relation: Option<ProviderCliVersionRelation>,
    pub observation_hash: String,
}

impl ProviderCliConsent {
    pub fn decision(&self) -> ProviderCliDecision {
        ProviderCliDecision {
            action: self.action,
            version_relation: self.version_relation,
        }
    }

    /// True when a fresh observation still supports exactly this consent.
    pub fn matches(
        &self,
        observation: &ProviderCliObservation,
        recipe_hash: &str,
        target_version: &str,
    ) -> bool {
        decide_provider_cli_action(observation, target_version) == Some(self.decision())
            && observation.hash(recipe_hash, target_version) == self.observation_hash
    }
}

/// Maps an observation to the action it allows. `None` when the prefix is
/// unobservable or the observation's own processes were not confirmed gone:
/// an observation that may still be running is not evidence for consent.
pub fn decide_provider_cli_action(
    observation: &ProviderCliObservation,
    target_version: &str,
) -> Option<ProviderCliDecision> {
    if !observation.probe.cleanup_confirmed {
        return None;
    }
    match observation.state {
        ProviderCliObservedState::Absent => Some(ProviderCliDecision {
            action: ProviderCliAction::Install,
            version_relation: None,
        }),
        ProviderCliObservedState::Installed => {
            let current = observation.current_version.as_deref()?;
            let relation = match compare_provider_cli_versions(current, target_version) {
                VersionOrder::Equal => {
                    return Some(ProviderCliDecision {
                        action: ProviderCliAction::Noop,
                        version_relation: None,
                    })
                }
                VersionOrder::Less => ProviderCliVersionRelation::Upgrade,
                VersionOrder::Greater => ProviderCliVersionRelation::Downgrade,
                VersionOrder::Incomparable => ProviderCliVersionRelation::Incomparable,
            };
            Some(ProviderCliDecision {
                action: ProviderCliAction::Replace,
                version_relation: Some(relation),
            })
        }
        ProviderCliObservedState::Unknown if observation.found_in_prefix => {
            Some(ProviderCliDecision {
                action: ProviderCliAction::Replace,
                version_relation: Some(ProviderCliVersionRelation::Unknown),
            })
        }
        ProviderCliObservedState::Unknown => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionOrder {
    Less,
    Equal,
    Greater,
    /// Equal precedence but different build metadata, or an unparsable side.
    Incomparable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Identifier {
    Numeric(u64),
    Alphanumeric(String),
}

/// Strict semantic version: bounded numeric components without leading zeros,
/// bounded prerelease identifiers, build metadata kept only for equality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictVersion {
    core: [u64; 3],
    prerelease: Vec<Identifier>,
    build: Option<String>,
}

fn numeric_component(text: &str) -> Option<u64> {
    if text.is_empty()
        || text.len() > PROVIDER_CLI_VERSION_SEGMENT_DIGITS
        || !text.bytes().all(|b| b.is_ascii_digit())
        || (text.len() > 1 && text.starts_with('0'))
    {
        return None;
    }
    text.parse().ok()
}

fn identifier_chars_ok(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn prerelease_identifier(text: &str) -> Option<Identifier> {
    if !identifier_chars_ok(text) {
        return None;
    }
    if text.bytes().all(|b| b.is_ascii_digit()) {
        return numeric_component(text).map(Identifier::Numeric);
    }
    Some(Identifier::Alphanumeric(text.to_owned()))
}

/// Parses one token: optional leading `v`, `MAJOR.MINOR.PATCH`, optional
/// `-prerelease`, optional `+build`. Anything else is not a version.
pub fn parse_strict_version(token: &str) -> Option<StrictVersion> {
    if token.len() > PROVIDER_CLI_VERSION_TOKEN_MAX || !token.is_ascii() {
        return None;
    }
    let token = token.strip_prefix('v').unwrap_or(token);
    let (rest, build) = match token.split_once('+') {
        Some((rest, build)) => {
            if !build.split('.').all(identifier_chars_ok) {
                return None;
            }
            (rest, Some(build.to_owned()))
        }
        None => (token, None),
    };
    let (core, prerelease) = match rest.split_once('-') {
        Some((core, prerelease)) => {
            let identifiers = prerelease
                .split('.')
                .map(prerelease_identifier)
                .collect::<Option<Vec<_>>>()?;
            if identifiers.len() > PROVIDER_CLI_VERSION_PRERELEASE_IDENTIFIERS {
                return None;
            }
            (core, identifiers)
        }
        None => (rest, Vec::new()),
    };
    let mut parts = core.split('.');
    let major = numeric_component(parts.next()?)?;
    let minor = numeric_component(parts.next()?)?;
    let patch = numeric_component(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some(StrictVersion {
        core: [major, minor, patch],
        prerelease,
        build,
    })
}

impl StrictVersion {
    /// Canonical text without the leading `v`.
    pub fn normalized(&self) -> String {
        let mut text = format!("{}.{}.{}", self.core[0], self.core[1], self.core[2]);
        if !self.prerelease.is_empty() {
            text.push('-');
            let identifiers: Vec<String> = self
                .prerelease
                .iter()
                .map(|identifier| match identifier {
                    Identifier::Numeric(value) => value.to_string(),
                    Identifier::Alphanumeric(value) => value.clone(),
                })
                .collect();
            text.push_str(&identifiers.join("."));
        }
        if let Some(build) = &self.build {
            text.push('+');
            text.push_str(build);
        }
        text
    }

    /// Semantic-version precedence; build metadata only decides equality.
    pub fn compare(&self, other: &Self) -> VersionOrder {
        match self.core.cmp(&other.core) {
            Ordering::Less => return VersionOrder::Less,
            Ordering::Greater => return VersionOrder::Greater,
            Ordering::Equal => {}
        }
        let precedence = match (self.prerelease.is_empty(), other.prerelease.is_empty()) {
            (true, true) => Ordering::Equal,
            // A release outranks any prerelease of the same core.
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => compare_prerelease(&self.prerelease, &other.prerelease),
        };
        match precedence {
            Ordering::Less => VersionOrder::Less,
            Ordering::Greater => VersionOrder::Greater,
            Ordering::Equal if self.build == other.build => VersionOrder::Equal,
            Ordering::Equal => VersionOrder::Incomparable,
        }
    }
}

fn compare_prerelease(left: &[Identifier], right: &[Identifier]) -> Ordering {
    for (a, b) in left.iter().zip(right.iter()) {
        let order = match (a, b) {
            (Identifier::Numeric(x), Identifier::Numeric(y)) => x.cmp(y),
            // Numeric identifiers always have lower precedence.
            (Identifier::Numeric(_), Identifier::Alphanumeric(_)) => Ordering::Less,
            (Identifier::Alphanumeric(_), Identifier::Numeric(_)) => Ordering::Greater,
            (Identifier::Alphanumeric(x), Identifier::Alphanumeric(y)) => {
                x.as_bytes().cmp(y.as_bytes())
            }
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    // Equal prefix: the longer identifier list wins.
    left.len().cmp(&right.len())
}

pub fn compare_provider_cli_versions(current: &str, target: &str) -> VersionOrder {
    match (parse_strict_version(current), parse_strict_version(target)) {
        (Some(current), Some(target)) => current.compare(&target),
        _ => VersionOrder::Incomparable,
    }
}

/// Display text: the first non-empty line with control characters removed,
/// bounded. Never used for the decision.
pub fn provider_cli_version_text(stdout: &str) -> Option<String> {
    let line = stdout.lines().find(|line| !line.trim().is_empty())?;
    let cleaned: String = line
        .chars()
        .filter(|c| !c.is_control())
        .take(PROVIDER_CLI_VERSION_TEXT_MAX)
        .collect();
    let cleaned = cleaned.trim().to_owned();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// The version a probe reported: the first whitespace-separated token of the
/// first non-empty line that parses as a strict version, normalized. Tokens
/// that merely contain a version (`codex-cli/1.2.3`) are not accepted.
pub fn provider_cli_version_from_output(stdout: &str) -> Option<String> {
    let line = stdout.lines().find(|line| !line.trim().is_empty())?;
    line.split_whitespace()
        .take(16)
        .find_map(|token| parse_strict_version(token).map(|version| version.normalized()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(
        state: ProviderCliObservedState,
        version: Option<&str>,
        found: bool,
    ) -> ProviderCliObservation {
        ProviderCliObservation {
            state,
            current_version: version.map(str::to_owned),
            version_text: version.map(|v| format!("codex-cli {v}")),
            executable: found.then(|| "/opt/prefix/bin/codex".to_owned()),
            found_in_prefix: found,
            probe: ProviderCliProbe {
                exit_code: Some(0),
                timed_out: false,
                cleanup_confirmed: true,
                error_code: None,
            },
        }
    }

    #[test]
    fn strict_versions_parse_normalize_and_order_per_semver() {
        let cases = [
            ("1.2.3", Some("1.2.3")),
            ("v1.2.3", Some("1.2.3")),
            ("0.163.0", Some("0.163.0")),
            ("1.0.0-rc.1", Some("1.0.0-rc.1")),
            (
                "1.0.0-alpha.beta.1+build.7",
                Some("1.0.0-alpha.beta.1+build.7"),
            ),
            ("1.2", None),
            ("1.2.3.4", None),
            ("01.2.3", None),
            ("1.02.3", None),
            ("1.2.3-", None),
            ("1.2.3-rc..1", None),
            ("1.2.3-rc.01", None),
            ("1.2.3+", None),
            ("1.2.3+build!", None),
            ("12345678901.0.0", None),
            ("1.2.3-a.b.c.d.e.f.g.h.i", None),
            ("vv1.2.3", None),
            ("1.2.3\u{0301}", None),
            ("", None),
        ];
        for (input, expected) in cases {
            assert_eq!(
                parse_strict_version(input).map(|v| v.normalized()),
                expected.map(str::to_owned),
                "{input:?}"
            );
        }
        let long = format!("1.2.3-{}", "a".repeat(PROVIDER_CLI_VERSION_TOKEN_MAX));
        assert!(parse_strict_version(&long).is_none());

        let ordered = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
        ];
        for pair in ordered.windows(2) {
            assert_eq!(
                compare_provider_cli_versions(pair[0], pair[1]),
                VersionOrder::Less,
                "{} < {}",
                pair[0],
                pair[1]
            );
            assert_eq!(
                compare_provider_cli_versions(pair[1], pair[0]),
                VersionOrder::Greater
            );
        }
        assert_eq!(
            compare_provider_cli_versions("v1.2.3", "1.2.3"),
            VersionOrder::Equal
        );
        assert_eq!(
            compare_provider_cli_versions("1.2.3+a", "1.2.3+a"),
            VersionOrder::Equal
        );
        assert_eq!(
            compare_provider_cli_versions("1.2.3+a", "1.2.3"),
            VersionOrder::Incomparable
        );
        assert_eq!(
            compare_provider_cli_versions("1.2.3+a", "1.2.4"),
            VersionOrder::Less,
            "build metadata never hides a real difference"
        );
        assert_eq!(
            compare_provider_cli_versions("latest", "1.2.3"),
            VersionOrder::Incomparable
        );
    }

    #[test]
    fn version_from_output_takes_the_first_strict_token_of_the_first_line() {
        assert_eq!(
            provider_cli_version_from_output("codex-cli 0.163.0\n"),
            Some("0.163.0".into())
        );
        assert_eq!(
            provider_cli_version_from_output("\n\n  v1.2.3 (build 9)\nignored 4.5.6\n"),
            Some("1.2.3".into())
        );
        assert_eq!(
            provider_cli_version_from_output("gemini 2.0.0 codex 0.163.0"),
            Some("2.0.0".into()),
            "multiple versions: the first wins, later ones cannot spoof"
        );
        assert_eq!(provider_cli_version_from_output("codex-cli/1.2.3"), None);
        assert_eq!(provider_cli_version_from_output("version: latest"), None);
        assert_eq!(provider_cli_version_from_output(""), None);
        assert_eq!(
            provider_cli_version_from_output("\x1b[31m1.2.3\x1b[0m"),
            None
        );
        let many_tokens = format!("{} 1.2.3", "x ".repeat(40));
        assert_eq!(
            provider_cli_version_from_output(&many_tokens),
            None,
            "only the first sixteen tokens are inspected"
        );
        let huge = "9".repeat(100_000);
        assert_eq!(provider_cli_version_from_output(&huge), None);
        assert_eq!(
            provider_cli_version_text("\n  codex-cli 1.2.3\x1b[0m\nextra\n"),
            Some("codex-cli 1.2.3[0m".into())
        );
        assert_eq!(provider_cli_version_text("\n \n"), None);
        assert_eq!(
            provider_cli_version_text(&"a".repeat(1000)).map(|t| t.len()),
            Some(PROVIDER_CLI_VERSION_TEXT_MAX)
        );
    }

    #[test]
    fn decisions_follow_the_contract_table() {
        let target = "0.163.0";
        let absent = observation(ProviderCliObservedState::Absent, None, false);
        assert_eq!(
            decide_provider_cli_action(&absent, target),
            Some(ProviderCliDecision {
                action: ProviderCliAction::Install,
                version_relation: None,
            })
        );
        let same = observation(ProviderCliObservedState::Installed, Some("0.163.0"), true);
        assert_eq!(
            decide_provider_cli_action(&same, target),
            Some(ProviderCliDecision {
                action: ProviderCliAction::Noop,
                version_relation: None,
            })
        );
        let older = observation(ProviderCliObservedState::Installed, Some("0.162.9"), true);
        assert_eq!(
            decide_provider_cli_action(&older, target)
                .unwrap()
                .version_relation,
            Some(ProviderCliVersionRelation::Upgrade)
        );
        let newer = observation(ProviderCliObservedState::Installed, Some("0.164.0"), true);
        let decision = decide_provider_cli_action(&newer, target).unwrap();
        assert_eq!(decision.action, ProviderCliAction::Replace);
        assert_eq!(
            decision.version_relation,
            Some(ProviderCliVersionRelation::Downgrade)
        );
        let prerelease = observation(
            ProviderCliObservedState::Installed,
            Some("0.163.0-rc.1"),
            true,
        );
        assert_eq!(
            decide_provider_cli_action(&prerelease, target)
                .unwrap()
                .version_relation,
            Some(ProviderCliVersionRelation::Upgrade)
        );
        let build = observation(
            ProviderCliObservedState::Installed,
            Some("0.163.0+g1"),
            true,
        );
        assert_eq!(
            decide_provider_cli_action(&build, target)
                .unwrap()
                .version_relation,
            Some(ProviderCliVersionRelation::Incomparable)
        );
        // A spoofed probe that prints the pinned version only yields noop.
        let spoofed = observation(ProviderCliObservedState::Installed, Some("0.163.0"), true);
        assert_eq!(
            decide_provider_cli_action(&spoofed, target).unwrap().action,
            ProviderCliAction::Noop
        );
        let found_unknown = observation(ProviderCliObservedState::Unknown, None, true);
        assert_eq!(
            decide_provider_cli_action(&found_unknown, target),
            Some(ProviderCliDecision {
                action: ProviderCliAction::Replace,
                version_relation: Some(ProviderCliVersionRelation::Unknown),
            })
        );
        let unobservable = observation(ProviderCliObservedState::Unknown, None, false);
        assert!(unobservable.unobservable());
        assert_eq!(decide_provider_cli_action(&unobservable, target), None);
        // A probe whose descendants were not confirmed gone decides nothing,
        // whatever it printed.
        let mut unconfirmed = observation(ProviderCliObservedState::Unknown, None, true);
        unconfirmed.probe.cleanup_confirmed = false;
        unconfirmed.probe.error_code = Some("cleanup_unconfirmed".into());
        assert_eq!(decide_provider_cli_action(&unconfirmed, target), None);
        let mut truncated = observation(ProviderCliObservedState::Unknown, None, true);
        truncated.probe.error_code = Some("provider_cli_probe_output_truncated".into());
        assert_eq!(
            decide_provider_cli_action(&truncated, target)
                .unwrap()
                .version_relation,
            Some(ProviderCliVersionRelation::Unknown),
            "truncated output never yields a version relation without acknowledgement"
        );
        // Installed without a parsed version is a contract violation; no action.
        let inconsistent = observation(ProviderCliObservedState::Installed, None, true);
        assert_eq!(decide_provider_cli_action(&inconsistent, target), None);
        for relation in [
            ProviderCliVersionRelation::Downgrade,
            ProviderCliVersionRelation::Incomparable,
            ProviderCliVersionRelation::Unknown,
        ] {
            assert!(relation.requires_acknowledgement());
        }
        assert!(!ProviderCliVersionRelation::Upgrade.requires_acknowledgement());
    }

    #[test]
    fn observation_hash_is_golden_and_ignores_probe_noise() {
        let recipe_hash = "e9e559641f371bb3c403245e97ad6cff629e410175dd9677e19226cc415f7c12";
        let base = observation(ProviderCliObservedState::Installed, Some("0.162.0"), true);
        let hash = base.hash(recipe_hash, "0.163.0");
        assert_eq!(hash.len(), 64);
        let mut noisy = base.clone();
        noisy.probe.exit_code = Some(0);
        noisy.version_text = Some("codex-cli 0.162.0 (noise)".into());
        assert_eq!(noisy.hash(recipe_hash, "0.163.0"), hash);
        let mut moved = base.clone();
        moved.executable = Some("/other/prefix/bin/codex".into());
        assert_ne!(moved.hash(recipe_hash, "0.163.0"), hash);
        let mut other_version = base.clone();
        other_version.current_version = Some("0.162.1".into());
        assert_ne!(other_version.hash(recipe_hash, "0.163.0"), hash);
        assert_ne!(base.hash(recipe_hash, "0.164.0"), hash);
        assert_ne!(base.hash("00", "0.163.0"), hash);
        let summary = base.summary(recipe_hash, "0.163.0");
        assert_eq!(summary.observation_hash, hash);
        assert_eq!(summary.current_version.as_deref(), Some("0.162.0"));
        let json = serde_json::to_string(&summary).unwrap();
        assert!(!json.contains("/opt/prefix"), "summary carries no path");
    }

    #[test]
    fn observation_hash_golden_value() {
        let base = observation(ProviderCliObservedState::Installed, Some("0.162.0"), true);
        let recipe_hash = "e9e559641f371bb3c403245e97ad6cff629e410175dd9677e19226cc415f7c12";
        let canonical = canonical_json(&json!({
            "schema": PROVIDER_CLI_OBSERVATION_SCHEMA,
            "recipe_hash": recipe_hash,
            "target_version": "0.163.0",
            "state": "installed",
            "current_version": "0.162.0",
            "executable": "/opt/prefix/bin/codex",
            "found_in_prefix": true,
        }));
        assert_eq!(
            canonical,
            r#"{"current_version":"0.162.0","executable":"/opt/prefix/bin/codex","found_in_prefix":true,"recipe_hash":"e9e559641f371bb3c403245e97ad6cff629e410175dd9677e19226cc415f7c12","schema":"treer.provider-cli-observation/v1","state":"installed","target_version":"0.163.0"}"#
        );
        assert_eq!(
            base.hash(recipe_hash, "0.163.0"),
            format!("{:x}", Sha256::digest(canonical.as_bytes()))
        );
        let absent = observation(ProviderCliObservedState::Absent, None, false);
        assert_eq!(
            absent.hash(recipe_hash, "0.163.0"),
            format!(
                "{:x}",
                Sha256::digest(
                    r#"{"current_version":null,"executable":null,"found_in_prefix":false,"recipe_hash":"e9e559641f371bb3c403245e97ad6cff629e410175dd9677e19226cc415f7c12","schema":"treer.provider-cli-observation/v1","state":"absent","target_version":"0.163.0"}"#
                        .as_bytes()
                )
            )
        );
    }

    /// Deterministic pseudo-random probe lines: the parser must never panic,
    /// never exceed its bounds, and parse exactly its own normalized output.
    #[test]
    fn version_parsing_survives_random_and_hostile_output() {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let alphabet: &[u8] = b"0123456789.v-+abcXYZ \t\n\r\x1b[]/!";
        for _ in 0..5000 {
            let mut line = String::new();
            let length = (next(&mut state) % 90) as usize;
            for _ in 0..length {
                let index = (next(&mut state) as usize) % alphabet.len();
                line.push(alphabet[index] as char);
            }
            if let Some(parsed) = provider_cli_version_from_output(&line) {
                let reparsed = parse_strict_version(&parsed).expect("normalized parses");
                assert_eq!(reparsed.normalized(), parsed);
                assert!(parsed.len() <= PROVIDER_CLI_VERSION_TOKEN_MAX);
                assert!(!parsed.starts_with('v'));
            }
            if let Some(text) = provider_cli_version_text(&line) {
                assert!(text.len() <= PROVIDER_CLI_VERSION_TEXT_MAX);
                assert!(!text.chars().any(char::is_control));
            }
            let a = format!(
                "{}.{}.{}",
                next(&mut state) % 20,
                next(&mut state) % 20,
                next(&mut state) % 20
            );
            let b = format!(
                "{}.{}.{}",
                next(&mut state) % 20,
                next(&mut state) % 20,
                next(&mut state) % 20
            );
            let forward = compare_provider_cli_versions(&a, &b);
            let backward = compare_provider_cli_versions(&b, &a);
            let expected = match forward {
                VersionOrder::Less => VersionOrder::Greater,
                VersionOrder::Greater => VersionOrder::Less,
                other => other,
            };
            assert_eq!(backward, expected, "{a} vs {b}");
            assert_eq!(forward == VersionOrder::Equal, a == b);
        }
    }

    fn next(state: &mut u64) -> u64 {
        // xorshift64*: deterministic, dependency-free.
        let mut x = *state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        *state = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    #[test]
    fn validation_fails_closed_on_malformed_or_inconsistent_observations() {
        let mut absent = observation(ProviderCliObservedState::Absent, None, false);
        absent.probe.exit_code = None;
        absent.probe.error_code = Some("provider_cli_binary_not_found".into());
        absent.executable = Some("/opt/prefix/bin/codex".into());
        assert_eq!(absent.validate(), Ok(()));
        let installed = observation(ProviderCliObservedState::Installed, Some("0.162.0"), true);
        assert_eq!(installed.validate(), Ok(()));
        let mut unknown = observation(ProviderCliObservedState::Unknown, None, true);
        unknown.probe.exit_code = Some(3);
        unknown.probe.error_code = Some("provider_cli_probe_failed".into());
        assert_eq!(unknown.validate(), Ok(()));
        let mut unresolved = observation(ProviderCliObservedState::Unknown, None, false);
        unresolved.executable = None;
        unresolved.probe.error_code = Some("provider_cli_prefix_unresolved".into());
        assert_eq!(unresolved.validate(), Ok(()));

        let mut bad = installed.clone();
        bad.current_version = Some("latest".into());
        assert!(bad.validate().is_err(), "non-semver version");
        let mut bad = installed.clone();
        bad.current_version = Some("v0.162.0".into());
        assert!(bad.validate().is_err(), "non-canonical version");
        let mut bad = installed.clone();
        bad.version_text = Some("x\u{1b}[0m".into());
        assert!(bad.validate().is_err(), "control character");
        let mut bad = installed.clone();
        bad.version_text = Some("a".repeat(PROVIDER_CLI_VERSION_TEXT_MAX + 1));
        assert!(bad.validate().is_err(), "overlong text");
        let mut bad = installed.clone();
        bad.executable = Some("bin/codex".into());
        assert!(bad.validate().is_err(), "relative path");
        let mut bad = installed.clone();
        bad.executable = Some(format!("/{}", "p".repeat(PROVIDER_CLI_EXECUTABLE_MAX)));
        assert!(bad.validate().is_err(), "overlong path");
        let mut bad = installed.clone();
        bad.probe.error_code = Some("made_up".into());
        assert!(bad.validate().is_err(), "unknown probe code");
        let mut bad = installed.clone();
        bad.probe.exit_code = Some(1);
        assert!(bad.validate().is_err(), "installed with failing probe");
        let mut bad = installed.clone();
        bad.found_in_prefix = false;
        assert!(bad.validate().is_err(), "installed but not found");
        let mut bad = absent.clone();
        bad.found_in_prefix = true;
        assert!(bad.validate().is_err(), "absent but found");
        let mut bad = absent.clone();
        bad.current_version = Some("1.2.3".into());
        assert!(bad.validate().is_err(), "absent with version");
        let mut bad = unknown.clone();
        bad.probe.error_code = None;
        assert!(bad.validate().is_err(), "unknown without reason");
        let mut bad = unknown.clone();
        bad.current_version = Some("1.2.3".into());
        assert!(bad.validate().is_err(), "unknown with version");
        let mut bad = unresolved.clone();
        bad.executable = Some("/opt/prefix/bin/codex".into());
        assert!(bad.validate().is_err(), "unresolved prefix with a path");
        let windows = ProviderCliObservation {
            executable: Some("C:\\prefix\\codex.cmd".into()),
            ..installed.clone()
        };
        assert_eq!(windows.validate(), Ok(()));
        let mut unconfirmed = unknown.clone();
        unconfirmed.probe.cleanup_confirmed = false;
        unconfirmed.probe.error_code = Some("cleanup_unconfirmed".into());
        assert_eq!(unconfirmed.validate(), Ok(()));
        let mut bad = unknown.clone();
        bad.probe.cleanup_confirmed = false;
        assert!(bad.validate().is_err(), "unconfirmed cleanup must say so");
        let mut bad = unknown.clone();
        bad.probe.error_code = Some("cleanup_unconfirmed".into());
        assert!(
            bad.validate().is_err(),
            "confirmed cleanup cannot claim otherwise"
        );
        let mut bad = installed.clone();
        bad.probe.cleanup_confirmed = false;
        assert!(
            bad.validate().is_err(),
            "installed requires confirmed cleanup"
        );
    }

    #[test]
    fn enums_round_trip_through_serde_and_text() {
        for action in [
            ProviderCliAction::Install,
            ProviderCliAction::Replace,
            ProviderCliAction::Noop,
        ] {
            assert_eq!(ProviderCliAction::parse(action.as_str()), Some(action));
            let json = serde_json::to_string(&action).unwrap();
            assert_eq!(json, format!("\"{}\"", action.as_str()));
        }
        for relation in [
            ProviderCliVersionRelation::Upgrade,
            ProviderCliVersionRelation::Downgrade,
            ProviderCliVersionRelation::Incomparable,
            ProviderCliVersionRelation::Unknown,
        ] {
            assert_eq!(
                ProviderCliVersionRelation::parse(relation.as_str()),
                Some(relation)
            );
        }
        for state in [
            ProviderCliObservedState::Absent,
            ProviderCliObservedState::Installed,
            ProviderCliObservedState::Unknown,
        ] {
            assert_eq!(ProviderCliObservedState::parse(state.as_str()), Some(state));
        }
        assert_eq!(ProviderCliAction::parse("reinstall"), None);
        assert!(serde_json::from_str::<ProviderCliAction>("\"reinstall\"").is_err());
    }
}
