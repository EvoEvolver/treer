use super::*;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MessageStdinEnvelope {
    pub body: String,
    pub external_source: Option<MessageExternalSource>,
}

pub(super) fn read_message_envelope(input: impl StdRead) -> anyhow::Result<MessageStdinEnvelope> {
    const MAX_ENVELOPE_BYTES: u64 = 1024 * 1024;
    let mut bytes = Vec::new();
    input
        .take(MAX_ENVELOPE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("failed to read message stdin envelope")?;
    if bytes.len() as u64 > MAX_ENVELOPE_BYTES {
        bail!("message stdin envelope exceeds 1 MiB");
    }
    // Serde errors can quote supplied values or unknown field names. Do not leak
    // message text through the CLI error channel.
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!(
        "invalid message stdin envelope: expected one JSON object with body (string) and optional external_source"
    ))
}

pub(super) async fn run_message_command(
    client: &ApiClient,
    command: MessageCommand,
) -> anyhow::Result<Value> {
    match command {
        MessageCommand::Send {
            from_endpoint,
            recipients,
            context_ids,
            body,
            body_file,
            idempotency_key,
            expires_at,
            correlation_id,
            trace_id,
            external_source_file,
            stdin_envelope,
        } => {
            let (body, external_source) = if stdin_envelope {
                let envelope = read_message_envelope(std::io::stdin().lock())?;
                (envelope.body, envelope.external_source)
            } else {
                (
                    read_message_body(body, body_file)?,
                    read_external_source(external_source_file)?,
                )
            };
            let request = SendMessageRequest {
                sender_endpoint: from_endpoint,
                recipients,
                context_ids,
                body,
                expires_at: expires_at
                    .map(|value| parse_rfc3339(&value, "message expiry"))
                    .transpose()?,
                idempotency_key,
                correlation_id,
                trace_id,
                external_source,
            };
            client
                .value(
                    Method::POST,
                    "api/messages",
                    Some(serde_json::to_value(request)?),
                )
                .await
        }
        MessageCommand::Reply {
            from_endpoint,
            message_id,
            mut recipients,
            additional_context_ids,
            body,
            body_file,
            idempotency_key,
            external_source_file,
        } => {
            let message_id = normalize_message_id(&message_id)?;
            let parent: GetMessageResponse = client
                .request(
                    Method::GET,
                    &format!("api/messages/{}", path_segment(&message_id)),
                    None,
                )
                .await?;
            let reply_target = match &parent.message.sender.app_endpoint {
                Some(endpoint) => format!("{}/{}", parent.message.sender.id, endpoint),
                None => parent.message.sender.id.clone(),
            };
            if recipients.is_empty() {
                recipients.push(reply_target.clone());
            } else {
                recipients = recipients
                    .into_iter()
                    .map(|recipient| {
                        if recipient == "sender" {
                            reply_target.clone()
                        } else {
                            recipient
                        }
                    })
                    .collect();
            }
            let mut context_ids = vec![message_id];
            context_ids.extend(additional_context_ids);
            client
                .value(
                    Method::POST,
                    "api/messages",
                    Some(serde_json::to_value(SendMessageRequest {
                        sender_endpoint: from_endpoint,
                        recipients,
                        context_ids,
                        body: read_message_body(body, body_file)?,
                        expires_at: None,
                        idempotency_key,
                        correlation_id: parent.message.correlation_id,
                        trace_id: parent.message.trace_id,
                        external_source: read_external_source(external_source_file)?,
                    })?),
                )
                .await
        }
        MessageCommand::Get { message_id } => {
            let message_id = normalize_message_id(&message_id)?;
            client
                .value(
                    Method::GET,
                    &format!("api/messages/{}", path_segment(&message_id)),
                    None,
                )
                .await
        }
        MessageCommand::List { before, limit } => {
            let suffix = message_list_suffix(before.as_deref(), limit);
            client.value(Method::GET, &suffix, None).await
        }
        MessageCommand::Receive { wait, limit } => {
            client
                .value(
                    Method::POST,
                    "api/messages/receive",
                    Some(serde_json::to_value(ReceiveMessagesRequest {
                        limit,
                        wait_milliseconds: wait,
                    })?),
                )
                .await
        }
        MessageCommand::Ack {
            delivery_ids,
            operation_id,
        } => {
            client
                .value(
                    Method::POST,
                    "api/messages/ack",
                    Some(serde_json::to_value(AcknowledgeMessagesRequest {
                        delivery_ids,
                        operation_id: operation_id.unwrap_or_else(new_operation_id),
                    })?),
                )
                .await
        }
        MessageCommand::Import {
            format,
            body_file,
            operation_id,
        } => {
            let messages =
                read_legacy_import(body_file.as_deref().unwrap_or_else(|| Path::new("-")))?;
            client
                .value(
                    Method::POST,
                    "api/messages/import",
                    Some(serde_json::to_value(ImportMessagesRequest {
                        format,
                        operation_id: operation_id.unwrap_or_else(new_operation_id),
                        messages,
                    })?),
                )
                .await
        }
    }
}

