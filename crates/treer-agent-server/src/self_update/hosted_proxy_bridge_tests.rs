//! Private test-process transport only. No production flag, capability or route.
use super::*;

pub(super) fn publish(
    directory: &PrivateDirectory,
    name: &str,
    value: &serde_json::Value,
) -> Result<()> {
    let temporary = format!("bridge-{}.tmp", uuid::Uuid::new_v4().simple());
    write_json(directory, &temporary, value)?;
    directory.replace_file(&temporary, name)?;
    Ok(())
}

fn validate_binding(binding: &DispatchBinding, input: &Input) -> Result<()> {
    if !binding.validate()
        || binding.expected_plan_digest().ok().as_deref() != Some(binding.plan_digest.as_str())
        || binding.workspace_id != "ws_native_acceptance"
        || binding.server_id != "srv_native_acceptance"
        || binding.catalog_revision != 1
        || binding.current != input.current
        || binding.target != input.target
    {
        bail!("bridge_dispatch_binding_invalid");
    }
    Ok(())
}

pub(super) async fn binding(
    directory: &PrivateDirectory,
    input: &Input,
) -> Result<DispatchBinding> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        match directory.read_file("proxy-native-dispatch.json") {
            Ok(mut file) => {
                let mut bytes = Vec::new();
                Read::by_ref(&mut file)
                    .take(65537)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > 65536 {
                    bail!("bridge_dispatch_binding_invalid");
                }
                let binding = serde_json::from_slice(&bytes)?;
                validate_binding(&binding, input)?;
                return Ok(binding);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("bridge_dispatch_timeout_no_retry");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub(super) fn publish_operation(
    directory: &PrivateDirectory,
    binding: &DispatchBinding,
    op: &wire::Operation,
) -> Result<()> {
    op.validate()
        .map_err(|_| anyhow::anyhow!("bridge_native_operation_invalid"))?;
    if op.operation_id != binding.operation_id
        || op.current != binding.current
        || op.target != binding.target
        || op.created_at != binding.created_at
    {
        bail!("bridge_native_operation_mismatch");
    }
    publish(
        directory,
        "proxy-native-operation.json",
        &serde_json::to_value(op)?,
    )
}

#[test]
fn native_bridge_binding_refuses_cross_scope_target_revision_and_plan() {
    let identity = |c: char| wire::Identity {
        release_sha: c.to_string().repeat(40),
        artifact_source_sha: c.to_string().repeat(40),
        controller_sha256: c.to_string().repeat(64),
        cli_sha256: c.to_string().repeat(64),
    };
    let input = Input {
        current: identity('a'),
        target: wire::Target {
            target_id: "native-next".into(),
            identity: identity('b'),
            platform: wire::Platform::LinuxX86_64,
            version_label: "fixture".into(),
        },
        sums_sha256: "c".repeat(64),
        host_sha256: "d".repeat(64),
    };
    let mut binding = DispatchBinding {
        operation_id: uuid::Uuid::new_v4().to_string(),
        workspace_id: "ws_native_acceptance".into(),
        server_id: "srv_native_acceptance".into(),
        current: input.current.clone(),
        target: input.target.clone(),
        catalog_revision: 1,
        plan_digest: "0".repeat(64),
        created_at: chrono::Utc::now(),
    };
    binding.plan_digest = binding.expected_plan_digest().unwrap();
    validate_binding(&binding, &input).unwrap();
    for field in [
        "workspace_id",
        "server_id",
        "catalog_revision",
        "target",
        "plan_digest",
    ] {
        let mut bad = serde_json::to_value(&binding).unwrap();
        match field {
            "catalog_revision" => bad[field] = serde_json::json!(2),
            "target" => bad[field]["identity"]["cli_sha256"] = serde_json::json!("e".repeat(64)),
            "plan_digest" => bad[field] = serde_json::json!("f".repeat(64)),
            _ => bad[field] = serde_json::json!("other"),
        }
        let mut bad: DispatchBinding = serde_json::from_value(bad).unwrap();
        if field != "plan_digest" {
            bad.plan_digest = bad.expected_plan_digest().unwrap();
        }
        assert!(validate_binding(&bad, &input).is_err(), "{field}");
    }
}
