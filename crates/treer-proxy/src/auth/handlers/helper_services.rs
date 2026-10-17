use super::*;
pub(crate) fn machine_service_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<MachineService, AuthFailure> {
    let target_port = u16::try_from(row.get::<i64, _>("target_port")).map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("machine service has invalid target_port: {error}"),
        )
    })?;
    if target_port == 0 {
        return Err(AuthFailure::internal(
            "database_error",
            "machine service target_port is zero".to_string(),
        ));
    }
    let protocol = match row.get::<String, _>("protocol").as_str() {
        "udp" => MachineServiceProtocol::Udp,
        "tcp" => MachineServiceProtocol::Tcp,
        "http" => MachineServiceProtocol::Http,
        value => {
            return Err(AuthFailure::internal(
                "database_error",
                format!("machine service has invalid protocol {value}"),
            ))
        }
    };
    Ok(MachineService {
        service_id: row.get("service_id"),
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        server_id: row.get("server_id"),
        target_agent_id: row.get("target_agent_id"),
        primary: row.get("is_primary"),
        target_host: row.get("target_host"),
        target_port,
        protocol,
        created_at: parse_database_timestamp(&row, "created_at", "machine service")?,
        created_by: row.get("created_by"),
        updated_at: parse_database_timestamp(&row, "updated_at", "machine service")?,
        updated_by: row.get("updated_by"),
    })
}

pub(crate) fn parse_database_timestamp(
    row: &sqlx::postgres::PgRow,
    column: &str,
    resource: &str,
) -> Result<chrono::DateTime<Utc>, AuthFailure> {
    row.get::<String, _>(column).parse().map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("{resource} has invalid {column}: {error}"),
        )
    })
}

