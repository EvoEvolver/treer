//! Bounded retained provider history. Replacements are admitted atomically;
//! exceeding the budget never evicts an accepted turn or receipt.
use anyhow::Result;
use serde_json::Value;
use std::{collections::HashMap, fmt, io, ops::Deref};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_TURNS: usize = 10_000;

#[derive(Debug)]
pub struct HistoryLimit;
impl fmt::Display for HistoryLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Rust experiment history limit reached; queue paused")
    }
}
impl std::error::Error for HistoryLimit {}

#[derive(Default)]
pub struct Transcript {
    turns: Vec<Value>,
    sizes: Vec<usize>,
    positions: HashMap<String, usize>,
    bytes: usize,
}
impl Deref for Transcript {
    type Target = [Value];
    fn deref(&self) -> &Self::Target {
        &self.turns
    }
}
impl Transcript {
    pub fn insert(&mut self, turn: Value) -> Result<()> {
        let id = turn["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("invalid turn identity"))?;
        let index = self.positions.get(id).copied();
        let old_size = index.map(|i| self.sizes[i]).unwrap_or(0);
        if index.is_none() && self.turns.len() >= MAX_TURNS {
            return Err(HistoryLimit.into());
        }
        let available = MAX_BYTES - (self.bytes - old_size);
        // Count JSON bytes without allocating another copy of the transcript.
        // The writer stops at the limit even for a large candidate turn.
        let mut counter = Counter {
            bytes: 0,
            available,
        };
        if serde_json::to_writer(&mut counter, &turn).is_err() {
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
    fn many_turn_limit_preserves_all_admitted_history() {
        let mut history = Transcript::default();
        for n in 0..MAX_TURNS {
            history
                .insert(json!({"id":n.to_string(),"items":[]}))
                .unwrap();
        }
        assert!(history
            .insert(json!({"id":"overflow"}))
            .unwrap_err()
            .is::<HistoryLimit>());
        assert_eq!(history.len(), MAX_TURNS);
        history
            .insert(json!({"id":"0","items":["replacement"]}))
            .unwrap();
        assert_eq!(history.len(), MAX_TURNS);
    }
}
