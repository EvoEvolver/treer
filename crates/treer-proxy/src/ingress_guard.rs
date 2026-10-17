//! Public ingress protection (#146): per-instance admission control for
//! anonymous internet traffic reaching public service ingresses.
//!
//! Everything here is in memory and per Proxy process: token buckets per
//! ingress and per verified client address, in-flight permits per ingress, a
//! global pre-resolution bucket per peer so unknown hostnames cannot turn into
//! database load, and rejection counters (counts only, never addresses or
//! paths). The verified client address is the TCP peer unless the peer is a
//! configured trusted front, in which case exactly one configured header is
//! consulted. Raw `X-Forwarded-For` from an untrusted peer is never trusted.
use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderName};
use ipnet::IpNet;
use treer_protocol::{EffectiveIngressLimits, IngressRejectionCounters};

/// Pre-resolution ceiling per peer address across all hostnames.
pub const GLOBAL_PEER_RATE_PER_MINUTE: u32 = 1200;
pub const GLOBAL_PEER_BURST: u32 = 300;
/// Idle buckets are dropped after this long.
pub const BUCKET_IDLE_TTL: Duration = Duration::from_secs(600);
/// Per-ingress budget of distinct client keys; beyond it clients share one
/// overflow bucket (fail closed on memory, never open).
pub const CLIENT_KEYS_PER_INGRESS: usize = 20_000;
pub const GLOBAL_PEER_KEYS: usize = 100_000;
/// Narrowest IPv4 prefix accepted as a trusted range without the wide flag.
const MIN_TRUSTED_IPV4_PREFIX: u8 = 8;
const MIN_TRUSTED_IPV6_PREFIX: u8 = 32;

#[derive(Debug, Clone)]
pub struct TrustedProxies {
    ranges: Vec<IpNet>,
    header: Option<HeaderName>,
}

#[derive(Debug, thiserror::Error)]
pub enum TrustedProxiesError {
    #[error("invalid trusted proxy range {0:?}")]
    InvalidRange(String),
    #[error("trusted proxy range {0} is too wide; it would trust any client's header (use --ingress-allow-wide-trust to accept)")]
    RangeTooWide(IpNet),
    #[error("invalid client IP header name {0:?}")]
    InvalidHeader(String),
    #[error("--ingress-client-ip-header requires at least one --ingress-trusted-proxies range")]
    HeaderWithoutRanges,
}

impl TrustedProxies {
    pub fn none() -> Self {
        Self {
            ranges: Vec::new(),
            header: None,
        }
    }

    /// Parses the operator configuration; refuses ranges that would trust
    /// everyone unless explicitly allowed.
    pub fn parse(
        ranges: &[String],
        header: Option<&str>,
        allow_wide: bool,
    ) -> Result<Self, TrustedProxiesError> {
        let mut parsed = Vec::new();
        for text in ranges.iter().flat_map(|item| item.split(',')) {
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let net: IpNet = text
                .parse()
                .or_else(|_| text.parse::<IpAddr>().map(IpNet::from))
                .map_err(|_| TrustedProxiesError::InvalidRange(text.to_owned()))?;
            let too_wide = match net {
                IpNet::V4(v4) => v4.prefix_len() < MIN_TRUSTED_IPV4_PREFIX,
                IpNet::V6(v6) => v6.prefix_len() < MIN_TRUSTED_IPV6_PREFIX,
            };
            if net.prefix_len() == 0 || (too_wide && !allow_wide) {
                return Err(TrustedProxiesError::RangeTooWide(net));
            }
            parsed.push(net);
        }
        let header = match header.map(str::trim).filter(|name| !name.is_empty()) {
            Some(name) => {
                if parsed.is_empty() {
                    return Err(TrustedProxiesError::HeaderWithoutRanges);
                }
                Some(
                    HeaderName::from_bytes(name.to_ascii_lowercase().as_bytes())
                        .map_err(|_| TrustedProxiesError::InvalidHeader(name.to_owned()))?,
                )
            }
            None => None,
        };
        Ok(Self {
            ranges: parsed,
            header,
        })
    }

    pub fn configured(&self) -> bool {
        !self.ranges.is_empty()
    }

    /// The front's client address header, if one is configured.
    pub fn client_ip_header(&self) -> Option<&HeaderName> {
        self.header.as_ref()
    }

    pub fn trusts(&self, address: IpAddr) -> bool {
        let address = normalize(address);
        self.ranges.iter().any(|net| net.contains(&address))
    }
}

