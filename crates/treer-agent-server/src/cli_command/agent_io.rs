use super::*;

pub(super) async fn prompt_and_maybe_wait(
    client: &ApiClient,
    target: &str,
    text: String,
    steer: bool,
    notify: bool,
    wait: PromptWaitArgs,
) -> anyhow::Result<Value> {
    if !wait.until.is_empty() {
        bail!("prompt --wait waits for its submission; use agent wait for --until status matching");
    }
    let target = workload_tree::resolve_target(client, target).await?;
    let mut submitted = client
        .value(
            Method::POST,
            &format!("api/agents/{}/prompt", path_segment(&target)),
            Some(serde_json::to_value(
                treer_protocol::SubmitAgentPromptRequest {
                    text,
                    delivery: if steer {
                        treer_protocol::PromptDelivery::Steer
                    } else {
                        treer_protocol::PromptDelivery::Queue
                    },
                    notify: notify.then(treer_protocol::PromptNotify::default),
                    require_receipt: wait.wait,
                },
            )?),
        )
        .await?;
    if !wait.wait {
        return Ok(submitted);
    }
    let id = submitted["submission"]["id"]
        .as_str()
        .context("Target did not return a submission receipt")?
        .to_string();
    let deadline = wait
        .timeout
        .map(|ms| Instant::now() + Duration::from_millis(ms));
    loop {
        let path = format!(
            "api/agents/{}/submissions/{}",
            path_segment(&target),
            path_segment(&id)
        );
        let request = client.value(Method::GET, &path, None);
        let result = match deadline {
            Some(deadline) => {
                tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), request)
                    .await
                    .with_context(|| {
                        format!("Timed out waiting for submission {id}; it remains submitted")
                    })??
            }
            None => request.await?,
        };
        let submission: treer_protocol::PromptSubmission =
            serde_json::from_value(result["submission"].clone())?;
        if submission.id != id {
            bail!("Target returned a different submission while waiting for {id}");
        }
        if submission.state.is_terminal() {
            submitted["completion"] = serde_json::to_value(submission)?;
            return Ok(submitted);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            bail!("Timed out waiting for submission {id}; it remains submitted");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub(super) async fn read_agent(
    client: &ApiClient,
    target: &str,
    lines: usize,
) -> anyhow::Result<Value> {
    let target = workload_tree::resolve_target(client, target).await?;
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
    let target = workload_tree::resolve_target(client, target).await?;
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
    let target = workload_tree::resolve_target(client, target).await?;
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
    let target = workload_tree::resolve_target(client, target).await?;
    client
        .value(
            Method::PATCH,
            &format!("api/agents/{}", path_segment(&target)),
            Some(serde_json::to_value(RenameRequest { name })?),
        )
        .await
}

pub(super) async fn delete_agent(client: &ApiClient, target: &str) -> anyhow::Result<Value> {
    let target = workload_tree::resolve_target(client, target).await?;
    client
        .value(
            Method::DELETE,
            &format!("api/agents/{}", path_segment(&target)),
            None,
        )
        .await
}
