//! Frozen stdout contract, checked on every production report and on all OSes.
use serde_json::{json, Value};

const ROOT: &[&str] = &[
    "schema",
    "result",
    "parent_pid",
    "host_version_source",
    "host_version",
    "windows_build",
    "helper_sha256",
    "helper_size",
    "parent_in_job",
    "parent_job_limit_flags",
    "attempts",
    "error_stage",
    "win32_error",
];
const ATTEMPT: &[&str] = &[
    "control",
    "flags",
    "child_pid",
    "child_creation_time",
    "child_in_job",
    "child_in_parent_job",
    "cleanup_confirmed",
    "result",
    "error_stage",
    "win32_error",
];
const CONTROL: &[&str] = &[
    "flags",
    "child_pid",
    "child_creation_time",
    "child_in_job",
    "child_in_parent_job",
    "cleanup_confirmed",
    "result",
    "error_stage",
    "win32_error",
];
const STAGES: &[&str] = &[
    "approved_directory",
    "unapproved_directory",
    "image_open",
    "image_identity",
    "image_reparse_or_alias",
    "image_final_path",
    "self_path",
    "self_metadata",
    "self_size",
    "self_hash",
    "self_changed",
    "job_query",
    "child_create",
    "breakaway_denied",
    "child_wait",
    "child_exited_early",
    "child_image_query",
    "child_image_mismatch",
    "child_creation_query",
    "child_still_in_job",
    "cleanup_unconfirmed",
    "parent_job_limits_unavailable",
    "parent_job_members",
    "parent_job_members_incomplete",
    "windows_version",
    "not_exec_job_evidence",
    "invalid_arguments",
];

pub(super) fn empty(pid: u32) -> Value {
    json!({"schema":1,"result":"failed","parent_pid":pid,
        "host_version_source":"runner_existing_host_query","host_version":null,
        "windows_build":null,"helper_sha256":null,"helper_size":null,"parent_in_job":null,
        "parent_job_limit_flags":null,"attempts":[],"error_stage":null,"win32_error":null})
}

fn keys(value: &Value, expected: &[&str]) -> bool {
    value.as_object().is_some_and(|o| {
        o.len() == expected.len() && o.keys().all(|k| expected.contains(&k.as_str()))
    })
}
fn optional_number(value: &Value) -> bool {
    value.is_null() || value.as_u64().is_some()
}
fn optional_bool(value: &Value) -> bool {
    value.is_null() || value.is_boolean()
}
fn outcome(value: &Value, success: &str) -> bool {
    (value["result"] == "failed" || value["result"] == success)
        && (value["error_stage"].is_null()
            || value["error_stage"]
                .as_str()
                .is_some_and(|s| STAGES.contains(&s)))
        && (value["win32_error"].is_null() || value["win32_error"].as_i64().is_some())
}
fn process_fields(v: &Value, success: &str) -> bool {
    outcome(v, success)
        && v["flags"].as_u64().is_some()
        && optional_number(&v["child_pid"])
        && optional_number(&v["child_creation_time"])
        && optional_bool(&v["child_in_job"])
        && optional_bool(&v["child_in_parent_job"])
        && v["cleanup_confirmed"].is_boolean()
}
pub(super) fn valid(value: &Value) -> bool {
    keys(value, ROOT)
        && outcome(value, "breakaway_observed")
        && value["schema"] == 1
        && value["parent_pid"].as_u64().is_some_and(|pid| pid > 0)
        && value["host_version_source"] == "runner_existing_host_query"
        && value["host_version"].is_null()
        && optional_number(&value["windows_build"])
        && optional_number(&value["helper_size"])
        && optional_number(&value["parent_job_limit_flags"])
        && optional_bool(&value["parent_in_job"])
        && (value["helper_sha256"].is_null()
            || value["helper_sha256"]
                .as_str()
                .is_some_and(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())))
        && value["attempts"].as_array().is_some_and(|a| {
            a.len() <= 2
                && a.iter().all(|v| {
                    keys(v, ATTEMPT)
                        && process_fields(v, "breakaway_observed")
                        && (v["control"].is_null()
                            || (keys(&v["control"], CONTROL)
                                && process_fields(&v["control"], "control_started")))
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_rejects_extra_fields_and_untrusted_text_at_both_levels() {
        let mut baseline = empty(1);
        baseline["attempts"] = json!([{"flags":1,"child_pid":null,"child_creation_time":null,
            "child_in_job":null,"child_in_parent_job":null,"cleanup_confirmed":false,
            "result":"failed","error_stage":"child_create","win32_error":5,"control":null}]);
        baseline["attempts"][0]["control"] = json!({"flags":0,"child_pid":2,"child_creation_time":1,
            "child_in_job":true,"child_in_parent_job":true,"cleanup_confirmed":true,
            "result":"control_started","error_stage":null,"win32_error":null});
        assert!(valid(&baseline));
        for key in [
            "path",
            "argv",
            "environment",
            "username",
            "token",
            "raw_error",
        ] {
            let mut value = baseline.clone();
            value[key] = json!("private material");
            assert!(!valid(&value), "extra root field {key}");
            let mut value = baseline.clone();
            value["attempts"][0][key] = json!("private material");
            assert!(!valid(&value), "extra nested field {key}");
            let mut value = baseline.clone();
            value["attempts"][0]["control"][key] = json!("private material");
            assert!(!valid(&value), "extra control field {key}");
        }
        for key in [
            "result",
            "error_stage",
            "host_version_source",
            "helper_sha256",
            "host_version",
        ] {
            let mut value = baseline.clone();
            value[key] = json!(r"C:\Users\private\document --secret=credential");
            assert!(!valid(&value), "untrusted text in {key}");
        }
        let mut value = baseline.clone();
        value["attempts"][0]["error_stage"] = json!("raw OS error containing private path");
        assert!(!valid(&value));
        let mut value = baseline.clone();
        value["attempts"][0]["control"]["error_stage"] =
            json!("raw control error containing private path");
        assert!(!valid(&value));
        baseline["attempts"][0]["child_pid"] = json!("environment value");
        assert!(!valid(&baseline));
    }

    #[test]
    fn invalid_argument_failure_is_a_bounded_whitelisted_report_without_child() {
        let mut value = empty(1);
        value["error_stage"] = json!("invalid_arguments");
        assert!(valid(&value));
        assert!(serde_json::to_vec(&value).unwrap().len() < 8192);
        assert!(value["attempts"].as_array().unwrap().is_empty());
    }
}
