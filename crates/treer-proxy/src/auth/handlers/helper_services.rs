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

pub(crate) fn normalize_ingress_slug(value: &str) -> Result<String, AuthFailure> {
    let mut slug = String::new();
    let mut separator = false;
    for byte in value.trim().bytes() {
        if byte.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() && slug.len() < 40 {
                slug.push('-');
            }
            if slug.len() < 40 {
                slug.push(byte.to_ascii_lowercase() as char);
            }
            separator = false;
        } else {
            separator = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        return Err(AuthFailure::bad_request(
            "invalid_ingress_slug",
            "ingress slug must contain an ASCII letter or number",
        ));
    }
    Ok(slug)
}

pub(crate) fn managed_app_ingress_hostname(
    app_name: &str,
    app_id: &str,
    base_domain: &str,
) -> Result<String, AuthFailure> {
    let slug = normalize_ingress_slug(app_name).unwrap_or_else(|_| "app".to_string());
    let suffix = app_id
        .strip_prefix("app_")
        .unwrap_or(app_id)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect::<String>()
        .to_ascii_lowercase();
    let hostname = format!("{slug}-{suffix}.{base_domain}");
    if suffix.is_empty() || hostname.len() > 253 {
        return Err(AuthFailure::bad_request(
            "invalid_ingress_slug",
            "generated App ingress hostname is invalid",
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