/// IPv4-mapped IPv6 addresses are the same client as their IPv4 form.
pub fn normalize(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        other => other,
    }
}

/// Addresses that cannot be an internet client: a front or a local link.
pub fn is_private_or_local(address: IpAddr) -> bool {
    match normalize(address) {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1])
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientIpSource {
    /// The TCP peer itself.
    Peer,
    /// The configured header from a trusted front.
    TrustedHeader,
    /// A trusted front sent no usable value; the peer was used instead.
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp {
    pub address: IpAddr,
    pub source: ClientIpSource,
}

/// Resolves the client address for one request.
pub fn client_ip(trusted: &TrustedProxies, peer: IpAddr, headers: &HeaderMap) -> ClientIp {
    let peer = normalize(peer);
    let Some(header) = &trusted.header else {
        return ClientIp {
            address: peer,
            source: ClientIpSource::Peer,
        };
    };
    if !trusted.trusts(peer) {
        return ClientIp {
            address: peer,
            source: ClientIpSource::Peer,
        };
    }
    let mut values = headers.get_all(header).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return ClientIp {
            address: peer,
            source: ClientIpSource::Fallback,
        };
    };
    let Ok(text) = value.to_str() else {
        return ClientIp {
            address: peer,
            source: ClientIpSource::Fallback,
        };
    };
    let resolved = if header == "x-forwarded-for" {
        // Right to left: skip the trusted hops, the first untrusted hop is
        // the client. A list made only of trusted hops has no client.
        text.rsplit(',')
            .map(str::trim)
            .filter_map(parse_forwarded_ip)
            .find(|address| !trusted.trusts(*address))
    } else {
        let mut parts = text.split(',').map(str::trim);
        match (parts.next().and_then(parse_forwarded_ip), parts.next()) {
            (Some(address), None) => Some(address),
            _ => None,
        }
    };
    match resolved {
        Some(address) => ClientIp {
            address: normalize(address),
            source: ClientIpSource::TrustedHeader,
        },
        None => ClientIp {
            address: peer,
            source: ClientIpSource::Fallback,
        },
    }
}

fn parse_forwarded_ip(text: &str) -> Option<IpAddr> {
    if text.len() > 64 {
        return None;
    }
    let text = text.trim_matches(|c| c == '"' || c == ' ');
    if let Ok(address) = text.parse::<IpAddr>() {
        return Some(address);
    }
    // "[v6]:port" or "v4:port"
    if let Ok(socket) = text.parse::<SocketAddr>() {
        return Some(socket.ip());
    }
    text.strip_prefix('[')
        .and_then(|rest| rest.split(']').next())
        .and_then(|inner| inner.parse::<Ipv6Addr>().ok())
        .map(IpAddr::V6)
}

/// Rate keys collapse IPv6 clients to their /64.
pub fn rate_key(address: IpAddr) -> IpAddr {
    match normalize(address) {
        IpAddr::V6(v6) => {
            let mut segments = v6.segments();
            segments[4..].fill(0);
            IpAddr::V6(Ipv6Addr::from(segments))
        }
        v4 => v4,
    }
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    updated: Instant,
    last_used: Instant,
}

impl Bucket {
    fn new(burst: u32, now: Instant) -> Self {
        Self {
            tokens: f64::from(burst),
            updated: now,
            last_used: now,
        }
    }

    /// Takes one token; on refusal returns the seconds until one is available.
    fn take(&mut self, rate_per_minute: u32, burst: u32, now: Instant) -> Result<(), u64> {
        let rate_per_second = f64::from(rate_per_minute.max(1)) / 60.0;
        let elapsed = now.saturating_duration_since(self.updated).as_secs_f64();
        self.tokens = (self.tokens + elapsed * rate_per_second).min(f64::from(burst.max(1)));
        self.updated = now;
        self.last_used = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            let wait = (1.0 - self.tokens) / rate_per_second;
            Err(wait.ceil().max(1.0) as u64)
        }
    }
}

