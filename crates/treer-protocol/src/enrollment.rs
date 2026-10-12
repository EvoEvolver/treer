use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineEnrollmentKey {
    pub identifier: String,
    pub workspace_id: String,
    pub enrollment_id: String,
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineBootstrapResponse {
    pub install_command: String,
    pub connect_command: String,
    pub windows_install_command: String,
    pub windows_connect_command: String,
    pub enrollment_key: String,
    pub script_url: String,
    pub workspace_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineEnrollmentRequest {
    pub installation_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_server_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineEnrollmentResponse {
    pub workspace_id: String,
    pub server_id: String,
    pub machine_token: String,
}

pub fn format_machine_enrollment_key(
    workspace_id: &str,
    enrollment_id: &str,
    secret: &str,
) -> Result<String, ProtocolError> {
    if workspace_id.is_empty()
        || enrollment_id.is_empty()
        || !enrollment_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
        || secret.len() != 64
        || !secret.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ProtocolError::new(
            "invalid_machine_enrollment_key",
            "machine enrollment key components are invalid",
        ));
    }
    let workspace = encode_hex(workspace_id.as_bytes());
    Ok(format!(
        "{MACHINE_ENROLLMENT_KEY_PREFIX}{workspace}_{enrollment_id}.{secret}"
    ))
}

pub fn parse_machine_enrollment_key(value: &str) -> Result<MachineEnrollmentKey, ProtocolError> {
    let (identifier, secret) = value.split_once('.').ok_or_else(invalid_enrollment_key)?;
    let encoded = identifier
        .strip_prefix(MACHINE_ENROLLMENT_KEY_PREFIX)
        .ok_or_else(invalid_enrollment_key)?;
    let (workspace, enrollment_id) = encoded
        .rsplit_once('_')
        .ok_or_else(invalid_enrollment_key)?;
    if enrollment_id.is_empty()
        || !enrollment_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
        || secret.len() != 64
        || !secret.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_enrollment_key());
    }
    let workspace_id =
        String::from_utf8(decode_hex(workspace)?).map_err(|_| invalid_enrollment_key())?;
    if workspace_id.is_empty() {
        return Err(invalid_enrollment_key());
    }
    Ok(MachineEnrollmentKey {
        identifier: identifier.to_string(),
        workspace_id,
        enrollment_id: enrollment_id.to_string(),
        secret: secret.to_string(),
    })
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[usize::from(byte >> 4)] as char);
        encoded.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

fn decode_hex(value: &str) -> Result<Vec<u8>, ProtocolError> {
    if value.is_empty() || !value.len().is_multiple_of(2) {
        return Err(invalid_enrollment_key());
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = hex_value(pair[0]).ok_or_else(invalid_enrollment_key)?;
            let low = hex_value(pair[1]).ok_or_else(invalid_enrollment_key)?;
            Ok((high << 4) | low)
        })
        .collect()
}

const fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn invalid_enrollment_key() -> ProtocolError {
    ProtocolError::new(
        "invalid_machine_enrollment_key",
        "machine enrollment key is invalid",
    )
}
