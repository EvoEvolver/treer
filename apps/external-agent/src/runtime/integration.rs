//! Agent handoff contains configuration and instructions, never credentials.
use super::config::Config;
use serde_json::{json, Value};

pub(super) fn reachability(origin: &url::Url) -> &'static str {
    if origin.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    }) {
        "loopback_only"
    } else {
        "configured_https_unverified"
    }
}

pub(super) fn view(config: &Config, enabled: bool) -> Value {
    let origin = &config.public_origin;
    let mut blockers = Vec::new();
    if reachability(origin) == "loopback_only" {
        blockers.push("loopback_only");
    } else {
        blockers.push("data_mapping_not_verified");
    }
    if !enabled {
        blockers.push("disabled");
    }
    json!({
        "schema_version":1,
        "identity":{"workspace":config.workspace,"app":config.app,"agent":config.agent},
        "data_base_url":origin.as_str(),
        "reachability":reachability(origin),
        "enabled":enabled,
        "activation_available":true,
        "prompt_available":true,
        "blockers":blockers,
        "manual_path":"SKILL.md",
        "skill_markdown":format!("{}\n\nOwner status at copy time: enabled={enabled}. This snapshot can change; it is not permission to execute.\n", short(config, origin))
    })
}

pub(super) fn manual(config: &Config) -> String {
    render(
        include_str!("agent-skill.md"),
        config,
        &config.public_origin,
    )
}
fn short(config: &Config, origin: &url::Url) -> String {
    render(include_str!("agent-connect.md"), config, origin)
}
fn render(template: &str, config: &Config, origin: &url::Url) -> String {
    template
        .replace("{{DATA_BASE_URL}}", origin.as_str())
        .replace(
            "{{PYTHON_BASE_URL}}",
            &serde_json::to_string(origin.as_str()).expect("URL string"),
        )
        .replace("{{APP_ID}}", &config.app)
        .replace("{{WORKSPACE_ID}}", &config.workspace)
        .replace("{{REACHABILITY}}", reachability(origin))
        .replace("{{POLICY_STATUS}}", "Owner confirmation enables this App; every request remains subject to its current workspace Policy.")
}
