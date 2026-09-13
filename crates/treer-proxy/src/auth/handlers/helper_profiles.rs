use super::*;
pub(crate) fn validate_launch_profile_description(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.chars().count() > MAX_LAUNCH_PROFILE_DESCRIPTION_CHARS
        || value.chars().any(|character| character == '\0')
    {
        return Err(AuthFailure::bad_request(
            "invalid_launch_profile",
            "launch profile description must be at most 1000 characters and contain no NUL bytes",
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_launch_profile_cwd(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    let value = if value.is_empty() { "." } else { value };
    if value.len() > MAX_LAUNCH_PROFILE_CWD_BYTES || value.contains('\0') {
        return Err(AuthFailure::bad_request(
            "invalid_launch_profile",
            "launch profile working directory is too long or contains a NUL byte",
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_launch_profile_command(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_LAUNCH_PROFILE_COMMAND_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(AuthFailure::bad_request(
            "invalid_launch_profile",
            "launch profile command must be 1-4096 printable characters",
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_launch_profile_args(args: Vec<String>) -> Result<Vec<String>, AuthFailure> {
    let total_bytes = args.iter().map(String::len).sum::<usize>();
    if args.len() > MAX_LAUNCH_PROFILE_ARGS
        || total_bytes > MAX_LAUNCH_PROFILE_ARGS_BYTES
        || args
            .iter()
            .any(|arg| arg.len() > MAX_LAUNCH_PROFILE_ARG_BYTES || arg.contains('\0'))
    {
        return Err(AuthFailure::bad_request(
            "invalid_launch_profile",
            "launch profile args exceed the count or size limit or contain a NUL byte",
        ));
    }
    Ok(args)
}

pub(crate) fn launch_profile_write_error(error: sqlx::Error) -> AuthFailure {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
    {
        AuthFailure::conflict(
            "launch_profile_exists",
            "a launch profile with this name already exists",
        )
    } else {
        AuthFailure::database(error)
    }
}

pub(crate) async fn insert_default_agent_launch_profiles(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: &str,
    user_id: &str,
    now: &chrono::DateTime<Utc>,
) -> Result<(), AuthFailure> {
    let timestamp = now.to_rfc3339();
    for (name, description, command) in DEFAULT_AGENT_LAUNCH_PROFILES {
        sqlx::query(
            "INSERT INTO agent_launch_profiles(\
             profile_id, workspace_id, name, description, cwd, command, args, created_at, \
             created_by, updated_at, updated_by) VALUES($1, $2, $3, $4, '.', $5, $6, $7, $8, $7, $8)",
        )
        .bind(format!("alp_{}", Uuid::new_v4().simple()))
        .bind(workspace_id)
        .bind(name)
        .bind(description)
        .bind(command)
        .bind(json!([]))
        .bind(&timestamp)
        .bind(user_id)
        .execute(&mut **transaction)
        .await
        .map_err(AuthFailure::database)?;
    }
    Ok(())
}

pub(crate) async fn insert_launch_profile_audit(
    transaction: &mut Transaction<'_, Postgres>,
    profile: &AgentLaunchProfile,
    actor: ProfileMutationActor<'_>,
    action: &str,
) -> Result<(), AuthFailure> {
    let organization_id = sqlx::query_scalar::<_, String>(
        "SELECT organization_id FROM workspaces WHERE workspace_id = $1",
    )
    .bind(&profile.workspace_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(AuthFailure::database)?;
    audit::insert(
        transaction,
        NewAuditEvent {
            organization_id: &organization_id,
            workspace_id: Some(&profile.workspace_id),
            actor_kind: actor.kind,
            actor_id: actor.id,
            source: "api",
            action,
            resource_kind: "agent_launch_profile",
            resource_id: &profile.profile_id,
            resource_name: Some(&profile.name),
            payload: json!({}),
        },
    )
    .await
    .map_err(AuthFailure::database)
}
