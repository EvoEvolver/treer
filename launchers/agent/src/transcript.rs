//! Bounded retained provider history. Replacements are admitted atomically;
//! exceeding the budget never evicts an accepted turn or receipt.
use anyhow::Result;
use serde_json::Value;
use std::{collections::HashMap, fmt, io, ops::Deref};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_TURNS: usize = 50;

#[derive(Debug)]
pub struct HistoryLimit;
impl fmt::Display for HistoryLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Conversation history limit reached; restarting the provider")
    }
}
impl std::error::Error for HistoryLimit {}

#[derive(Debug)]
pub struct InvalidTurnIdentity;
impl fmt::Display for InvalidTurnIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invalid provider turn identity; restarting the provider")
    }
}
impl std::error::Error for InvalidTurnIdentity {}

#[derive(Default)]
pub struct Transcript {
    turns: Vec<Value>,
    sizes: Vec<usize>,
    positions: HashMap<String, usize>,
    bytes: usize,
    pub dropped: usize,
}
impl Deref for Transcript {
    type Target = [Value];
    fn deref(&self) -> &Self::Target {
        &self.turns
    }
}
impl Transcript {
    pub fn insert(&mut self, turn: Value) -> Result<()> {
        let id = turn["id"].as_str().ok_or(InvalidTurnIdentity)?;
        let index = self.positions.get(id).copied();
        let old_size = index.map(|i| self.sizes[i]).unwrap_or(0);
        let available = MAX_BYTES;
        // Count JSON bytes without allocating another copy of the transcript.
        // The writer stops at the limit even for a large candidate turn.
        let mut counter = Counter {
            bytes: 0,
            available,
        };
        if serde_json::to_writer(&mut counter, &turn).is_err() {
            return Err(HistoryLimit.into());
        }
        let mut next_bytes = self.bytes - old_size + counter.bytes;
        let mut next_count = self.turns.len() + usize::from(index.is_none());
        let mut next_nodes = crate::provider_stream::shape_nodes(&turn)
            + self
                .turns
                .iter()
                .enumerate()
                .filter(|(i, _)| Some(*i) != index)
                .map(|(_, t)| crate::provider_stream::shape_nodes(t))
                .sum::<usize>();
        let mut remove = Vec::new();
        for (i, old) in self.turns.iter().enumerate() {
            if next_bytes <= MAX_BYTES && next_count <= MAX_TURNS && next_nodes <= 500_000 {
                break;
            }
            if Some(i) != index && old["status"] != "inProgress" {
                remove.push(i);
                next_bytes -= self.sizes[i];
                next_count -= 1;
                next_nodes -= crate::provider_stream::shape_nodes(old);
            }
        }
        if next_bytes > MAX_BYTES || next_count > MAX_TURNS || next_nodes > 500_000 {
            return Err(HistoryLimit.into());
        }
        self.bytes = self.bytes - old_size + counter.bytes;
        if let Some(i) = index {
            self.turns[i] = turn;
            self.sizes[i] = counter.bytes;
        } else {
            self.positions.insert(id.to_owned(), self.turns.len());
            self.turns.push(turn);
            self.sizes.push(counter.bytes);
        }
        for i in remove.into_iter().rev() {
            self.bytes -= self.sizes.remove(i);
            self.turns.remove(i);
            self.dropped += 1;
        }
        self.positions = self
            .turns
            .iter()
            .enumerate()
            .map(|(i, t)| (t["id"].as_str().unwrap().to_owned(), i))
            .collect();
        Ok(())
    }
}
struct Counter {
    bytes: usize,
    available: usize,
}
impl io::Write for Counter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.available - self.bytes {
            return Err(io::Error::other(HistoryLimit));
        }
        self.bytes += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn invalid_identity_is_rejected_without_changing_history() {
        let mut history = Transcript::default();
        history.insert(json!({"id":"accepted"})).unwrap();
        for id in [Value::Null, json!(123), json!({"secret":"never echo"})] {
            let error = history.insert(json!({"id":id})).unwrap_err();
            assert!(error.is::<InvalidTurnIdentity>());
            assert_eq!(
                error.to_string(),
                "Invalid provider turn identity; restarting the provider"
            );
            assert_eq!(history.len(), 1);
            assert_eq!(history[0]["id"], "accepted");
        }
    }

    #[test]
    fn repeated_replacements_charge_only_retained_bytes_and_reject_atomically() {
        let mut history = Transcript::default();
        for n in 0..20_000 {
            history
                .insert(json!({"id":"same","text":n.to_string()}))
                .unwrap();
        }
        let before = history.turns.clone();
        let bytes = history.bytes;
        // Escaping consumes serialized bytes, not just raw string length.
        let error = history
            .insert(json!({"id":"same","text":"\n".repeat(MAX_BYTES / 2)}))
            .unwrap_err();
        assert!(error.is::<HistoryLimit>());
        assert_eq!(history.turns, before);
        assert_eq!(history.bytes, bytes);
    }

    #[test]
    fn recent_window_evicts_finished_turns_and_preserves_active() {
        let mut history = Transcript::default();
        history
            .insert(json!({"id":"active","status":"inProgress"}))
            .unwrap();
        for n in 0..1000 {
            history
                .insert(json!({"id":n.to_string(),"items":[]}))
                .unwrap();
        }
        assert_eq!(history.len(), MAX_TURNS);
        assert_eq!(history[0]["id"], "active");
        assert_eq!(history[1]["id"], "951");
        history
            .insert(json!({"id":"active","status":"completed","items":["replacement"]}))
            .unwrap();
        assert_eq!(history.len(), MAX_TURNS);
    }
}
