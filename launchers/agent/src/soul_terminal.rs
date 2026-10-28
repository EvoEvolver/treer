//! Passive, terminal-only Node metadata. Never task execution or file authority.
use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::HashSet;

pub fn validate(value: &Value) -> Result<Value> {
    let tasks = value
        .as_array()
        .filter(|v| v.len() <= 100)
        .ok_or_else(|| anyhow::anyhow!("terminal_tasks_invalid"))?;
    if serde_json::to_vec(value)?.len() > 1024 * 1024 {
        bail!("terminal_tasks_limit");
    }
    let mut ids = HashSet::new();
    for task in tasks {
        crate::runtime::node_fields(
            task,
            &[
                "id",
                "kind",
                "description",
                "status",
                "startedAt",
                "endedAt",
                "exitCode",
                "summary",
                "outputFile",
            ],
        )?;
        let id = task["id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 1024)
            .ok_or_else(|| anyhow::anyhow!("terminal_tasks_identity"))?;
        if !ids.insert(id)
            || !matches!(
                task["kind"].as_str(),
                Some("shell" | "agent" | "monitor" | "other")
            )
            || !matches!(
                task["status"].as_str(),
                Some("completed" | "failed" | "stopped")
            )
            || task["description"]
                .as_str()
                .is_none_or(|s| s.len() > 16 * 1024)
        {
            bail!("terminal_tasks_invalid");
        }
        for key in ["startedAt", "endedAt"] {
            if !task.get(key).is_some_and(|v| {
                v.is_null()
                    || v.as_str().is_some_and(|s| {
                        s.len() <= 128 && chrono::DateTime::parse_from_rfc3339(s).is_ok()
                    })
            }) {
                bail!("terminal_tasks_timestamp");
            }
        }
        if !task
            .get("exitCode")
            .is_some_and(|v| v.is_null() || v.as_i64().is_some())
            || !task
                .get("summary")
                .is_some_and(|v| v.is_null() || v.as_str().is_some_and(|s| s.len() <= 64 * 1024))
            || task.get("outputFile").is_some_and(|v| {
                !v.as_str()
                    .is_some_and(|s| s.len() <= 8192 && !s.contains('\0'))
            })
        {
            bail!("terminal_tasks_invalid");
        }
    }
    Ok(value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_bounded_unique_terminal_metadata_survives_unchanged() {
        let task = json!({"id":"task1","kind":"shell","description":"retained","status":"completed","startedAt":null,"endedAt":"2026-10-22T00:00:00Z","exitCode":0,"summary":"done","outputFile":"/synthetic/never-open"});
        let tasks = json!([task]);
        assert_eq!(validate(&tasks).unwrap(), tasks);
        for (key, value) in [
            ("status", json!("running")),
            ("kind", json!("new-kind")),
            ("endedAt", json!(false)),
            ("exitCode", json!(0.5)),
            ("summary", json!(false)),
            ("execute", json!(true)),
        ] {
            let mut bad = tasks.clone();
            bad[0][key] = value;
            assert!(validate(&bad).is_err(), "{key}");
        }
        assert!(validate(&json!([tasks[0], tasks[0]])).is_err());
        assert!(validate(&json!(vec![tasks[0].clone(); 101])).is_err());
    }
}
