//! Native goal state and a bounded, durable model-context delivery journal.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Goals {
    pub goal: Value,
    pub edits: Vec<Edit>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Edit {
    pub id: String,
    pub input: Value,
    pub previous: Value,
    pub goal: Value,
    pub applying: bool,
    pub applied: bool,
    pub sent: bool,
    pub prompt_id: Value,
    pub error: Value,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub attempt: u32,
}
impl Goals {
    pub fn load(value: &Value) -> Result<Self> {
        if value.is_null() {
            return Ok(Self::default());
        }
        let state: Self = serde_json::from_value(value.clone())
            .map_err(|_| anyhow::anyhow!("private_state_invalid"))?;
        if state.edits.len() > 1024 || state.edits.iter().filter(|e| !e.sent).count() > 256 {
            bail!("private_state_invalid");
        }
        let mut ids = std::collections::HashSet::new();
        for e in &state.edits {
            if !operation_id(&e.id)
                || !ids.insert(&e.id)
                || validate(&e.input).ok().as_ref() != Some(&e.input)
                || e.mode
                    .as_deref()
                    .is_some_and(|m| m != "queue" && m != "steer")
                || e.sent && !e.applied
                || e.applied && !e.applying
            {
                bail!("private_state_invalid");
            }
        }
        Ok(state)
    }
    pub fn delivery(&self) -> Value {
        self.edits.last().map(|e| json!({"id":e.id,"status":if e.sent {"sent"} else if e.error.is_string() {"failed"} else {"pending"},"error":e.error,"promptId":e.prompt_id})).unwrap_or(Value::Null)
    }
    pub fn add(&mut self, id: &str, input: Value) -> Result<()> {
        if !operation_id(id) {
            bail!("goal_operation_id_invalid");
        }
        if let Some(e) = self.edits.iter().find(|e| e.id == id) {
            if e.input != input {
                bail!("goal_operation_id_conflict");
            }
            return Ok(());
        }
        if self.edits.iter().filter(|e| !e.sent).count() >= 256 {
            bail!("goal_edit_limit");
        }
        if self.edits.len() >= 1024 {
            let remove = self
                .edits
                .iter()
                .position(|e| e.sent)
                .ok_or_else(|| anyhow::anyhow!("goal_edit_limit"))?;
            self.edits.remove(remove);
        }
        self.edits.push(Edit {
            id: id.into(),
            input,
            previous: self.goal.clone(),
            goal: Value::Null,
            applying: false,
            applied: false,
            sent: false,
            prompt_id: Value::Null,
            error: Value::Null,
            mode: None,
            attempt: 0,
        });
        Ok(())
    }
}
fn operation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
pub fn validate(body: &Value) -> Result<Value> {
    let mut input = json!({});
    if let Some(v) = body.get("objective") {
        let Some(s) = v
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 100_000)
        else {
            bail!("goal_objective_invalid");
        };
        input["objective"] = json!(s);
    }
    if let Some(v) = body.get("status") {
        if !v.as_str().is_some_and(|s| {
            [
                "active",
                "paused",
                "blocked",
                "stale",
                "done",
                "cleared",
                "usageLimited",
                "budgetLimited",
                "complete",
            ]
            .contains(&s)
        }) {
            bail!("goal_status_invalid");
        }
        input["status"] = v.clone();
    }
    for (key, max) in [
        ("tokenBudget", 9_007_199_254_740_991_u64),
        ("roundBudget", 10_000),
    ] {
        if let Some(v) = body.get(key) {
            if !(v.is_null() || v.as_u64().is_some_and(|n| n > 0 && n <= max)) {
                bail!("goal_budget_invalid");
            }
            input[key] = v.clone();
        }
    }
    for key in ["reason", "requiredAction"] {
        if let Some(v) = body.get(key) {
            let Some(s) = v
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty() && s.len() <= 4096)
            else {
                bail!("goal_reason_invalid");
            };
            input[key] = json!(s);
        }
    }
    if input.as_object().is_none_or(|o| o.is_empty()) {
        bail!("goal_update_empty");
    }
    if [json!("blocked"), json!("stale"), json!("done")].contains(&input["status"])
        && !input["reason"].is_string()
    {
        bail!("goal_reason_required");
    }
    Ok(input)
}
pub fn map(raw: &Value, previous: &Value) -> Value {
    let Some(objective) = raw["objective"].as_str() else {
        return Value::Null;
    };
    let status = match raw["status"].as_str().unwrap_or("") {
        "complete" => "done",
        s @ ("active" | "paused" | "blocked" | "stale" | "done" | "cleared" | "usageLimited"
        | "budgetLimited") => s,
        _ => "blocked",
    };
    let same = previous["objective"] == objective;
    let mut goal = json!({"id":if same {previous["id"].as_str().map(str::to_owned).unwrap_or_else(||uuid::Uuid::new_v4().to_string())} else {uuid::Uuid::new_v4().to_string()},"objective":objective,"status":status,"roundBudget":if same {previous["roundBudget"].clone()} else {Value::Null},"roundsUsed":if same {previous["roundsUsed"].as_u64().unwrap_or(0)} else {0}});
    for key in ["tokenBudget", "tokensUsed", "timeUsedSeconds"] {
        goal[key] = if raw[key].is_number() {
            raw[key].clone()
        } else {
            Value::Null
        };
    }
    goal
}
pub fn prompt(previous: &Value, goal: &Value) -> String {
    format!("The user changed the goal. New objective (user-authored): {}\nPrevious objective: {}\nStatus: {}. Token budget: {}. Round budget: {}.\n{}", goal["objective"].as_str().unwrap_or("None"),previous["objective"].as_str().unwrap_or("None"),goal["status"].as_str().unwrap_or("cleared"),goal["tokenBudget"].as_u64().map(|n|n.to_string()).unwrap_or("unlimited".into()),goal["roundBudget"].as_u64().map(|n|n.to_string()).unwrap_or("unlimited".into()),if goal["status"] == "paused" {"Pause goal-directed work and await the user's instructions."} else {"Use the updated goal and budget; do not keep following the previous objective."})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_and_edit_recovery_are_bounded_and_idempotent() {
        for v in [
            json!({}),
            json!({"objective":" "}),
            json!({"tokenBudget":0}),
            json!({"roundBudget":10001}),
            json!({"status":"done"}),
        ] {
            assert!(validate(&v).is_err());
        }
        let input = validate(&json!({"objective":" ship ","tokenBudget":null})).unwrap();
        let mut state = Goals::default();
        state.add("one", input.clone()).unwrap();
        state.add("one", input).unwrap();
        assert_eq!(state.edits.len(), 1);
        assert!(state.add("one", json!({"objective":"other"})).is_err());
        let recovered = Goals::load(&serde_json::to_value(state).unwrap()).unwrap();
        assert!(!recovered.edits[0].applying);
        assert_eq!(recovered.delivery()["status"], "pending");
        assert_eq!(
            map(
                &json!({"objective":"ship","status":"complete"}),
                &Value::Null
            )["status"],
            "done"
        );
    }
}
