use super::*;

pub(super) async fn prompt_and_maybe_wait(
    client: &ApiClient,
    target: &str,
    text: String,
    wait: PromptWaitArgs,
) -> anyhow::Result<Value> {
    let submitted = client.prompt(target, text).await?;
    if !wait.wait {
        return Ok(serde_json::to_value(submitted)?);
    }
    let until = default_wait_statuses(wait.until);
    let baseline = Some((submitted.status, submitted.output_revision));
    Ok(serde_json::to_value(
        client
            .wait_for(target, &until, wait.timeout, baseline)
            .await?,
    )?)
}

pub(super) async fn read_agent(
    client: &ApiClient,
    target: &str,
    lines: usize,
) -> anyhow::Result<Value> {
    let target = normalize_target(target)?;
    client
        .value(
            Method::GET,
            &format!("api/agents/{}/output?lines={lines}", path_segment(&target)),
            None,
        )
        .await
}

pub(super) async fn read_agent_transcript(
    client: &ApiClient,
    target: &str,
    page: Option<u32>,
    cursor: Option<String>,
    limit: usize,
) -> anyhow::Result<Value> {
    let target = normalize_target(target)?;
    let mut path = format!(
        "api/agents/{}/transcript?limit={}",
        path_segment(&target),
        limit.min(1000)
    );
    let page = page
        .map(|value| value.to_string())
        .or_else(|| cursor.filter(|value| !value.is_empty()));
    if let Some(page) = page {
        path.push_str("&page=");
        path.push_str(&path_segment(&page));
        path.push_str("&cursor=");
        path.push_str(&path_segment(&page));
    }
    client.value(Method::GET, &path, None).await
}

pub(super) async fn stop_agent(client: &ApiClient, target: &str) -> anyhow::Result<Value> {
    let target = normalize_target(target)?;
    client
        .value(
            Method::POST,
            &format!("api/agents/{}/stop", path_segment(&target)),
            Some(json!({})),
        )
        .await
}

pub(super) async fn rename_agent(
    client: &ApiClient,
    target: &str,
    name: String,
) -> anyhow::Result<Value> {
    let target = normalize_target(target)?;
    client
        .value(
            Method::PATCH,
            &format!("api/agents/{}", path_segment(&target)),
            Some(serde_json::to_value(RenameRequest { name })?),
        )
        .await
}

pub(super) async fn delete_agent(client: &ApiClient, target: &str) -> anyhow::Result<Value> {
    let target = normalize_target(target)?;
    client
        .value(
            Method::DELETE,
            &format!("api/agents/{}", path_segment(&target)),
            None,
        )
        .await
}