#[derive(Debug, Default)]
struct Counters {
    rate_limited: AtomicU64,
    ip_rate_limited: AtomicU64,
    concurrency_limited: AtomicU64,
    body_too_large: AtomicU64,
    body_timeout: AtomicU64,
    upstream_timeout: AtomicU64,
    request_rejected: AtomicU64,
    client_ip_fallback: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> IngressRejectionCounters {
        IngressRejectionCounters {
            rate_limited: self.rate_limited.load(Ordering::Relaxed),
            ip_rate_limited: self.ip_rate_limited.load(Ordering::Relaxed),
            concurrency_limited: self.concurrency_limited.load(Ordering::Relaxed),
            body_too_large: self.body_too_large.load(Ordering::Relaxed),
            body_timeout: self.body_timeout.load(Ordering::Relaxed),
            upstream_timeout: self.upstream_timeout.load(Ordering::Relaxed),
            request_rejected: self.request_rejected.load(Ordering::Relaxed),
            client_ip_fallback: self.client_ip_fallback.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    RateLimited,
    IpRateLimited,
    ConcurrencyLimited,
    BodyTooLarge,
    BodyTimeout,
    UpstreamTimeout,
    /// Malformed or ambiguous request framing (e.g. Content-Length together
    /// with Transfer-Encoding).
    RequestRejected,
}

#[derive(Debug)]
struct ServiceState {
    bucket: Bucket,
    in_flight: Arc<AtomicU64>,
    clients: HashMap<IpAddr, Bucket>,
    overflow: Bucket,
    counters: Arc<Counters>,
    last_warning: Option<Instant>,
    last_evict: Instant,
}

impl ServiceState {
    fn new(limits: &EffectiveIngressLimits, now: Instant) -> Self {
        Self {
            bucket: Bucket::new(limits.rate_burst, now),
            in_flight: Arc::new(AtomicU64::new(0)),
            clients: HashMap::new(),
            overflow: Bucket::new(limits.ip_rate_burst, now),
            counters: Arc::new(Counters::default()),
            last_warning: None,
            last_evict: now,
        }
    }

    /// Sweeps idle client buckets, at most once per interval: a flood of
    /// fresh addresses cannot make every request pay for a full scan.
    fn evict(&mut self, now: Instant) {
        if now.saturating_duration_since(self.last_evict) < EVICT_INTERVAL {
            return;
        }
        self.last_evict = now;
        self.clients
            .retain(|_, bucket| now.saturating_duration_since(bucket.last_used) < BUCKET_IDLE_TTL);
    }
}

/// One in-flight slot. Released once: explicitly by a lease that expires, or
/// when the last holder drops it, whichever comes first.
#[derive(Debug)]
pub struct Permit {
    in_flight: Arc<AtomicU64>,
    released: AtomicBool,
}

impl Permit {
    /// Idempotent: the slot is returned exactly once.
    pub fn release(&self) {
        if !self.released.swap(true, Ordering::AcqRel) {
            self.in_flight.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.release();
    }
}

/// Least often the per-ingress client table is swept when it is full.
const EVICT_INTERVAL: Duration = Duration::from_secs(10);

/// Outcome of a successful admission.
#[derive(Debug)]
pub struct Admission {
    pub permit: Permit,
    pub client: ClientIp,
    /// False when per-IP limiting is disabled (private peer without a
    /// trusted-front configuration); the service bucket still applies.
    pub per_ip_enforced: bool,
}

#[derive(Debug)]
pub struct Refusal {
    pub rejection: Rejection,
    /// Seconds for Retry-After (rate limits only).
    pub retry_after: Option<u64>,
}

struct Inner {
    trusted: TrustedProxies,
    services: Mutex<HashMap<String, ServiceState>>,
    peers: Mutex<HashMap<IpAddr, Bucket>>,
    peers_last_evict: Mutex<Instant>,
    peers_overflow: Mutex<Bucket>,
    private_peer_warned: AtomicBool,
    started: chrono::DateTime<chrono::Utc>,
}

/// Shared by every ingress handler; cheap to clone.
#[derive(Clone)]
pub struct IngressGuard {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for IngressGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IngressGuard")
            .field("trusted", &self.inner.trusted)
            .finish()
    }
}

impl IngressGuard {
    pub fn new(trusted: TrustedProxies) -> Self {
        let now = Instant::now();
        Self {
            inner: Arc::new(Inner {
                trusted,
                services: Mutex::new(HashMap::new()),
                peers: Mutex::new(HashMap::new()),
                peers_last_evict: Mutex::new(now),
                peers_overflow: Mutex::new(Bucket::new(GLOBAL_PEER_BURST, now)),
                private_peer_warned: AtomicBool::new(false),
                started: chrono::Utc::now(),
            }),
        }
    }

    pub fn started_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.inner.started
    }

    pub fn client_ip_header(&self) -> Option<&HeaderName> {
        self.inner.trusted.client_ip_header()
    }

    /// Admission for a hostname that is not in the ingress snapshot: the one
    /// request class that must stay cheap under a flood. Keyed by the
    /// verified client address (never the raw front address) and skipped,
    /// like the per-client limit, for a private peer without a configured
    /// front, so an unconfigured load balancer never throttles a whole site.
    pub fn admit_unknown_host(&self, peer: IpAddr, headers: &HeaderMap) -> Result<(), Refusal> {
        let client = client_ip(&self.inner.trusted, peer, headers);
        if !self.inner.trusted.configured() && is_private_or_local(client.address) {
            self.warn_private_peer_once();
            return Ok(());
        }
        let now = Instant::now();
        let key = rate_key(client.address);
        let mut peers = self.inner.peers.lock().unwrap_or_else(|p| p.into_inner());
        if peers.len() >= GLOBAL_PEER_KEYS && !peers.contains_key(&key) {
            // Sweep at most once per interval; in between, new keys share
            // the overflow bucket instead of paying for a full scan.
            let mut last_evict = self
                .inner
                .peers_last_evict
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if now.saturating_duration_since(*last_evict) >= EVICT_INTERVAL {
                *last_evict = now;
                peers.retain(|_, bucket| {
                    now.saturating_duration_since(bucket.last_used) < BUCKET_IDLE_TTL
                });
            }
            drop(last_evict);
            if peers.len() >= GLOBAL_PEER_KEYS {
                drop(peers);
                let mut overflow = self
                    .inner
                    .peers_overflow
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                return overflow
                    .take(GLOBAL_PEER_RATE_PER_MINUTE, GLOBAL_PEER_BURST, now)
                    .map_err(|retry| Refusal {
                        rejection: Rejection::RateLimited,
                        retry_after: Some(retry),
                    });
            }
        }
        peers
            .entry(key)
            .or_insert_with(|| Bucket::new(GLOBAL_PEER_BURST, now))
            .take(GLOBAL_PEER_RATE_PER_MINUTE, GLOBAL_PEER_BURST, now)
            .map_err(|retry| Refusal {
                rejection: Rejection::RateLimited,
                retry_after: Some(retry),
            })
    }

    fn warn_private_peer_once(&self) {
        if !self.inner.private_peer_warned.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                "public ingress sees a private or local peer address and no trusted front is configured: per-client rate limits are not enforced; set --ingress-trusted-proxies and --ingress-client-ip-header"
            );
        }
    }

    /// Resolves the client and admits one request to a public ingress:
    /// per-client bucket first (a noisy client cannot drain the service
    /// budget), then the service bucket, then an in-flight permit.
    pub fn admit(
        &self,
        ingress_id: &str,
        limits: &EffectiveIngressLimits,
        peer: IpAddr,
        headers: &HeaderMap,
    ) -> Result<Admission, Refusal> {
        let now = Instant::now();
        let client = client_ip(&self.inner.trusted, peer, headers);
        // Behind an unconfigured front every client would share the front's
        // address: do not punish them all for one, warn the operator instead.
        let per_ip_enforced =
            self.inner.trusted.configured() || !is_private_or_local(client.address);
        let mut services = self
            .inner
            .services
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let service = services
            .entry(ingress_id.to_owned())
            .or_insert_with(|| ServiceState::new(limits, now));
        if client.source == ClientIpSource::Fallback {
            service
                .counters
                .client_ip_fallback
                .fetch_add(1, Ordering::Relaxed);
        }
        if !per_ip_enforced {
            self.warn_private_peer_once();
        }
        if per_ip_enforced {
            let key = rate_key(client.address);
            if service.clients.len() >= CLIENT_KEYS_PER_INGRESS
                && !service.clients.contains_key(&key)
            {
                service.evict(now);
            }
            let bucket = if service.clients.len() >= CLIENT_KEYS_PER_INGRESS
                && !service.clients.contains_key(&key)
            {
                &mut service.overflow
            } else {
                service
                    .clients
                    .entry(key)
                    .or_insert_with(|| Bucket::new(limits.ip_rate_burst, now))
            };
            if let Err(retry) =
                bucket.take(limits.ip_rate_limit_per_minute, limits.ip_rate_burst, now)
            {
                service
                    .counters
                    .ip_rate_limited
                    .fetch_add(1, Ordering::Relaxed);
                return Err(Refusal {
                    rejection: Rejection::IpRateLimited,
                    retry_after: Some(retry),
                });
            }
        }
        if let Err(retry) =
            service
                .bucket
                .take(limits.rate_limit_per_minute, limits.rate_burst, now)
        {
            service
                .counters
                .rate_limited
                .fetch_add(1, Ordering::Relaxed);
            Self::maybe_warn(service, ingress_id, now);
            return Err(Refusal {
                rejection: Rejection::RateLimited,
                retry_after: Some(retry),
            });
        }
        let in_flight = service.in_flight.clone();
        let current = in_flight.fetch_add(1, Ordering::AcqRel) + 1;
        if current > u64::from(limits.concurrency_limit) {
            in_flight.fetch_sub(1, Ordering::AcqRel);
            service
                .counters
                .concurrency_limited
                .fetch_add(1, Ordering::Relaxed);
            Self::maybe_warn(service, ingress_id, now);
            return Err(Refusal {
                rejection: Rejection::ConcurrencyLimited,
                retry_after: Some(1),
            });
        }
        Ok(Admission {
            permit: Permit {
                in_flight,
                released: AtomicBool::new(false),
            },
            client,
            per_ip_enforced,
        })
    }

    fn maybe_warn(service: &mut ServiceState, ingress_id: &str, now: Instant) {
        let due = service
            .last_warning
            .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_secs(60));
        if due {
            service.last_warning = Some(now);
            let counters = service.counters.snapshot();
            tracing::warn!(
                ingress_id,
                rate_limited = counters.rate_limited,
                ip_rate_limited = counters.ip_rate_limited,
                concurrency_limited = counters.concurrency_limited,
                "public ingress is rejecting requests"
            );
        }
    }

    /// Records a rejection decided outside `admit` (body, timeouts, framing).
    pub fn record(&self, ingress_id: &str, rejection: Rejection) {
        let mut services = self
            .inner
            .services
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let service = services.entry(ingress_id.to_owned()).or_insert_with(|| {
            ServiceState::new(&EffectiveIngressLimits::default(), Instant::now())
        });
        let counter = match rejection {
            Rejection::RateLimited => &service.counters.rate_limited,
            Rejection::IpRateLimited => &service.counters.ip_rate_limited,
            Rejection::ConcurrencyLimited => &service.counters.concurrency_limited,
            Rejection::BodyTooLarge => &service.counters.body_too_large,
            Rejection::BodyTimeout => &service.counters.body_timeout,
            Rejection::UpstreamTimeout => &service.counters.upstream_timeout,
            Rejection::RequestRejected => &service.counters.request_rejected,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn counters(&self, ingress_id: &str) -> IngressRejectionCounters {
        self.inner
            .services
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(ingress_id)
            .map(|service| service.counters.snapshot())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn in_flight(&self, ingress_id: &str) -> u64 {
        self.inner
            .services
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(ingress_id)
            .map(|service| service.in_flight.load(Ordering::Acquire))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn make_limits(
        rate: u32,
        burst: u32,
        concurrency: u32,
        ip_rate: u32,
        ip_burst: u32,
    ) -> EffectiveIngressLimits {
        EffectiveIngressLimits {
            rate_limit_per_minute: rate,
            rate_burst: burst,
            concurrency_limit: concurrency,
            ip_rate_limit_per_minute: ip_rate,
            ip_rate_burst: ip_burst,
            ..EffectiveIngressLimits::default()
        }
    }

    fn public(n: u8) -> IpAddr {
        IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, n))
    }

    #[test]
    fn token_bucket_refills_and_reports_retry_after() {
        let start = Instant::now();
        let mut bucket = Bucket::new(2, start);
        assert!(bucket.take(60, 2, start).is_ok());
        assert!(bucket.take(60, 2, start).is_ok());
        assert_eq!(bucket.take(60, 2, start), Err(1));
        // One token per second at 60/min: after 1.5 s one request passes.
        let later = start + Duration::from_millis(1500);
        assert!(bucket.take(60, 2, later).is_ok());
        assert!(bucket.take(60, 2, later).is_err());
        // Never above the burst, however long the idle period.
        let much_later = start + Duration::from_secs(3600);
        assert!(bucket.take(60, 2, much_later).is_ok());
        assert!(bucket.take(60, 2, much_later).is_ok());
        assert!(bucket.take(60, 2, much_later).is_err());
        // Slow rates report the real wait, rounded up, never zero.
        let mut slow = Bucket::new(1, start);
        assert!(slow.take(6, 1, start).is_ok());
        assert_eq!(slow.take(6, 1, start), Err(10));
    }

    #[test]
    fn trusted_proxy_configuration_is_strict() {
        assert!(TrustedProxies::parse(&[], None, false)
            .unwrap()
            .ranges
            .is_empty());
        let ok = TrustedProxies::parse(
            &["10.0.0.0/8, 192.168.1.5".into(), "2001:db8::/32".into()],
            Some("CF-Connecting-IP"),
            false,
        )
        .unwrap();
        assert_eq!(ok.ranges.len(), 3);
        assert_eq!(ok.header.as_ref().unwrap().as_str(), "cf-connecting-ip");
        for wide in ["0.0.0.0/0", "::/0", "10.0.0.0/7", "2001:db8::/31"] {
            assert!(
                matches!(
                    TrustedProxies::parse(&[wide.into()], None, false),
                    Err(TrustedProxiesError::RangeTooWide(_))
                ),
                "{wide}"
            );
        }
        assert!(TrustedProxies::parse(&["10.0.0.0/7".into()], None, true).is_ok());
        assert!(
            matches!(
                TrustedProxies::parse(&["0.0.0.0/0".into()], None, true),
                Err(TrustedProxiesError::RangeTooWide(_))
            ),
            "/0 is never accepted"
        );
        assert!(matches!(
            TrustedProxies::parse(&[], Some("X-Real-IP"), false),
            Err(TrustedProxiesError::HeaderWithoutRanges)
        ));
        assert!(matches!(
            TrustedProxies::parse(&["nonsense".into()], None, false),
            Err(TrustedProxiesError::InvalidRange(_))
        ));
        assert!(matches!(
            TrustedProxies::parse(&["10.0.0.0/8".into()], Some("bad header"), false),
            Err(TrustedProxiesError::InvalidHeader(_))
        ));
    }

    #[test]
    fn client_ip_trusts_only_the_configured_header_from_trusted_peers() {
        let trusted =
            TrustedProxies::parse(&["10.0.0.0/8".into()], Some("X-Forwarded-For"), false).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.7, 203.0.113.9, 10.1.1.1"),
        );
        // Untrusted peer: the header is ignored.
        let direct = client_ip(&trusted, public(5), &headers);
        assert_eq!(direct.address, public(5));
        assert_eq!(direct.source, ClientIpSource::Peer);
        // Trusted peer: walk right to left skipping trusted hops.
        let behind = client_ip(&trusted, "10.0.0.2".parse().unwrap(), &headers);
        assert_eq!(behind.address, "203.0.113.9".parse::<IpAddr>().unwrap());
        assert_eq!(behind.source, ClientIpSource::TrustedHeader);
        // Only trusted hops listed: fall back to the peer, counted.
        headers.insert("x-forwarded-for", HeaderValue::from_static("10.9.9.9"));
        let fallback = client_ip(&trusted, "10.0.0.2".parse().unwrap(), &headers);
        assert_eq!(fallback.source, ClientIpSource::Fallback);
        assert_eq!(fallback.address, "10.0.0.2".parse::<IpAddr>().unwrap());
        // Missing or garbage header: fallback as well.
        headers.remove("x-forwarded-for");
        assert_eq!(
            client_ip(&trusted, "10.0.0.2".parse().unwrap(), &headers).source,
            ClientIpSource::Fallback
        );
        headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        assert_eq!(
            client_ip(&trusted, "10.0.0.2".parse().unwrap(), &headers).source,
            ClientIpSource::Fallback
        );
        // Duplicate header lines are ambiguous: fallback.
        headers.append("x-forwarded-for", HeaderValue::from_static("203.0.113.1"));
        assert_eq!(
            client_ip(&trusted, "10.0.0.2".parse().unwrap(), &headers).source,
            ClientIpSource::Fallback
        );

        // Single-value header: exactly one address, with port forms accepted.
        let cf =
            TrustedProxies::parse(&["10.0.0.0/8".into()], Some("CF-Connecting-IP"), false).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "cf-connecting-ip",
            HeaderValue::from_static("[2001:db8::1]:443"),
        );
        let v6 = client_ip(&cf, "10.0.0.2".parse().unwrap(), &headers);
        assert_eq!(v6.address, "2001:db8::1".parse::<IpAddr>().unwrap());
        headers.insert(
            "cf-connecting-ip",
            HeaderValue::from_static("1.2.3.4, 5.6.7.8"),
        );
        assert_eq!(
            client_ip(&cf, "10.0.0.2".parse().unwrap(), &headers).source,
            ClientIpSource::Fallback,
            "a list in a single-value header is not trusted"
        );
        // IPv4-mapped IPv6 normalises to IPv4 for trust and keys.
        let mapped: IpAddr = "::ffff:10.0.0.2".parse().unwrap();
        headers.insert(
            "cf-connecting-ip",
            HeaderValue::from_static("::ffff:203.0.113.4"),
        );
        let resolved = client_ip(&cf, mapped, &headers);
        assert_eq!(resolved.address, public(4));
        assert_eq!(resolved.source, ClientIpSource::TrustedHeader);
        assert_eq!(
            rate_key("2001:db8:1:2:3:4:5:6".parse().unwrap()),
            "2001:db8:1:2::".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn admission_checks_client_before_service_and_releases_permits_on_drop() {
        let guard = IngressGuard::new(TrustedProxies::none());
        let limits = make_limits(600, 10, 2, 60, 2);
        let headers = HeaderMap::new();
        // Client A exhausts its own burst; the service bucket is untouched
        // for client B.
        for _ in 0..2 {
            guard.admit("ing_a", &limits, public(1), &headers).unwrap();
        }
        let refused = guard
            .admit("ing_a", &limits, public(1), &headers)
            .unwrap_err();
        assert_eq!(refused.rejection, Rejection::IpRateLimited);
        assert!(refused.retry_after.is_some_and(|secs| secs >= 1));
        assert!(guard.admit("ing_a", &limits, public(2), &headers).is_ok());
        assert_eq!(guard.counters("ing_a").ip_rate_limited, 1);
        assert_eq!(guard.counters("ing_a").rate_limited, 0);

        // Concurrency: permits are counted while alive and freed on drop.
        let guard2 = IngressGuard::new(TrustedProxies::none());
        let a = guard2.admit("ing_b", &limits, public(1), &headers).unwrap();
        let b = guard2.admit("ing_b", &limits, public(2), &headers).unwrap();
        assert_eq!(guard2.in_flight("ing_b"), 2);
        let refused = guard2
            .admit("ing_b", &limits, public(3), &headers)
            .unwrap_err();
        assert_eq!(refused.rejection, Rejection::ConcurrencyLimited);
        assert_eq!(
            guard2.in_flight("ing_b"),
            2,
            "a refused request holds nothing"
        );
        drop(a);
        assert_eq!(guard2.in_flight("ing_b"), 1);
        assert!(guard2.admit("ing_b", &limits, public(3), &headers).is_ok());
        drop(b);
        assert_eq!(guard2.counters("ing_b").concurrency_limited, 1);

        // Service bucket as the backstop: many clients, small service burst.
        let guard3 = IngressGuard::new(TrustedProxies::none());
        let small = make_limits(600, 3, 100, 600, 100);
        for n in 1..=3 {
            guard3.admit("ing_c", &small, public(n), &headers).unwrap();
        }
        assert_eq!(
            guard3
                .admit("ing_c", &small, public(4), &headers)
                .unwrap_err()
                .rejection,
            Rejection::RateLimited
        );
    }

    #[test]
    fn private_peer_without_trusted_front_disables_only_the_client_limit() {
        let guard = IngressGuard::new(TrustedProxies::none());
        let limits = make_limits(600, 100, 100, 60, 1);
        let headers = HeaderMap::new();
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        for _ in 0..5 {
            let admission = guard.admit("ing_p", &limits, loopback, &headers).unwrap();
            assert!(!admission.per_ip_enforced);
        }
        assert_eq!(guard.counters("ing_p").ip_rate_limited, 0);
        // With a trusted front configured the same peer is limited per client.
        let trusted =
            TrustedProxies::parse(&["10.0.0.0/8".into()], Some("X-Real-IP"), false).unwrap();
        let guard = IngressGuard::new(trusted);
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("203.0.113.8"));
        let first = guard
            .admit("ing_q", &limits, "10.0.0.1".parse().unwrap(), &headers)
            .unwrap();
        assert!(first.per_ip_enforced);
        assert_eq!(first.client.address, public(8));
        assert_eq!(
            guard
                .admit("ing_q", &limits, "10.0.0.1".parse().unwrap(), &headers)
                .unwrap_err()
                .rejection,
            Rejection::IpRateLimited
        );
    }

    #[test]
    fn unknown_host_admission_is_keyed_by_client_and_skips_unconfigured_fronts() {
        let guard = IngressGuard::new(TrustedProxies::none());
        let headers = HeaderMap::new();
        for _ in 0..GLOBAL_PEER_BURST {
            guard.admit_unknown_host(public(9), &headers).unwrap();
        }
        let refused = guard.admit_unknown_host(public(9), &headers).unwrap_err();
        assert_eq!(refused.rejection, Rejection::RateLimited);
        assert!(
            guard.admit_unknown_host(public(10), &headers).is_ok(),
            "another peer is unaffected"
        );
        // A private peer with no configured front is never limited here:
        // behind an unconfigured load balancer it would be the whole site.
        let lb: IpAddr = "10.0.0.7".parse().unwrap();
        for _ in 0..(GLOBAL_PEER_BURST * 2) {
            guard.admit_unknown_host(lb, &headers).unwrap();
        }
        // With a configured front the key is the client the front names.
        let trusted =
            TrustedProxies::parse(&["10.0.0.0/8".into()], Some("X-Real-IP"), false).unwrap();
        let guard = IngressGuard::new(trusted);
        let mut a = HeaderMap::new();
        a.insert("x-real-ip", HeaderValue::from_static("203.0.113.1"));
        let mut b = HeaderMap::new();
        b.insert("x-real-ip", HeaderValue::from_static("203.0.113.2"));
        for _ in 0..GLOBAL_PEER_BURST {
            guard.admit_unknown_host(lb, &a).unwrap();
        }
        assert!(guard.admit_unknown_host(lb, &a).is_err());
        assert!(
            guard.admit_unknown_host(lb, &b).is_ok(),
            "clients behind the front are independent"
        );
    }

    #[test]
    fn unknown_host_table_sweeps_are_throttled_too() {
        let guard = IngressGuard::new(TrustedProxies::none());
        let headers = HeaderMap::new();
        for n in 0..GLOBAL_PEER_KEYS {
            let address: IpAddr = format!("2001:db8:{:x}:{:x}::1", n / 65536, n % 65536)
                .parse()
                .unwrap();
            guard.admit_unknown_host(address, &headers).unwrap();
        }
        let before = std::time::Instant::now();
        for n in 0..2000 {
            let address: IpAddr = format!("2001:db8:ffff:{n:x}::1").parse().unwrap();
            let _ = guard.admit_unknown_host(address, &headers);
        }
        assert!(
            before.elapsed() < Duration::from_secs(2),
            "{:?}",
            before.elapsed()
        );
        assert_eq!(guard.inner.peers.lock().unwrap().len(), GLOBAL_PEER_KEYS);
    }

    #[test]
    fn permits_release_once_through_lease_or_drop() {
        let guard = IngressGuard::new(TrustedProxies::none());
        let limits = make_limits(600, 10, 4, 600, 10);
        let headers = HeaderMap::new();
        let admission = guard.admit("ing_l", &limits, public(1), &headers).unwrap();
        assert_eq!(guard.in_flight("ing_l"), 1);
        admission.permit.release();
        assert_eq!(guard.in_flight("ing_l"), 0);
        admission.permit.release();
        drop(admission);
        assert_eq!(guard.in_flight("ing_l"), 0, "never below zero, never twice");
    }

    #[test]
    fn client_table_sweeps_are_throttled() {
        let guard = IngressGuard::new(TrustedProxies::none());
        let limits = make_limits(60000, 60000, 60000, 60000, 60000);
        let headers = HeaderMap::new();
        // Fill the per-ingress key budget with distinct IPv6 /64 clients.
        for n in 0..CLIENT_KEYS_PER_INGRESS {
            let address: IpAddr = format!("2001:db8:{:x}:{:x}::1", n / 65536, n % 65536)
                .parse()
                .unwrap();
            guard.admit("ing_s", &limits, address, &headers).unwrap();
        }
        // New keys share the overflow bucket; admission still works, the
        // table does not grow and no request pays for a full sweep.
        let before = std::time::Instant::now();
        for n in 0..2000 {
            let address: IpAddr = format!("2001:db8:ffff:{n:x}::1").parse().unwrap();
            guard.admit("ing_s", &limits, address, &headers).unwrap();
        }
        assert!(
            before.elapsed() < Duration::from_secs(2),
            "{:?}",
            before.elapsed()
        );
        let services = guard.inner.services.lock().unwrap();
        assert_eq!(services["ing_s"].clients.len(), CLIENT_KEYS_PER_INGRESS);
    }
}