pub(super) fn read_message_body(
    body: Option<String>,
    body_file: Option<PathBuf>,
) -> anyhow::Result<String> {
    match (body, body_file) {
        (Some(body), None) => Ok(body),
        (None, Some(path)) => read_text_file_or_stdin(&path),
        (None, None) if !std::io::stdin().is_terminal() => read_stdin_text(),
        (None, None) => bail!("message body is required through --body or piped stdin"),
        (Some(_), Some(_)) => bail!("--body and --body-file cannot be used together"),
    }
}

pub(super) fn read_external_source(
    path: Option<PathBuf>,
) -> anyhow::Result<Option<MessageExternalSource>> {
    path.map(|path| {
        let contents = read_text_file_or_stdin(&path)?;
        serde_json::from_str(&contents).context("external source file is not valid JSON")
    })
    .transpose()
}

pub(super) fn read_legacy_import(path: &Path) -> anyhow::Result<Vec<LegacyMailMessage>> {
    let contents = read_text_file_or_stdin(path)?;
    if let Ok(messages) = serde_json::from_str::<Vec<LegacyMailMessage>>(&contents) {
        return Ok(messages);
    }
    if let Ok(request) = serde_json::from_str::<ImportMessagesRequest>(&contents) {
        return Ok(request.messages);
    }
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str::<LegacyMailMessage>(line)
                .with_context(|| format!("legacy-mail-v1 JSONL record {} is invalid", index + 1))
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .and_then(|messages| {
            if messages.is_empty() {
                bail!("legacy-mail-v1 import contains no messages")
            } else {
                Ok(messages)
            }
        })
}

pub(super) fn read_text_file_or_stdin(path: &Path) -> anyhow::Result<String> {
    if path.as_os_str() == "-" {
        read_stdin_text()
    } else {
        bail!("client-local files cannot be read by the Controller; pipe the contents to stdin and use '-' instead")
    }
}

pub(super) fn read_stdin_text() -> anyhow::Result<String> {
    let mut contents = String::new();
    std::io::stdin()
        .read_to_string(&mut contents)
        .context("failed to read stdin")?;
    Ok(contents)
}

pub(super) fn parse_rfc3339(value: &str, field: &str) -> anyhow::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .with_context(|| format!("{field} must be an RFC3339 timestamp"))
}

pub(super) fn message_list_suffix(before: Option<&str>, limit: u16) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("limit", &limit.to_string());
    if let Some(before) = before {
        query.append_pair("before", before);
    }
    format!("api/messages?{}", query.finish())
}

pub(super) fn normalize_message_id(message_id: &str) -> anyhow::Result<String> {
    let message_id = message_id.trim();
    if message_id.is_empty() || message_id.len() > 256 {
        bail!("message ID must be non-empty and at most 256 bytes");
    }
    Ok(message_id.to_string())
}

pub(super) fn new_operation_id() -> String {
    format!("op_{}", uuid::Uuid::new_v4().simple())
}
