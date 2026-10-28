//! Transport-peer pre-auth budget, never an authorization decision or forwarded IP.
use crate::Error;
use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Hash, Eq, PartialEq)]
pub(super) enum Lane {
    External,
    Owner,
    Containment,
}
#[derive(Default)]
pub(super) struct Budget {
    peers: HashMap<(IpAddr, Lane), (Instant, u32)>,
}
impl Budget {
    pub fn check(&mut self, peer: Option<IpAddr>, lane: Lane) -> Result<(), Error> {
        self.check_at(peer.ok_or(Error::Denied)?, lane, Instant::now())
    }
    fn check_at(&mut self, peer: IpAddr, lane: Lane, now: Instant) -> Result<(), Error> {
        // One-second fixed windows; bounded memory even with spoofed/rotating peers.
        self.peers
            .retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(1));
        let key = (peer, lane);
        if !self.peers.contains_key(&key)
            && self.peers.keys().filter(|(_, l)| *l == lane).count() >= 128
        {
            return Err(Error::Capacity);
        }
        let (_, count) = self.peers.entry(key).or_insert((now, 0));
        let max = if lane == Lane::External { 64 } else { 16 };
        if *count >= max {
            return Err(Error::Capacity);
        }
        *count += 1;
        Ok(())
    }
}
pub(super) fn bearer(header: Option<&str>) -> Result<&str, Error> {
    let token = header
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(Error::Denied)?;
    let b = token.as_bytes();
    if b.len() != 102
        || !b.starts_with(b"ext1_")
        || b[37] != b'.'
        || !b[5..37]
            .iter()
            .chain(&b[38..])
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(v))
    {
        return Err(Error::Denied);
    }
    Ok(token)
}
pub(super) fn assertion(header: Option<&str>) -> Result<&str, Error> {
    header
        .filter(|s| !s.is_empty() && s.len() <= 8192 && s.bytes().all(|b| b.is_ascii_graphic()))
        .ok_or(Error::Denied)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peer_budget_is_bounded_expires_and_separates_containment() {
        let mut b = Budget::default();
        let now = Instant::now();
        let peer = "127.0.0.1".parse().unwrap();
        for _ in 0..16 {
            b.check_at(peer, Lane::Owner, now).unwrap();
        }
        assert!(matches!(
            b.check_at(peer, Lane::Owner, now),
            Err(Error::Capacity)
        ));
        b.check_at(peer, Lane::Containment, now).unwrap();
        for i in 1..128 {
            b.check_at(
                IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, i)),
                Lane::Owner,
                now,
            )
            .unwrap();
        }
        assert_eq!(b.peers.len(), 129);
        assert!(b
            .check_at("192.0.2.254".parse().unwrap(), Lane::Owner, now)
            .is_err());
        b.check_at("192.0.2.254".parse().unwrap(), Lane::Containment, now)
            .unwrap();
        b.check_at(peer, Lane::Owner, now + Duration::from_secs(1))
            .unwrap();
        assert_eq!(b.peers.len(), 1);
        assert!(b.check(None, Lane::External).is_err());
    }
}
