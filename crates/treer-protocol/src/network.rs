use super::*;

fn default_network_target_host() -> String {
    "127.0.0.1".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NetworkBinaryKind {
    Open = 1,
    Opened = 2,
    Data = 3,
    WindowUpdate = 4,
    HalfClose = 5,
    Reset = 6,
    Direct = 7,
    Usage = 8,
    /// Opens a datagram association; each Data frame is exactly one datagram.
    OpenDatagram = 9,
    UsageAck = 10,
}

impl TryFrom<u8> for NetworkBinaryKind {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Open),
            2 => Ok(Self::Opened),
            3 => Ok(Self::Data),
            4 => Ok(Self::WindowUpdate),
            5 => Ok(Self::HalfClose),
            6 => Ok(Self::Reset),
            7 => Ok(Self::Direct),
            8 => Ok(Self::Usage),
            9 => Ok(Self::OpenDatagram),
            10 => Ok(Self::UsageAck),
            _ => Err(ProtocolError::new(
                "invalid_network_frame",
                format!("unknown network binary frame kind {value}"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkBinaryFrame {
    pub kind: NetworkBinaryKind,
    pub stream_id: String,
    pub payload: Vec<u8>,
}

impl NetworkBinaryFrame {
    pub fn is_network_frame(encoded: &[u8]) -> bool {
        encoded.starts_with(NETWORK_BINARY_MAGIC)
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let stream = self.stream_id.as_bytes();
        let stream_len = u16::try_from(stream.len()).map_err(|_| {
            ProtocolError::new("invalid_network_frame", "network stream id is too long")
        })?;
        if stream.is_empty() {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "network stream id is empty",
            ));
        }
        if stream.len() > MAX_NETWORK_STREAM_ID_BYTES
            || self.payload.len() > MAX_NETWORK_PAYLOAD_BYTES
        {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "network frame exceeds its size limit",
            ));
        }
        let mut encoded =
            Vec::with_capacity(NETWORK_BINARY_HEADER_LEN + stream.len() + self.payload.len());
        encoded.extend_from_slice(NETWORK_BINARY_MAGIC);
        encoded.push(NETWORK_BINARY_VERSION);
        encoded.push(self.kind as u8);
        encoded.extend_from_slice(&stream_len.to_be_bytes());
        encoded.extend_from_slice(stream);
        encoded.extend_from_slice(&self.payload);
        Ok(encoded)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self, ProtocolError> {
        if encoded.len() < NETWORK_BINARY_HEADER_LEN {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "network binary frame is shorter than its header",
            ));
        }
        if !Self::is_network_frame(encoded) {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "network binary frame has an invalid magic value",
            ));
        }
        if encoded[3] != NETWORK_BINARY_VERSION {
            return Err(ProtocolError::new(
                "network_binary_version_mismatch",
                format!(
                    "network binary frame uses version {}, expected {}",
                    encoded[3], NETWORK_BINARY_VERSION
                ),
            ));
        }
        let kind = NetworkBinaryKind::try_from(encoded[4])?;
        let stream_len = usize::from(u16::from_be_bytes([encoded[5], encoded[6]]));
        let payload_offset = NETWORK_BINARY_HEADER_LEN
            .checked_add(stream_len)
            .filter(|offset| *offset <= encoded.len())
            .ok_or_else(|| {
                ProtocolError::new(
                    "invalid_network_frame",
                    "network stream id exceeds the binary frame",
                )
            })?;
        if stream_len == 0 {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "network stream id is empty",
            ));
        }
        if stream_len > MAX_NETWORK_STREAM_ID_BYTES
            || encoded.len().saturating_sub(payload_offset) > MAX_NETWORK_PAYLOAD_BYTES
        {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "network frame exceeds its size limit",
            ));
        }
        let stream_id = std::str::from_utf8(&encoded[7..payload_offset])
            .map_err(|_| {
                ProtocolError::new("invalid_network_frame", "network stream id is not UTF-8")
            })?
            .to_string();
        Ok(Self {
            kind,
            stream_id,
            payload: encoded[payload_offset..].to_vec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkOpenRequest {
    pub destination: String,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_id: Option<String>,
    /// The source sends Reset after full completion, including Direct routes.
    /// Older Controllers omit this and cannot provide Direct lifetime tracking.
    #[serde(default)]
    pub track_lifetime: bool,
    /// Negotiates persisted usage reports and commit acknowledgements.
    #[serde(default)]
    pub durable_usage: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkConnectRequest {
    pub source_server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_agent_id: Option<String>,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkDirectTarget {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub report_usage: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_ticket: Option<String>,
}

/// Cumulative application bytes successfully written to TCP sockets or UDP
/// datagrams. Chunks count successful writes/datagrams, not IP packets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkUsageTotals {
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub sent_chunks: u64,
    pub received_chunks: u64,
}

/// A Proxy-issued, durable authorization receipt binds late/replayed usage to
/// its original workspace, machine, Agent and destination. It does not permit
/// reopening a connection after Policy revocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkUsageReport {
    pub ticket: String,
    pub totals: NetworkUsageTotals,
    #[serde(default)]
    pub finished: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineServiceProtocol {
    Tcp,
    Http,
    Udp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineService {
    pub service_id: String,
    pub workspace_id: String,
    pub name: String,
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_agent_id: Option<String>,
    /// The owning Agent's primary HTTP communication endpoint.
    #[serde(default)]
    pub primary: bool,
    pub target_host: String,
    pub target_port: u16,
    pub protocol: MachineServiceProtocol,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateMachineServiceRequest {
    pub name: String,
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_agent_id: Option<String>,
    #[serde(default)]
    pub primary: bool,
    #[serde(default = "default_network_target_host")]
    pub target_host: String,
    pub target_port: u16,
    #[serde(default = "default_machine_service_protocol")]
    pub protocol: MachineServiceProtocol,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateMachineServiceRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<MachineServiceProtocol>,
}

const fn default_machine_service_protocol() -> MachineServiceProtocol {
    MachineServiceProtocol::Tcp
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualNetworkHost {
    pub workspace_id: String,
    pub hostname: String,
    pub service_id: String,
    pub service_protocol: MachineServiceProtocol,
    pub destination_server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_agent_id: Option<String>,
    pub target_host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_port: Option<u16>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualNetworkHostsSnapshot {
    pub workspace_id: String,
    pub revision: u64,
    pub hosts: Vec<VirtualNetworkHost>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateVirtualNetworkHostRequest {
    pub hostname: String,
    pub service_id: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceIngressAccess {
    Public,
    #[default]
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceIngress {
    pub ingress_id: String,
    pub workspace_id: String,
    pub service_id: String,
    pub hostname: String,
    pub access: ServiceIngressAccess,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
    /// Owner overrides for the public ingress protection (#146); unset
    /// fields use the Proxy defaults.
    #[serde(default)]
    pub limits: ServiceIngressLimits,
}

/// Advertised by the Proxy health document and `/api/auth/config` once public
/// ingress is rate-limited, body-capped, time-limited and forwards the
/// verified client IP.
pub const CAPABILITY_INGRESS_PROTECTION: &str = "ingress.protection.v1";
/// Set by the Proxy on every forwarded public-ingress request after client
/// `x-treer-*` headers are stripped; Apps trust only this header for the
/// client address.
pub const TREER_CLIENT_IP_HEADER: &str = "x-treer-client-ip";

pub const INGRESS_DEFAULT_RATE_PER_MINUTE: u32 = 600;
pub const INGRESS_DEFAULT_RATE_BURST: u32 = 120;
pub const INGRESS_DEFAULT_CONCURRENCY: u32 = 64;
pub const INGRESS_DEFAULT_IP_RATE_PER_MINUTE: u32 = 120;
pub const INGRESS_DEFAULT_IP_RATE_BURST: u32 = 30;
pub const INGRESS_DEFAULT_MAX_BODY_BYTES: u64 = 1024 * 1024;
pub const INGRESS_DEFAULT_RESPONSE_TIMEOUT_SECS: u32 = 35;
/// Hard ceilings an owner override cannot exceed: protection can be loosened,
/// never switched off.
pub const INGRESS_MAX_RATE_PER_MINUTE: u32 = 60_000;
pub const INGRESS_MAX_CONCURRENCY: u32 = 4096;
pub const INGRESS_MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;
pub const INGRESS_MIN_RESPONSE_TIMEOUT_SECS: u32 = 5;
pub const INGRESS_MAX_RESPONSE_TIMEOUT_SECS: u32 = 600;

/// Stored overrides; `None` means "Proxy default".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceIngressLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_minute: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_burst: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency_limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_rate_limit_per_minute: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_rate_burst: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_body_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_timeout_secs: Option<u32>,
}

/// The limits actually enforced for one ingress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveIngressLimits {
    pub rate_limit_per_minute: u32,
    pub rate_burst: u32,
    pub concurrency_limit: u32,
    pub ip_rate_limit_per_minute: u32,
    pub ip_rate_burst: u32,
    pub max_body_bytes: u64,
    pub response_timeout_secs: u32,
}

impl Default for EffectiveIngressLimits {
    fn default() -> Self {
        Self {
            rate_limit_per_minute: INGRESS_DEFAULT_RATE_PER_MINUTE,
            rate_burst: INGRESS_DEFAULT_RATE_BURST,
            concurrency_limit: INGRESS_DEFAULT_CONCURRENCY,
            ip_rate_limit_per_minute: INGRESS_DEFAULT_IP_RATE_PER_MINUTE,
            ip_rate_burst: INGRESS_DEFAULT_IP_RATE_BURST,
            max_body_bytes: INGRESS_DEFAULT_MAX_BODY_BYTES,
            response_timeout_secs: INGRESS_DEFAULT_RESPONSE_TIMEOUT_SECS,
        }
    }
}

impl ServiceIngressLimits {
    pub fn effective(&self) -> EffectiveIngressLimits {
        let defaults = EffectiveIngressLimits::default();
        EffectiveIngressLimits {
            rate_limit_per_minute: self
                .rate_limit_per_minute
                .unwrap_or(defaults.rate_limit_per_minute),
            rate_burst: self.rate_burst.unwrap_or(defaults.rate_burst),
            concurrency_limit: self.concurrency_limit.unwrap_or(defaults.concurrency_limit),
            ip_rate_limit_per_minute: self
                .ip_rate_limit_per_minute
                .unwrap_or(defaults.ip_rate_limit_per_minute),
            ip_rate_burst: self.ip_rate_burst.unwrap_or(defaults.ip_rate_burst),
            max_body_bytes: self.max_body_bytes.unwrap_or(defaults.max_body_bytes),
            response_timeout_secs: self
                .response_timeout_secs
                .unwrap_or(defaults.response_timeout_secs),
        }
    }

    /// Every set value must be positive and within the hard ceilings.
    pub fn validate(&self) -> Result<(), &'static str> {
        for (value, max, name) in [
            (
                self.rate_limit_per_minute,
                INGRESS_MAX_RATE_PER_MINUTE,
                "rate_limit_per_minute",
            ),
            (self.rate_burst, INGRESS_MAX_RATE_PER_MINUTE, "rate_burst"),
            (
                self.concurrency_limit,
                INGRESS_MAX_CONCURRENCY,
                "concurrency_limit",
            ),
            (
                self.ip_rate_limit_per_minute,
                INGRESS_MAX_RATE_PER_MINUTE,
                "ip_rate_limit_per_minute",
            ),
            (
                self.ip_rate_burst,
                INGRESS_MAX_RATE_PER_MINUTE,
                "ip_rate_burst",
            ),
        ] {
            if let Some(value) = value {
                if value == 0 || value > max {
                    return Err(name);
                }
            }
        }
        if let Some(bytes) = self.max_body_bytes {
            if bytes == 0 || bytes > INGRESS_MAX_BODY_BYTES {
                return Err("max_body_bytes");
            }
        }
        if let Some(secs) = self.response_timeout_secs {
            if !(INGRESS_MIN_RESPONSE_TIMEOUT_SECS..=INGRESS_MAX_RESPONSE_TIMEOUT_SECS)
                .contains(&secs)
            {
                return Err("response_timeout_secs");
            }
        }
        Ok(())
    }
}

fn nullable_override<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// Owner patch: a present field sets the override, an explicit `null` clears
/// it back to the default, an absent field leaves it alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateIngressLimitsRequest {
    #[serde(
        default,
        deserialize_with = "nullable_override",
        skip_serializing_if = "Option::is_none"
    )]
    pub rate_limit_per_minute: Option<Option<u32>>,
    #[serde(
        default,
        deserialize_with = "nullable_override",
        skip_serializing_if = "Option::is_none"
    )]
    pub rate_burst: Option<Option<u32>>,
    #[serde(
        default,
        deserialize_with = "nullable_override",
        skip_serializing_if = "Option::is_none"
    )]
    pub concurrency_limit: Option<Option<u32>>,
    #[serde(
        default,
        deserialize_with = "nullable_override",
        skip_serializing_if = "Option::is_none"
    )]
    pub ip_rate_limit_per_minute: Option<Option<u32>>,
    #[serde(
        default,
        deserialize_with = "nullable_override",
        skip_serializing_if = "Option::is_none"
    )]
    pub ip_rate_burst: Option<Option<u32>>,
    #[serde(
        default,
        deserialize_with = "nullable_override",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_body_bytes: Option<Option<u64>>,
    #[serde(
        default,
        deserialize_with = "nullable_override",
        skip_serializing_if = "Option::is_none"
    )]
    pub response_timeout_secs: Option<Option<u32>>,
}

