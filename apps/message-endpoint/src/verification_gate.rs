//! Cheap bounded verification admission. Failure hashes stay private in RAM;
//! positive identity or membership decisions are NEVER cached.
use crate::owner_auth::Error;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};
const GLOBAL: usize = 4;
const SOURCE: usize = 2;
const REQUESTS: u32 = 20;
const WINDOW: Duration = Duration::from_secs(10);
const SOURCES: usize = 512;
const NEGATIVES: usize = 512;
const NEGATIVE_TTL: Duration = Duration::from_secs(3);
pub(crate) fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}
struct Source {
    inflight: usize,
    requests: u32,
    window: Instant,
    last_seen: Instant,
}
struct Negative {
    until: Instant,
    error: Error,
}
#[derive(Default)]
struct State {
    inflight: usize,
    sources: HashMap<IpAddr, Source>,
    negative: HashMap<[u8; 32], Negative>,
}
#[derive(Default)]
pub(crate) struct Gate {
    state: Mutex<State>,
}
pub(crate) struct Permit<'a> {
    gate: &'a Gate,
    source: IpAddr,
}
impl Gate {
    pub(crate) fn enter(
        &self,
        source: IpAddr,
        token: Option<[u8; 32]>,
    ) -> Result<Permit<'_>, Error> {
        let mut state = self.state.lock().map_err(|_| Error::Proxy)?;
        let now = Instant::now();
        state.negative.retain(|_, n| n.until > now);
        if let Some(negative) = token.and_then(|key| state.negative.get(&key)) {
            return Err(negative.error.clone());
        }
        state.sources.retain(|_, s| {
            s.inflight > 0 || now.duration_since(s.last_seen) < Duration::from_secs(60)
        });
        if state.inflight >= GLOBAL
            || (!state.sources.contains_key(&source) && state.sources.len() >= SOURCES)
        {
            return Err(Error::RateLimited);
        }
        let entry = state.sources.entry(source).or_insert_with(|| Source {
            inflight: 0,
            requests: 0,
            window: now,
            last_seen: now,
        });
        if now.duration_since(entry.window) >= WINDOW {
            entry.window = now;
            entry.requests = 0;
        }
        entry.last_seen = now;
        if entry.inflight >= SOURCE || entry.requests >= REQUESTS {
            return Err(Error::RateLimited);
        }
        entry.inflight += 1;
        entry.requests += 1;
        state.inflight += 1;
        Ok(Permit { gate: self, source })
    }
    pub(crate) fn remember_failure(&self, token: [u8; 32], error: Error) {
        if !matches!(
            error,
            Error::InvalidIdentity | Error::OwnerRequired | Error::Proxy
        ) {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            let now = Instant::now();
            state.negative.retain(|_, n| n.until > now);
            if state.negative.len() >= NEGATIVES && !state.negative.contains_key(&token) {
                if let Some(key) = state
                    .negative
                    .iter()
                    .min_by_key(|(_, n)| n.until)
                    .map(|(key, _)| *key)
                {
                    state.negative.remove(&key);
                }
            }
            state.negative.insert(
                token,
                Negative {
                    until: now + NEGATIVE_TTL,
                    error,
                },
            );
        }
    }
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.gate.state.lock() {
            state.inflight = state.inflight.saturating_sub(1);
            if let Some(source) = state.sources.get_mut(&self.source) {
                source.inflight = source.inflight.saturating_sub(1);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn ip(n: u8) -> IpAddr {
        IpAddr::from([127, 0, 0, n])
    }
    #[test]
    fn global_and_source_concurrency_are_bounded_and_permits_release_on_drop() {
        let gate = Gate::default();
        let a = gate.enter(ip(1), None).unwrap();
        let b = gate.enter(ip(1), None).unwrap();
        assert!(matches!(gate.enter(ip(1), None), Err(Error::RateLimited)));
        let c = gate.enter(ip(2), None).unwrap();
        let d = gate.enter(ip(2), None).unwrap();
        assert!(matches!(gate.enter(ip(3), None), Err(Error::RateLimited)));
        drop(a);
        let e = gate.enter(ip(3), None).unwrap();
        drop((b, c, d, e));
        assert_eq!(gate.state.lock().unwrap().inflight, 0);
    }
    #[test]
    fn source_window_limits_sequential_requests_and_idle_source_capacity_is_bounded() {
        let gate = Gate::default();
        for _ in 0..20 {
            drop(gate.enter(ip(1), None).unwrap());
        }
        assert!(matches!(gate.enter(ip(1), None), Err(Error::RateLimited)));
        gate.state
            .lock()
            .unwrap()
            .sources
            .get_mut(&ip(1))
            .unwrap()
            .window = Instant::now() - Duration::from_secs(11);
        drop(gate.enter(ip(1), None).unwrap());
        for n in 2..=512u16 {
            drop(
                gate.enter(
                    IpAddr::from([10, 1, (n / 256) as u8, (n % 256) as u8]),
                    None,
                )
                .unwrap(),
            );
        }
        assert_eq!(gate.state.lock().unwrap().sources.len(), 512);
        assert!(matches!(gate.enter(ip(9), None), Err(Error::RateLimited)));
        gate.state
            .lock()
            .unwrap()
            .sources
            .get_mut(&ip(1))
            .unwrap()
            .last_seen = Instant::now() - Duration::from_secs(61);
        drop(gate.enter(ip(9), None).unwrap());
        assert_eq!(gate.state.lock().unwrap().sources.len(), 512);
    }
    #[test]
    fn failures_only_hash_cache_is_short_lived_bounded_and_never_remembers_rate_refusals() {
        let gate = Gate::default();
        let key = token_hash("synthetic.header.signature");
        gate.remember_failure(key, Error::OwnerRequired);
        assert!(matches!(
            gate.enter(ip(1), Some(key)),
            Err(Error::OwnerRequired)
        ));
        assert_eq!(gate.state.lock().unwrap().inflight, 0);
        gate.state
            .lock()
            .unwrap()
            .negative
            .get_mut(&key)
            .unwrap()
            .until = Instant::now() - Duration::from_millis(1);
        drop(gate.enter(ip(1), Some(key)).unwrap());
        gate.remember_failure(key, Error::RateLimited);
        assert!(!gate.state.lock().unwrap().negative.contains_key(&key));
        for n in 0..600 {
            gate.remember_failure(
                token_hash(&format!("synthetic.{n}.signature")),
                Error::InvalidIdentity,
            );
        }
        assert_eq!(gate.state.lock().unwrap().negative.len(), 512);
    }
}
