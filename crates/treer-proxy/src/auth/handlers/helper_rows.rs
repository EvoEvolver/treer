use super::*;
pub(crate) fn workspace_from_row(row: sqlx::postgres::PgRow) -> Result<WorkspaceInfo, AuthFailure> {
    let created_at: String = row.get("created_at");
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            tracing::error!(%error, "invalid workspace timestamp in database");
            AuthFailure::internal(
                "database_error",
                "workspace timestamp is invalid".to_string(),
            )
        })?;
    Ok(WorkspaceInfo {
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        created_at,
    })
}

pub(crate) fn agent_launch_profile_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<AgentLaunchProfile, AuthFailure> {
    let args = serde_json::from_value::<Vec<String>>(row.get("args")).map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("agent launch profile has invalid args: {error}"),
        )
    })?;
    Ok(AgentLaunchProfile {
        profile_id: row.get("profile_id"),
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        description: row.get("description"),
        cwd: row.get("cwd"),
        command: row.get("command"),
        args,
        created_at: parse_database_timestamp(&row, "created_at", "agent launch profile")?,
        created_by: row.get("created_by"),
        updated_at: parse_database_timestamp(&row, "updated_at", "agent launch profile")?,
        updated_by: row.get("updated_by"),
    })
}

pub(crate) fn app_deployment_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<AppDeployment, AuthFailure> {
    let args = serde_json::from_value::<Vec<String>>(row.get("args")).map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("App deployment has invalid args: {error}"),
        )
    })?;
    let port = u16::try_from(row.get::<i64, _>("port")).map_err(|_| {
        AuthFailure::internal(
            "database_error",
            "App deployment has invalid port".to_string(),
        )
    })?;
    let restart_count = u64::try_from(row.get::<i64, _>("restart_count")).map_err(|_| {
        AuthFailure::internal(
            "database_error",
            "App deployment has invalid restart count".to_string(),
        )
    })?;
    let desired_state = match row.get::<String, _>("desired_state").as_str() {
        "running" => AppDesiredState::Running,
        "stopped" => AppDesiredState::Stopped,
        value => {
            return Err(AuthFailure::internal(
                "database_error",
                format!("App deployment has invalid desired state {value}"),
            ))
        }
    };
    Ok(AppDeployment {
        app_id: row.get("app_id"),
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        server_id: row.get("server_id"),
        command: row.get("command"),
        args,
        cwd: row.get("cwd"),
        port,
        hostname: row.get("hostname"),
        service_id: row.get("service_id"),
        public_url: None,
        access: None,
        desired_state,
        runtime_agent_id: row.get("runtime_agent_id"),
        restart_count,
        status: AppDeploymentStatus::Pending,
        pid: None,
        exit_code: None,
        last_error: row.get("last_error"),
        created_at: parse_database_timestamp(&row, "created_at", "App deployment")?,
        created_by: row.get("created_by"),
        updated_at: parse_database_timestamp(&row, "updated_at", "App deployment")?,
        updated_by: row.get("updated_by"),
    })
}

pub(crate) const fn app_desired_state_str(state: AppDesiredState) -> &'static str {
    match state {
        AppDesiredState::Running => "running",
        AppDesiredState::Stopped => "stopped",
    }
}

pub(crate) fn app_deployment_write_error(error: sqlx::Error) -> AuthFailure {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
    {
        AuthFailure::conflict(
            "app_conflict",
            "App name, service name, or virtual hostname already exists",
        )
    } else {
        AuthFailure::database(error)
    }
}