pub(crate) fn validate_primary_service(
    primary: bool,
    target_agent_id: Option<&str>,
    protocol: MachineServiceProtocol,
) -> Result<(), AuthFailure> {
    if primary && (target_agent_id.is_none() || protocol != MachineServiceProtocol::Http) {
        return Err(AuthFailure::bad_request(
            "invalid_primary_service",
            "a primary endpoint must be an Agent-owned HTTP service",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn primary_service_requires_an_agent_owned_http_endpoint() {
    assert!(validate_primary_service(true, Some("agent-a"), MachineServiceProtocol::Http).is_ok());
    assert!(validate_primary_service(true, None, MachineServiceProtocol::Http).is_err());
    assert!(validate_primary_service(true, Some("agent-a"), MachineServiceProtocol::Tcp).is_err());
    assert!(validate_primary_service(false, None, MachineServiceProtocol::Tcp).is_ok());
}

pub(crate) fn validate_service_target_host(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 253
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(AuthFailure::bad_request(
            "invalid_service",
            "target_host must be a non-empty hostname or address",
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_service_target_agent_id(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 255
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(AuthFailure::bad_request(
            "invalid_service",
            "target_agent_id must be a non-empty Agent identifier",
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_agent_service_target_host(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if !matches!(value, "127.0.0.1" | "localhost" | "::1") {
        return Err(AuthFailure::bad_request(
            "invalid_agent_service_target",
            "Agent services may only target the Agent loopback interface",
        ));
    }
    Ok("127.0.0.1".to_string())
}

pub(crate) const fn machine_service_protocol_str(protocol: MachineServiceProtocol) -> &'static str {
    match protocol {
        MachineServiceProtocol::Udp => "udp",
        MachineServiceProtocol::Tcp => "tcp",
        MachineServiceProtocol::Http => "http",
    }
}

pub(crate) const fn service_ingress_access_str(access: ServiceIngressAccess) -> &'static str {
    match access {
        ServiceIngressAccess::Public => "public",
        ServiceIngressAccess::Workspace => "workspace",
    }
}

pub(crate) fn platform_service_hostname(
    service_id: &str,
    base_domain: &str,
) -> Result<String, AuthFailure> {
    let id = service_id.strip_prefix("svc_").unwrap_or(service_id);
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(AuthFailure::bad_request(
            "invalid_service_id",
            "service ID cannot form a platform address",
        ));
    }
    let hostname = format!("svc-{}.{}", id.to_ascii_lowercase(), base_domain);
    if hostname.len() > 253 {
        return Err(AuthFailure::bad_request(
            "invalid_service_domain",
            "platform address exceeds DNS length",
        ));
    }
    Ok(hostname)
}

pub(crate) fn validate_ingress_return_path(value: &str) -> Result<String, AuthFailure> {
    if !value.starts_with('/')
        || value.starts_with("//")
        || value.len() > 4096
        || value.chars().any(char::is_control)
    {
        return Err(AuthFailure::bad_request(
            "invalid_ingress_return_path",
            "ingress return path must be a local absolute path",
        ));
    }
    Ok(value.to_string())
}

fn limit_u32(row: &sqlx::postgres::PgRow, column: &str) -> Result<Option<u32>, AuthFailure> {
    let value: Option<i64> = row.try_get(column).map_err(AuthFailure::database)?;
    value
        .map(|value| {
            u32::try_from(value).map_err(|_| {
                AuthFailure::internal(
                    "database_error",
                    format!("service ingress limit {column} is out of range"),
                )
            })
        })
        .transpose()
}

/// The limit columns were added by #146; a row without them (older
/// schema in a test fixture) reads as "all defaults".
pub(crate) fn ingress_limits_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<treer_protocol::ServiceIngressLimits, AuthFailure> {
    if row
        .try_get::<Option<i64>, _>("limit_rate_per_minute")
        .is_err()
    {
        return Ok(Default::default());
    }
    let max_body: Option<i64> = row
        .try_get("limit_max_body_bytes")
        .map_err(AuthFailure::database)?;
    Ok(treer_protocol::ServiceIngressLimits {
        rate_limit_per_minute: limit_u32(row, "limit_rate_per_minute")?,
        rate_burst: limit_u32(row, "limit_rate_burst")?,
        concurrency_limit: limit_u32(row, "limit_concurrency")?,
        ip_rate_limit_per_minute: limit_u32(row, "limit_ip_rate_per_minute")?,
        ip_rate_burst: limit_u32(row, "limit_ip_rate_burst")?,
        max_body_bytes: max_body.map(|value| u64::try_from(value).unwrap_or(0)),
        response_timeout_secs: limit_u32(row, "limit_response_timeout_secs")?,
        body_timeout_secs: if row
            .try_get::<Option<i64>, _>("limit_body_timeout_secs")
            .is_ok()
        {
            limit_u32(row, "limit_body_timeout_secs")?
        } else {
            None
        },
    })
}

pub(crate) fn resolved_service_ingress_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<ResolvedServiceIngress, AuthFailure> {
    let ingress = ServiceIngress {
        ingress_id: row.get("ingress_id"),
        workspace_id: row.get("workspace_id"),
        service_id: row.get("service_id"),
        hostname: row.get("hostname"),
        access: match row.get::<String, _>("access").as_str() {
            "public" => ServiceIngressAccess::Public,
            "workspace" => ServiceIngressAccess::Workspace,
            value => {
                return Err(AuthFailure::internal(
                    "database_error",
                    format!("service ingress has invalid access mode {value}"),
                ))
            }
        },
        enabled: row.get("enabled"),
        created_at: parse_database_timestamp(&row, "created_at", "service ingress")?,
        created_by: row.get("created_by"),
        updated_at: parse_database_timestamp(&row, "updated_at", "service ingress")?,
        updated_by: row.get("updated_by"),
        limits: ingress_limits_from_row(&row)?,
    };
    let target_port = u16::try_from(row.get::<i64, _>("target_port")).map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("service ingress target has invalid port: {error}"),
        )
    })?;
    let service = MachineService {
        service_id: ingress.service_id.clone(),
        workspace_id: ingress.workspace_id.clone(),
        name: row.get("service_name"),
        server_id: row.get("server_id"),
        target_agent_id: row.get("target_agent_id"),
        primary: row.get("is_primary"),
        target_host: row.get("target_host"),
        target_port,
        protocol: match row.get::<String, _>("service_protocol").as_str() {
            "udp" => MachineServiceProtocol::Udp,
            "tcp" => MachineServiceProtocol::Tcp,
            "http" => MachineServiceProtocol::Http,
            value => {
                return Err(AuthFailure::internal(
                    "database_error",
                    format!("service ingress target has invalid protocol {value}"),
                ))
            }
        },
        created_at: parse_database_timestamp(&row, "service_created_at", "service ingress target")?,
        created_by: row.get("service_created_by"),
        updated_at: parse_database_timestamp(&row, "service_updated_at", "service ingress target")?,
        updated_by: row.get("service_updated_by"),
    };
    Ok(ResolvedServiceIngress { ingress, service })
}
