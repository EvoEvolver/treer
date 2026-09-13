use super::*;
pub(crate) fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(crate) fn normalize_email(email: &str) -> Result<String, AuthFailure> {
    let email = email.trim().to_ascii_lowercase();
    let valid = email.len() <= 254
        && !email
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && local.len() <= 64
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        });
    if !valid {
        return Err(AuthFailure::bad_request(
            "invalid_email",
            "enter a valid email address",
        ));
    }
    Ok(email)
}

pub(crate) fn validate_preferred_name(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 80 || value.chars().any(char::is_control) {
        return Err(AuthFailure::bad_request(
            "invalid_preferred_name",
            "preferred name must contain 1-80 visible characters",
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_resource_name(value: &str, resource: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty() || value.len() > 80 || value.chars().any(|character| character.is_control())
    {
        return Err(AuthFailure::bad_request(
            "invalid_name",
            &format!("{resource} name must be 1-80 printable characters"),
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_workspace_role(role: &str) -> Result<(), AuthFailure> {
    if matches!(role, "owner" | "member") {
        Ok(())
    } else {
        Err(AuthFailure::bad_request(
            "invalid_workspace_role",
            "workspace role must be owner or member",
        ))
    }
}

pub(crate) fn validate_installation_id(value: &str) -> Result<String, AuthFailure> {
    if value.len() != 36
        || !value.starts_with("mid_")
        || !value[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AuthFailure::bad_request(
            "invalid_machine_identity",
            "machine installation identity is invalid",
        ));
    }
    Ok(value.to_ascii_lowercase())
}

pub(crate) fn validate_machine_server_id(value: &str) -> Result<String, AuthFailure> {
    if value.len() != 36
        || !value.starts_with("srv_")
        || !value[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AuthFailure::bad_request(
            "invalid_machine_identity",
            "installed machine ID is invalid",
        ));
    }
    Ok(value.to_ascii_lowercase())
}
