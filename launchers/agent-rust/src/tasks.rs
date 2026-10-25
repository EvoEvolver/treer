//! Claude task metadata, independent of turn/receipt authority. Never executes a task.
use anyhow::{bail, Result};
use serde_json::{json, Value};

fn text<'a>(value: &'a Value, key: &str, limit: usize) -> Option<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= limit && !s.contains('\0'))
}
fn timestamp(value: &Value) -> Option<Value> {
    if let Some(s) = value.as_str().filter(|s| s.len() <= 128) {
        return chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| json!(d.to_rfc3339()));
    }
    value
        .as_i64()
        .and_then(|n| {
            if n < 1_000_000_000_000 {
                n.checked_mul(1000)
            } else {
                Some(n)
            }
        })
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|d| json!(d.to_rfc3339()))
}
pub fn update(tasks: &mut Value, event: &Value) -> Result<()> {
    // Atomic metadata admission: an invalid batch cannot partially update the map.
    let mut next = tasks.as_array().cloned().unwrap_or_default();
    let updates = if event["subtype"] == "background_tasks_changed" {
        event["tasks"]
            .as_array()
            .filter(|v| v.len() <= 100)
            .ok_or_else(|| anyhow::anyhow!("claude_tasks_invalid"))?
            .clone()
    } else {
        vec![event.clone()]
    };
    let now = chrono::Utc::now();
    for value in updates {
        let Some(id) = text(&value, "task_id", 1024) else {
            continue;
        };
        let mut patch = value.clone();
        if let Some(fields) = value["patch"].as_object() {
            for (key, value) in fields {
                patch[key] = value.clone();
            }
        }
        let index = next.iter().position(|t| t["id"] == id);
        let prior = index.map(|i| next[i].clone()).unwrap_or(Value::Null);
        let status = match patch["status"].as_str() {
            Some("failed" | "error") => "failed",
            Some("stopped" | "killed" | "cancelled" | "canceled") => "stopped",
            Some("completed" | "done" | "success") => "completed",
            Some("running" | "in_progress") => "running",
            _ => prior["status"].as_str().unwrap_or("running"),
        };
        let kind = match patch["task_type"].as_str() {
            Some(s) if s.contains("bash") || s == "shell" => "shell",
            Some(s) if s.contains("agent") => "agent",
            Some(s) if s.contains("monitor") => "monitor",
            _ => prior["kind"].as_str().unwrap_or("other"),
        };
        let description = text(&patch, "description", 16 * 1024)
            .map(json_value)
            .unwrap_or_else(|| prior.get("description").cloned().unwrap_or(json!(id)));
        let summary = text(&patch, "summary", 64 * 1024)
            .map(json_value)
            .unwrap_or_else(|| prior["summary"].clone());
        let exit = patch["exit_code"]
            .as_i64()
            .or(prior["exitCode"].as_i64())
            .or_else(|| {
                let s = summary.as_str()?.split_once("exit code ")?.1;
                s.split(|c: char| !c.is_ascii_digit() && c != '-')
                    .next()?
                    .parse::<i64>()
                    .ok()
            });
        let started = timestamp(&patch["start_time"])
            .or_else(|| prior.get("startedAt").cloned())
            .unwrap_or(json!(now.to_rfc3339()));
        let ended = timestamp(&patch["end_time"])
            .or_else(|| prior.get("endedAt").filter(|v| !v.is_null()).cloned())
            .unwrap_or_else(|| {
                if status == "running" {
                    Value::Null
                } else {
                    json!(now.to_rfc3339())
                }
            });
        let mut task = json!({"id":id,"kind":kind,"description":description,"status":status,"startedAt":started,"endedAt":ended,"exitCode":exit,"summary":summary});
        if let Some(path) = text(&patch, "output_file", 8192)
            .map(json_value)
            .or_else(|| prior.get("outputFile").cloned())
        {
            task["outputFile"] = path;
        }
        if let Some(index) = index {
            next[index] = task;
        } else {
            next.push(task);
        }
    }
    // Running records are never pruned. Bound resource admission rather than kill tasks.
    let mut finished = next
        .iter()
        .enumerate()
        .filter(|(_, t)| t["status"] != "running")
        .map(|(i, t)| {
            (
                i,
                t["endedAt"]
                    .as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.timestamp_millis())
                    .unwrap_or(0),
            )
        })
        .collect::<Vec<_>>();
    finished.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
    let remove = finished
        .into_iter()
        .enumerate()
        .filter(|(rank, (_, at))| {
            *rank >= 20 || now.timestamp_millis().saturating_sub(*at) > 86_400_000
        })
        .map(|(_, (i, _))| i)
        .collect::<std::collections::HashSet<_>>();
    next = next
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !remove.contains(i))
        .map(|(_, v)| v)
        .collect();
    let result = json!(next);
    if next.len() > 100 || serde_json::to_vec(&result)?.len() > 1024 * 1024 {
        bail!("claude_tasks_limit");
    }
    *tasks = result;
    Ok(())
}
fn json_value(s: &str) -> Value {
    json!(s)
}
pub fn finished(tasks: &Value) -> Value {
    json!(tasks
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["status"] != "running")
        .cloned()
        .collect::<Vec<_>>())
}
pub fn clear(tasks: &mut Value) -> usize {
    let before = tasks.as_array().map_or(0, Vec::len);
    *tasks = json!(tasks
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["status"] == "running")
        .cloned()
        .collect::<Vec<_>>());
    before - tasks.as_array().map_or(0, Vec::len)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_events_are_independent_bounded_metadata() {
        let mut tasks = json!([]);
        update(
            &mut tasks,
            &json!({"task_id":"a","task_type":"local_bash","description":"own","status":"running"}),
        )
        .unwrap();
        update(
            &mut tasks,
            &json!({"task_id":"a","patch":{"summary":"exit code -2","status":"completed"}}),
        )
        .unwrap();
        assert_eq!(tasks[0]["exitCode"], -2);
        assert_eq!(tasks[0]["kind"], "shell");
        crate::soul_terminal::validate(&finished(&tasks)).unwrap();
        update(&mut tasks, &json!({"task_id":"b","status":"running"})).unwrap();
        assert_eq!(clear(&mut tasks), 1);
        assert_eq!(tasks[0]["id"], "b");
        let old = tasks.clone();
        assert!(update(
            &mut tasks,
            &json!({"subtype":"background_tasks_changed","tasks":vec![json!({});101]})
        )
        .is_err());
        assert_eq!(tasks, old);
    }
    #[test]
    fn retention_never_prunes_running_and_oversize_batch_is_atomic() {
        let mut tasks = json!([]);
        let records=(0..100).map(|i|json!({"task_id":format!("running-{i}"),"status":"running","start_time":1700000000})).collect::<Vec<_>>();
        update(
            &mut tasks,
            &json!({"subtype":"background_tasks_changed","tasks":records}),
        )
        .unwrap();
        assert_eq!(tasks[0]["startedAt"], "2023-11-14T22:13:20+00:00");
        let old = tasks.clone();
        assert!(update(
            &mut tasks,
            &json!({"task_id":"overflow","status":"running"})
        )
        .is_err());
        assert_eq!(tasks, old);
        assert_eq!(clear(&mut tasks), 0);
        assert_eq!(finished(&tasks), json!([]));
        let records = (0..30)
            .map(|i| json!({"task_id":format!("done-{i}"),"status":"done"}))
            .collect::<Vec<_>>();
        let mut done = json!([]);
        update(
            &mut done,
            &json!({"subtype":"background_tasks_changed","tasks":records}),
        )
        .unwrap();
        assert_eq!(done.as_array().unwrap().len(), 20);
        assert_eq!(done[0]["id"], "done-10");
    }
}
