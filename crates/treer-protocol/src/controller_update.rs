//! Controller self-update: "update machine M to release version V".
//!
//! The Proxy sends `ControllerUpdateStart { version }`; the Controller downloads
//! the signed release for its platform, replaces its binaries next to the
//! installed ones and asks the Host for a plain Controller restart. The new
//! Controller reporting `version` on reconnect is the completion signal.
use serde::{Deserialize, Serialize};

pub const CAPABILITY: &str = "machine.controller-update.v2";

/// Body of `POST .../machines/{server_id}/controller-update`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRequest {
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateState {
    Idle,
    Downloading,
    Installing,
    Restarting,
    Failed { reason: String },
    Done,
}

impl UpdateState {
    pub fn active(&self) -> bool {
        matches!(
            self,
            Self::Downloading | Self::Installing | Self::Restarting
        )
    }
}

/// Read model returned by both the Controller and the Proxy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    #[serde(flatten)]
    pub state: UpdateState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_version: Option<String>,
}

/// Release tags published by scripts/release-r2.mjs: `vMAJOR.MINOR.PATCH[-PRE]`.
pub fn valid_version(version: &str) -> bool {
    let Some(rest) = version.strip_prefix('v') else {
        return false;
    };
    let (core, pre) = rest.split_once('-').unwrap_or((rest, ""));
    let numbers: Vec<&str> = core.split('.').collect();
    version.len() <= 64
        && numbers.len() == 3
        && numbers.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
        && (!rest.contains('-')
            || (!pre.is_empty()
                && pre
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')))
}

/// Binary version (`CARGO_PKG_VERSION`) a release tag produces.
pub fn binary_version(version: &str) -> &str {
    let rest = version.strip_prefix('v').unwrap_or(version);
    rest.split_once('-').map_or(rest, |(core, _)| core)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_follow_release_tags() {
        for ok in ["v0.1.15", "v1.0.0-rc.1", "v10.20.30"] {
            assert!(valid_version(ok), "{ok}");
        }
        for bad in [
            "0.1.15", "v0.1", "v01.1.1", "v0.1.1-", "v0.1.1/x", "v../1.1",
        ] {
            assert!(!valid_version(bad), "{bad}");
        }
        assert_eq!(binary_version("v1.2.3-rc.1"), "1.2.3");
    }

    #[test]
    fn status_wire_shape_is_flat() {
        let status = UpdateStatus {
            state: UpdateState::Failed {
                reason: "bad signature".into(),
            },
            target_version: Some("v0.1.16".into()),
            controller_version: None,
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"state":"failed","reason":"bad signature","target_version":"v0.1.16"})
        );
        assert_eq!(
            serde_json::from_value::<UpdateStatus>(value).unwrap(),
            status
        );
    }
}
