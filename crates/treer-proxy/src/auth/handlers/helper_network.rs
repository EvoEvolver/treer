use super::*;
pub(crate) fn normalize_virtual_hostname(value: &str) -> Result<String, AuthFailure> {
    let hostname = value.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels_valid = !hostname.is_empty()
        && hostname.len() <= 253
        && hostname.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
    if !labels_valid {
        return Err(AuthFailure::bad_request(
            "invalid_virtual_hostname",
            "hostname must contain valid DNS labels",
        ));
    }
    Ok(hostname)
}

pub(crate) fn virtual_network_host_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<VirtualNetworkHost, AuthFailure> {
    let created_at = row
        .get::<String, _>("created_at")
        .parse()
        .map_err(|error| {
            AuthFailure::internal(
                "database_error",
                format!("virtual network host has invalid created_at: {error}"),
            )
        })?;
    let target_port = row
        .get::<Option<i64>, _>("target_port")
        .map(u16::try_from)
        .transpose()
        .map_err(|_| {
            AuthFailure::internal(
                "database_error",
                "virtual network host has invalid target_port".to_string(),
            )
        })?;
    Ok(VirtualNetworkHost {
        workspace_id: row.get("workspace_id"),
        hostname: row.get("hostname"),
        service_id: row.get("service_id"),
        service_protocol: match row.get::<String, _>("service_protocol").as_str() {
            "udp" => MachineServiceProtocol::Udp,
            "tcp" => MachineServiceProtocol::Tcp,
            "http" => MachineServiceProtocol::Http,
            value => {
                return Err(AuthFailure::internal(
                    "database_error",
                    format!("virtual network host has invalid service protocol {value}"),
                ))
            }
        },
        destination_server_id: row.get("destination_server_id"),
        destination_agent_id: row.get("destination_agent_id"),
        target_host: row.get("target_host"),
        target_port,
        created_at,
        created_by: row.get("created_by"),
    })
}

pub(crate) fn hash_password(password: &str) -> Result<String, AuthFailure> {
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
        .map_err(|error| AuthFailure::internal("password_hash_error", error.to_string()))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| AuthFailure::internal("password_hash_error", error.to_string()))
}

pub(crate) fn verify_password(password: &str, encoded: &str) -> bool {
    PasswordHash::new(encoded).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}