impl UpdateIngressLimitsRequest {
    pub fn apply(&self, limits: &mut ServiceIngressLimits) {
        if let Some(value) = self.rate_limit_per_minute {
            limits.rate_limit_per_minute = value;
        }
        if let Some(value) = self.rate_burst {
            limits.rate_burst = value;
        }
        if let Some(value) = self.concurrency_limit {
            limits.concurrency_limit = value;
        }
        if let Some(value) = self.ip_rate_limit_per_minute {
            limits.ip_rate_limit_per_minute = value;
        }
        if let Some(value) = self.ip_rate_burst {
            limits.ip_rate_burst = value;
        }
        if let Some(value) = self.max_body_bytes {
            limits.max_body_bytes = value;
        }
        if let Some(value) = self.response_timeout_secs {
            limits.response_timeout_secs = value;
        }
    }
}

/// Per-ingress rejection counters of one Proxy instance since its start.
/// Counts only: no addresses, paths or payloads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngressRejectionCounters {
    pub rate_limited: u64,
    pub ip_rate_limited: u64,
    pub concurrency_limited: u64,
    pub body_too_large: u64,
    pub body_timeout: u64,
    pub upstream_timeout: u64,
    pub request_rejected: u64,
    /// A trusted front did not supply a usable client address and the peer
    /// address was used instead: a misconfigured front, not an attack.
    pub client_ip_fallback: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateServiceIngressRequest {
    pub service_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default)]
    pub access: ServiceIngressAccess,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateServiceIngressRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<ServiceIngressAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformTrafficRecord {
    pub window_start: DateTime<Utc>,
    pub traffic_class: String,
    pub payload_bytes: u64,
    pub payload_frames: u64,
    pub billable_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTrafficRecord {
    pub window_start: DateTime<Utc>,
    pub traffic_class: String,
    pub source_type: String,
    pub source_server_id: String,
    pub destination_type: String,
    pub destination_server_id: String,
    pub payload_bytes: u64,
    pub payload_frames: u64,
    pub billable_bytes: u64,
    pub meter_version: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTrafficRecord {
    pub window_start: DateTime<Utc>,
    pub traffic_class: String,
    pub source_type: String,
    pub source_id: String,
    pub destination_type: String,
    pub destination_id: String,
    pub payload_bytes: u64,
    pub payload_frames: u64,
    pub billable_bytes: u64,
    pub meter_version: u16,
}

impl From<MachineTrafficRecord> for AgentTrafficRecord {
    fn from(value: MachineTrafficRecord) -> Self {
        Self {
            window_start: value.window_start,
            traffic_class: value.traffic_class,
            source_type: value.source_type,
            source_id: value.source_server_id,
            destination_type: value.destination_type,
            destination_id: value.destination_server_id,
            payload_bytes: value.payload_bytes,
            payload_frames: value.payload_frames,
            billable_bytes: value.billable_bytes,
            meter_version: value.meter_version,
        }
    }
}

#[cfg(test)]
mod ingress_limit_tests {
    use super::*;

    #[test]
    fn limits_default_apply_and_validate_within_ceilings() {
        let defaults = ServiceIngressLimits::default().effective();
        assert_eq!(
            defaults.rate_limit_per_minute,
            INGRESS_DEFAULT_RATE_PER_MINUTE
        );
        assert_eq!(defaults.max_body_bytes, INGRESS_DEFAULT_MAX_BODY_BYTES);
        let mut limits = ServiceIngressLimits::default();
        let patch: UpdateIngressLimitsRequest = serde_json::from_value(serde_json::json!({
            "concurrency_limit": 8,
            "max_body_bytes": 2048
        }))
        .unwrap();
        patch.apply(&mut limits);
        assert_eq!(limits.concurrency_limit, Some(8));
        assert_eq!(limits.max_body_bytes, Some(2048));
        assert_eq!(limits.effective().concurrency_limit, 8);
        assert_eq!(
            limits.effective().rate_limit_per_minute,
            INGRESS_DEFAULT_RATE_PER_MINUTE
        );
        assert_eq!(limits.validate(), Ok(()));
        // null clears, absent leaves alone, unknown fields are refused.
        let clear: UpdateIngressLimitsRequest =
            serde_json::from_value(serde_json::json!({"concurrency_limit": null})).unwrap();
        clear.apply(&mut limits);
        assert_eq!(limits.concurrency_limit, None);
        assert_eq!(limits.max_body_bytes, Some(2048));
        assert!(serde_json::from_value::<UpdateIngressLimitsRequest>(
            serde_json::json!({"paths": []})
        )
        .is_err());
        for (field, bad) in [
            (
                "rate_limit_per_minute",
                ServiceIngressLimits {
                    rate_limit_per_minute: Some(0),
                    ..Default::default()
                },
            ),
            (
                "rate_limit_per_minute",
                ServiceIngressLimits {
                    rate_limit_per_minute: Some(INGRESS_MAX_RATE_PER_MINUTE + 1),
                    ..Default::default()
                },
            ),
            (
                "concurrency_limit",
                ServiceIngressLimits {
                    concurrency_limit: Some(INGRESS_MAX_CONCURRENCY + 1),
                    ..Default::default()
                },
            ),
            (
                "max_body_bytes",
                ServiceIngressLimits {
                    max_body_bytes: Some(INGRESS_MAX_BODY_BYTES + 1),
                    ..Default::default()
                },
            ),
            (
                "max_body_bytes",
                ServiceIngressLimits {
                    max_body_bytes: Some(0),
                    ..Default::default()
                },
            ),
            (
                "response_timeout_secs",
                ServiceIngressLimits {
                    response_timeout_secs: Some(INGRESS_MIN_RESPONSE_TIMEOUT_SECS - 1),
                    ..Default::default()
                },
            ),
            (
                "response_timeout_secs",
                ServiceIngressLimits {
                    response_timeout_secs: Some(INGRESS_MAX_RESPONSE_TIMEOUT_SECS + 1),
                    ..Default::default()
                },
            ),
        ] {
            assert_eq!(bad.validate(), Err(field));
        }
        let serialized = serde_json::to_value(ServiceIngressLimits::default()).unwrap();
        assert_eq!(
            serialized,
            serde_json::json!({}),
            "unset overrides serialise empty"
        );
    }
}
