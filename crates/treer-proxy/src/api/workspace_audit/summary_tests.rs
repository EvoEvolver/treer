//! Audit summary route, shared list filters, category parity, backfill and
//! index tests (#191, #192). PostgreSQL-backed like the sibling module.
use super::tests::{body, fixture, response, test_router};
use super::*;
use chrono::{DateTime, Duration, Utc};
use sqlx::Row;
use treer_protocol::{audit_category, is_notable, AuditCategory};

#[allow(clippy::too_many_arguments)]
async fn at(
    auth: &AuthStore,
    workspace: &str,
    actor_kind: &str,
    actor: Option<&str>,
    action: &str,
    outcome: &str,
    resource: (&str, &str),
    when: DateTime<Utc>,
) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO organization_audit_events(event_id, organization_id, workspace_id, occurred_at, \
         actor_kind, actor_id, source, action, outcome, resource_kind, resource_id, payload, workspace_origin_id) \
         VALUES($1, 'org_a', $2, $3, $4, $5, 'test', $6, $7, $8, $9, '{}'::JSONB, $2) RETURNING sequence",
    )
    .bind(format!("aud_{}", uuid::Uuid::new_v4().simple()))
    .bind(workspace)
    .bind(when.to_rfc3339())
    .bind(actor_kind)
    .bind(actor)
    .bind(action)
    .bind(outcome)
    .bind(resource.0)
    .bind(resource.1)
    .fetch_one(&auth.pool())
    .await
    .unwrap()
}

fn hour(now: DateTime<Utc>, hours: i64) -> DateTime<Utc> {
    now - Duration::hours(hours)
}

/// PostgreSQL stores microseconds; `Utc::now()` carries nanoseconds on Linux
/// (not on macOS), so every instant a test compares with a stored value is
/// truncated first. Rounding on the way in is the database's business and is
/// covered separately by [`audit_instant_trigger_and_backfill_touch_only_the_typed_column`].
fn micros_now() -> DateTime<Utc> {
    let now = Utc::now();
    DateTime::from_timestamp_micros(now.timestamp_micros()).unwrap()
}

#[tokio::test]
async fn audit_summary_counts_series_rankings_notable_anomalies_and_coverage() {
    let auth = fixture().await;
    let now = micros_now() - Duration::seconds(5);
    sqlx::query("INSERT INTO agent_names(agent_id,workspace_id,name,updated_at) VALUES('ag_1','a','Builder','t')")
        .execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO machine_names(server_id,workspace_id,name,updated_at) VALUES('srv_1','a','Mac','t')")
        .execute(&auth.pool()).await.unwrap();
    for h in [5, 4, 3, 2] {
        at(
            &auth,
            "a",
            "user",
            Some("owner"),
            "message.created",
            "succeeded",
            ("message", "m1"),
            hour(now, h),
        )
        .await;
    }
    for _ in 0..12 {
        at(
            &auth,
            "a",
            "agent",
            Some("ag_1"),
            "agent.started",
            "succeeded",
            ("agent", "ag_1"),
            hour(now, 1),
        )
        .await;
    }
    let policy_seq = at(
        &auth,
        "a",
        "user",
        Some("owner"),
        "policy.rule.changed",
        "failed",
        ("workspace", "a"),
        now,
    )
    .await;
    at(
        &auth,
        "a",
        "system",
        None,
        "weird.thing",
        "succeeded",
        ("workspace", "a"),
        now,
    )
    .await;
    let exec_seq = at(
        &auth,
        "a",
        "user",
        Some("member"),
        "machine.exec",
        "failed",
        ("machine", "srv_1"),
        now,
    )
    .await;
    at(
        &auth,
        "a",
        "user",
        Some("member"),
        "machine.exec",
        "succeeded",
        ("machine", "srv_1"),
        now,
    )
    .await;
    at(
        &auth,
        "b",
        "user",
        Some("owner"),
        "agent.created",
        "succeeded",
        ("agent", "ag_b"),
        now,
    )
    .await;
    // A legacy row without the typed instant is invisible to the summary and
    // visible in coverage until the backfill marks completion.
    at(
        &auth,
        "a",
        "user",
        Some("owner"),
        "message.created",
        "succeeded",
        ("message", "legacy"),
        hour(now, 6),
    )
    .await;
    sqlx::query(
        "UPDATE organization_audit_events SET occurred_at_ts = NULL WHERE resource_id = 'legacy'",
    )
    .execute(&auth.pool())
    .await
    .unwrap();

    let router = test_router(&auth, PolicyEngine::allow_all());
    let denied = response(
        router.clone(),
        "/api/workspaces/a/audit-summary",
        Some("member"),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(denied.headers()[header::CACHE_CONTROL], "no-store");

    let ok = response(
        router.clone(),
        "/api/workspaces/a/audit-summary?range=24h",
        Some("owner"),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(ok.headers()[header::CACHE_CONTROL], "no-store");
    let summary = body(ok).await;
    assert_eq!(summary["category_table_version"], 1);
    assert_eq!(summary["range"]["bucket"], "hour");
    assert_eq!(summary["range"]["tz"], "UTC");
    assert_eq!(summary["totals"], json!({"events": 20, "failed": 2}));
    let categories = summary["categories"].as_array().unwrap();
    let names: Vec<&str> = categories
        .iter()
        .map(|c| c["category"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "agent_lifecycle",
            "messages",
            "tree",
            "policy",
            "apps_ingress",
            "machines_files",
            "access_admin",
            "other"
        ]
    );
    let count = |name: &str| {
        categories
            .iter()
            .find(|c| c["category"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(count("messages")["events"], 4);
    assert_eq!(count("agent_lifecycle")["events"], 12);
    assert_eq!(
        count("policy"),
        json!({"category": "policy", "events": 1, "failed": 1})
    );
    assert_eq!(
        count("machines_files"),
        json!({"category": "machines_files", "events": 2, "failed": 1})
    );
    assert_eq!(count("other")["events"], 1);
    assert_eq!(count("tree")["events"], 0);

    let series = summary["series"].as_array().unwrap();
    assert!(series.len() == 24 || series.len() == 25, "{}", series.len());
    let total: i64 = series.iter().map(|b| b["events"].as_i64().unwrap()).sum();
    assert_eq!(total, 20);
    let spike = series.iter().find(|b| b["events"] == 12).unwrap();
    assert_eq!(spike["by_category"], json!({"agent_lifecycle": 12}));
    assert_eq!(spike["failed"], 0);
    let first = &series[0];
    assert!(first["from"].as_str().unwrap() >= summary["range"]["since"].as_str().unwrap());
    assert!(first["bucket_start"].as_str().unwrap() <= first["from"].as_str().unwrap());
    assert!(series
        .windows(2)
        .all(|w| w[0]["bucket_end"] == w[1]["bucket_start"]));

    let anomalies = &summary["anomalies"];
    assert_eq!(anomalies["volume_evaluated"], true);
    assert_eq!(anomalies["failure_evaluated"], false);
    let items = anomalies["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["kind"], "volume_spike");
    assert_eq!(items[0]["observed"], 12);
    assert_eq!(items[0]["baseline"], 1.0);
    assert_eq!(items[0]["threshold"], 10.0);
    assert_eq!(items[0]["bucket_start"], spike["bucket_start"]);

    let actors = &summary["actors"];
    assert_eq!(actors["distinct"], 4);
    assert_eq!(actors["omitted_events"], 0);
    let top = actors["top"].as_array().unwrap();
    assert_eq!(top[0]["actor_id"], "ag_1");
    assert_eq!(top[0]["actor_kind"], "agent");
    assert_eq!(top[0]["events"], 12);
    assert_eq!(top[0]["drillable"], true);
    assert_eq!(top[1]["actor_id"], "owner");
    assert_eq!(
        top[1],
        json!({"actor_kind": "user", "actor_id": "owner", "drillable": true, "events": 5, "failed": 1})
    );
    let system = top.iter().find(|a| a["actor_kind"] == "system").unwrap();
    assert!(system.get("actor_id").is_none());
    assert_eq!(system["drillable"], false);

    let agents = &summary["resources"]["agent"];
    assert_eq!(agents["distinct"], 1);
    assert_eq!(
        agents["top"][0],
        json!({"resource_id": "ag_1", "drillable": true, "resource_name": "Builder", "events": 12, "failed": 0})
    );
    let machines = &summary["resources"]["machine"];
    assert_eq!(
        machines["top"][0],
        json!({"resource_id": "srv_1", "drillable": true, "resource_name": "Mac", "events": 2, "failed": 1})
    );
    assert_eq!(
        summary["resources"]["app"],
        json!({"top": [], "distinct": 0, "omitted_events": 0})
    );

    assert_eq!(summary["notable_total"], 2);
    assert!(summary.get("notable_capped").is_none());
    let notable = summary["notable"].as_array().unwrap();
    assert_eq!(notable.len(), 2);
    assert_eq!(notable[0]["sequence"], exec_seq);
    assert_eq!(notable[0]["category"], "machines_files");
    assert_eq!(notable[0]["resource_name"], "Mac");
    assert_eq!(notable[0]["outcome"], "failed");
    assert_eq!(notable[1]["sequence"], policy_seq);
    assert_eq!(notable[1]["category"], "policy");
    assert!(notable[1].get("resource_name").is_none());

    assert_eq!(summary["maintenance"]["retention_days"], 180);
    let coverage = &summary["coverage"];
    assert_eq!(coverage["backfill_complete"], false);
    assert!(coverage.get("complete_since").is_none());
    assert_eq!(
        coverage["oldest_in_range"],
        hour(now, 5).to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
    );

    // Backfill completes; the legacy row becomes visible.
    assert_eq!(
        crate::audit::maintenance::backfill_batch(&auth.pool())
            .await
            .unwrap(),
        1
    );
    let after = body(
        response(
            router.clone(),
            "/api/workspaces/a/audit-summary?range=24h",
            Some("owner"),
        )
        .await,
    )
    .await;
    assert_eq!(after["coverage"]["backfill_complete"], true);
    assert_eq!(after["totals"]["events"], 21);

    // Category and notable filters apply to the whole response.
    let messages = body(
        response(
            router.clone(),
            "/api/workspaces/a/audit-summary?range=24h&category=messages",
            Some("owner"),
        )
        .await,
    )
    .await;
    assert_eq!(messages["totals"]["events"], 5);
    assert_eq!(messages["filters"]["category"], "messages");
    assert_eq!(messages["actors"]["distinct"], 1);
    assert_eq!(messages["notable_total"], 0);
    let notable_only = body(
        response(
            router.clone(),
            "/api/workspaces/a/audit-summary?range=24h&notable=true",
            Some("owner"),
        )
        .await,
    )
    .await;
    assert_eq!(notable_only["totals"], json!({"events": 2, "failed": 2}));
    assert_eq!(notable_only["notable"].as_array().unwrap().len(), 2);
    let failed = body(
        response(
            router.clone(),
            "/api/workspaces/a/audit-summary?range=24h&outcome=failed&actor_kind=user",
            Some("owner"),
        )
        .await,
    )
    .await;
    assert_eq!(failed["totals"]["events"], 2);

    // Zones and day buckets: boundaries follow the zone.
    let shanghai = body(
        response(
            router.clone(),
            "/api/workspaces/a/audit-summary?range=7d&tz=Asia/Shanghai&bucket=day",
            Some("owner"),
        )
        .await,
    )
    .await;
    assert_eq!(shanghai["range"]["tz"], "Asia/Shanghai");
    assert_eq!(shanghai["range"]["bucket"], "day");
    let starts = shanghai["series"].as_array().unwrap();
    assert!(starts.len() >= 8 && starts.len() <= 9, "{}", starts.len());
    assert!(
        starts
            .iter()
            .all(|b| b["bucket_start"].as_str().unwrap().contains("T16:00:00")),
        "{starts:?}"
    );
    let total: i64 = starts.iter().map(|b| b["events"].as_i64().unwrap()).sum();
    assert_eq!(total, 21);

    for uri in [
        "/api/workspaces/a/audit-summary?tz=Mars/Base",
        "/api/workspaces/a/audit-summary?tz=Asia/Shanghai/x/y",
        "/api/workspaces/a/audit-summary?tz=a%20b",
        "/api/workspaces/a/audit-summary?range=7d&since=2026-01-01T00:00:00Z&until=2026-01-02T00:00:00Z",
        "/api/workspaces/a/audit-summary?since=2026-01-01T00:00:00Z",
        "/api/workspaces/a/audit-summary?since=2026-01-01T00:00:00Z&until=2026-08-01T00:00:00Z",
        "/api/workspaces/a/audit-summary?since=2026-01-01T00:00:00Z&until=2026-04-01T00:00:00Z&bucket=hour",
        "/api/workspaces/a/audit-summary?range=48h",
        "/api/workspaces/a/audit-summary?outcome=bogus",
        "/api/workspaces/a/audit-summary?actor_kind=alien",
        "/api/workspaces/a/audit-summary?resource_id=has%20space",
        "/api/workspaces/a/audit-summary?unknown=1",
    ] {
        let result = response(router.clone(), uri, Some("owner")).await;
        assert_eq!(result.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
    let explicit = body(
        response(
            router.clone(),
            &format!(
                "/api/workspaces/a/audit-summary?since={}&until={}",
                hour(now, 2).to_rfc3339().replace('+', "%2B"),
                (now + Duration::seconds(1))
                    .to_rfc3339()
                    .replace('+', "%2B")
            ),
            Some("owner"),
        )
        .await,
    )
    .await;
    assert_eq!(explicit["totals"]["events"], 17, "{explicit}");
    assert_eq!(explicit["series"].as_array().unwrap().len(), 3);

    // Per-caller rate limit, then the next caller is unaffected.
    let mut limited = None;
    for _ in 0..SUMMARIES_PER_CALLER_PER_MINUTE {
        let result = response(
            router.clone(),
            "/api/workspaces/a/audit-summary",
            Some("owner"),
        )
        .await;
        if result.status() == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(body(result).await);
            break;
        }
    }
    let limited = limited.expect("rate limit reached within the budget");
    assert_eq!(limited["error"]["code"], "audit_summary_rate_limited");
    assert_eq!(
        response(
            router.clone(),
            "/api/workspaces/a/audit-summary",
            Some("local-owner")
        )
        .await
        .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn audit_list_shares_the_summary_filters() {
    let auth = fixture().await;
    let now = micros_now() - Duration::seconds(5);
    at(
        &auth,
        "a",
        "agent",
        Some("ag_1"),
        "agent.started",
        "succeeded",
        ("agent", "ag_1"),
        now,
    )
    .await;
    let policy_seq = at(
        &auth,
        "a",
        "user",
        Some("owner"),
        "policy.rule.changed",
        "failed",
        ("workspace", "a"),
        now,
    )
    .await;
    at(
        &auth,
        "a",
        "user",
        Some("member"),
        "machine.exec",
        "failed",
        ("machine", "srv_1"),
        now,
    )
    .await;
    at(
        &auth,
        "a",
        "user",
        Some("member"),
        "machine.exec",
        "succeeded",
        ("machine", "srv_1"),
        now,
    )
    .await;
    at(
        &auth,
        "a",
        "system",
        None,
        "weird.thing",
        "failed",
        ("workspace", "a"),
        now,
    )
    .await;
    let router = test_router(&auth, PolicyEngine::allow_all());
    let count = |uri: &'static str| {
        let router = router.clone();
        async move {
            let result = response(router, uri, Some("owner")).await;
            assert_eq!(result.status(), StatusCode::OK, "{uri}");
            let page = body(result).await;
            assert_eq!(page["category_table_version"], 1);
            page["events"].as_array().unwrap().len()
        }
    };
    assert_eq!(count("/api/workspaces/a/audit-events").await, 5);
    assert_eq!(
        count("/api/workspaces/a/audit-events?category=policy").await,
        1
    );
    assert_eq!(
        count("/api/workspaces/a/audit-events?category=machines_files").await,
        2
    );
    assert_eq!(
        count("/api/workspaces/a/audit-events?category=other").await,
        1
    );
    // Notable: failed policy, failed machine.exec, failed other.
    assert_eq!(
        count("/api/workspaces/a/audit-events?notable=true").await,
        3
    );
    assert_eq!(
        count("/api/workspaces/a/audit-events?notable=true&category=machines_files").await,
        1
    );
    assert_eq!(
        count("/api/workspaces/a/audit-events?actor_kind=agent").await,
        1
    );
    assert_eq!(
        count("/api/workspaces/a/audit-events?outcome=failed").await,
        3
    );
    assert_eq!(
        count("/api/workspaces/a/audit-events?resource_id=srv_1").await,
        2
    );
    assert_eq!(
        count("/api/workspaces/a/audit-events?resource_id=srv_1&outcome=succeeded").await,
        1
    );
    let one = body(
        response(
            router.clone(),
            &format!("/api/workspaces/a/audit-events?sequence={policy_seq}"),
            Some("owner"),
        )
        .await,
    )
    .await;
    assert_eq!(one["events"].as_array().unwrap().len(), 1);
    assert_eq!(one["events"][0]["action"], "policy.rule.changed");
    for uri in [
        "/api/workspaces/a/audit-events?outcome=bogus",
        "/api/workspaces/a/audit-events?actor_kind=alien",
        "/api/workspaces/a/audit-events?sequence=0",
        "/api/workspaces/a/audit-events?category=nonsense",
        "/api/workspaces/a/audit-events?notable=maybe",
    ] {
        assert_eq!(
            response(router.clone(), uri, Some("owner")).await.status(),
            StatusCode::BAD_REQUEST,
            "{uri}"
        );
    }
}

/// The SQL category and notable expressions used by the list route must agree
/// with the Rust rule for every table entry, prefix derivative and near miss.
#[tokio::test]
async fn audit_category_sql_matches_the_rust_rule() {
    let auth = fixture().await;
    let mut probes: Vec<String> = treer_protocol::AUDIT_CATEGORY_EXACT
        .iter()
        .map(|(a, _)| (*a).to_owned())
        .collect();
    for (prefix, _) in treer_protocol::AUDIT_CATEGORY_PREFIXES {
        probes.push((*prefix).to_owned());
        probes.push(format!("{prefix}."));
        probes.push(format!("{prefix}.x"));
        probes.push(format!("{prefix}.x.y"));
        probes.push(format!("{prefix}x.y"));
        probes.push(format!("{prefix}_x.y"));
        probes.push(prefix[..prefix.len() - 1].to_owned());
        probes.push(format!("{}.x", &prefix[..prefix.len() - 1]));
    }
    for action in treer_protocol::NOTABLE_ACTIONS
        .iter()
        .chain(treer_protocol::NOTABLE_WHEN_FAILED)
    {
        probes.push((*action).to_owned());
        probes.push(format!("{action}.x"));
    }
    probes.extend(
        [
            "",
            ".",
            "x",
            "machine.file",
            "machine.filex.write",
            "policy_provider.x",
            "ingress.httpx",
            "ingress.http.x",
            "application.x",
            "app",
            "weird.thing",
        ]
        .map(str::to_owned),
    );
    let sql = format!(
        "SELECT {} AS category, {} AS notable FROM (SELECT $22::TEXT AS action, $23::TEXT AS outcome) e \
         WHERE COALESCE($1::TEXT, $3, $4, $5, $6, $7, $9, $10, $11, $13) IS NULL \
         AND COALESCE($2::BIGINT, $8, $12) IS NULL AND NOT $14::BOOLEAN",
        crate::audit::CATEGORY_SQL,
        crate::audit::NOTABLE_SQL
    );
    for action in &probes {
        for outcome in ["succeeded", "failed"] {
            let statement = sqlx::query(&sql)
                .bind(None::<String>)
                .bind(None::<i64>)
                .bind(None::<String>)
                .bind(None::<String>)
                .bind(None::<String>)
                .bind(None::<String>)
                .bind(None::<String>)
                .bind(None::<i64>)
                .bind(None::<String>)
                .bind(None::<String>)
                .bind(None::<String>)
                .bind(None::<i64>)
                .bind(None::<String>)
                .bind(false);
            let row = crate::audit::bind_category_terms(statement)
                .bind(action)
                .bind(outcome)
                .fetch_one(&auth.pool())
                .await
                .unwrap();
            let category: String = row.get("category");
            let notable: bool = row.get("notable");
            assert_eq!(
                category,
                audit_category(action).as_str(),
                "category of {action:?}"
            );
            assert_eq!(
                notable,
                is_notable(action, outcome == "failed"),
                "notable of {action:?} {outcome}"
            );
        }
    }
    assert!(probes.len() > 150);
}

#[tokio::test]
async fn audit_instant_trigger_and_backfill_touch_only_the_typed_column() {
    let auth = fixture().await;
    let now = micros_now();
    // The trigger fills new rows and marks malformed text without failing.
    let good = at(
        &auth,
        "a",
        "user",
        Some("owner"),
        "agent.created",
        "succeeded",
        ("agent", "ag_1"),
        now,
    )
    .await;
    let (ts, unparsable): (Option<i64>, bool) = sqlx::query_as(
        "SELECT (extract(epoch FROM occurred_at_ts)*1000000)::BIGINT, occurred_at_ts_unparsable \
         FROM organization_audit_events WHERE sequence = $1",
    )
    .bind(good)
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(ts, Some(now.timestamp_micros()));
    assert!(!unparsable);
    sqlx::query("INSERT INTO organization_audit_events(event_id, organization_id, workspace_id, occurred_at, actor_kind, source, action, outcome, resource_kind, resource_id) \
                 VALUES('aud_bad', 'org_a', 'a', 'not a time', 'system', 'test', 'agent.created', 'succeeded', 'agent', 'ag_1')")
        .execute(&auth.pool()).await.unwrap();
    let (ts, unparsable): (Option<i64>, bool) = sqlx::query_as(
        "SELECT (extract(epoch FROM occurred_at_ts)*1000000)::BIGINT, occurred_at_ts_unparsable \
         FROM organization_audit_events WHERE event_id = 'aud_bad'",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(ts, None);
    assert!(unparsable);
    // Nanosecond text (what a Linux recorder writes) is rounded to the
    // microsecond by the database, consistently with the text cast the feed's
    // range filters use; the Rust side never compares such values exactly.
    sqlx::query("INSERT INTO organization_audit_events(event_id, organization_id, workspace_id, occurred_at, actor_kind, source, action, outcome, resource_kind, resource_id) \
                 VALUES('aud_nanos', 'org_a', 'a', '2026-10-19T03:34:00.162180369Z', 'system', 'test', 'agent.created', 'succeeded', 'agent', 'ag_1')")
        .execute(&auth.pool()).await.unwrap();
    let (nanos_ts, same_as_cast): (Option<i64>, bool) = sqlx::query_as(
        "SELECT (extract(epoch FROM occurred_at_ts)*1000000)::BIGINT, occurred_at_ts = occurred_at::TIMESTAMPTZ \
         FROM organization_audit_events WHERE event_id = 'aud_nanos'",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(
        nanos_ts,
        Some(
            DateTime::parse_from_rfc3339("2026-10-19T03:34:00.162180Z")
                .unwrap()
                .timestamp_micros()
        )
    );
    assert!(same_as_cast);

    // 300 legacy rows (no typed instant) plus one malformed legacy row.
    sqlx::query(
        "INSERT INTO organization_audit_events(event_id, organization_id, workspace_id, occurred_at, actor_kind, actor_id, source, action, outcome, resource_kind, resource_id, payload) \
         SELECT 'aud_legacy_' || i, 'org_a', 'a', to_char(($1::TEXT::TIMESTAMPTZ - (i || ' minutes')::INTERVAL) AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'), \
         'user', 'owner', 'test', 'message.created', 'succeeded', 'message', 'm' || i, jsonb_build_object('i', i) FROM generate_series(1, 300) i",
    )
    .bind(now.to_rfc3339())
    .execute(&auth.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE organization_audit_events SET occurred_at_ts = NULL, occurred_at_ts_unparsable = FALSE WHERE event_id LIKE 'aud_legacy_%' OR event_id = 'aud_bad'")
        .execute(&auth.pool()).await.unwrap();
    const ROW_HASH: &str = "SELECT sequence, md5(row(sequence, event_id, schema_version, organization_id, workspace_id, occurred_at, \
        actor_kind, actor_id, actor_name, source, action, outcome, resource_kind, resource_id, resource_name, correlation_id, \
        payload, workspace_origin_id)::TEXT) FROM organization_audit_events ORDER BY sequence";
    let before: Vec<(i64, String)> = sqlx::query_as(ROW_HASH)
        .fetch_all(&auth.pool())
        .await
        .unwrap();

    // First batch: the 256 newest legacy rows (by sequence) get the instant.
    assert_eq!(
        crate::audit::maintenance::backfill_batch(&auth.pool())
            .await
            .unwrap(),
        256
    );
    let still_null: Vec<i64> = sqlx::query_scalar(
        "SELECT sequence FROM organization_audit_events WHERE occurred_at_ts IS NULL AND NOT occurred_at_ts_unparsable ORDER BY sequence",
    )
    .fetch_all(&auth.pool())
    .await
    .unwrap();
    assert_eq!(still_null.len(), 301 - 256);
    let max_null = *still_null.iter().max().unwrap();
    let filled_min: i64 = sqlx::query_scalar(
        "SELECT MIN(sequence) FROM organization_audit_events WHERE event_id LIKE 'aud_legacy_%' AND occurred_at_ts IS NOT NULL",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert!(max_null < filled_min, "newest rows are filled first");
    let (done, watermark, complete_since): (bool, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT backfill_done, backfill_watermark_sequence, (extract(epoch FROM backfill_complete_since)*1000000)::BIGINT FROM audit_maintenance_settings",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert!(!done);
    assert_eq!(watermark, Some(filled_min));
    let oldest_filled: i64 = sqlx::query_scalar(
        "SELECT (extract(epoch FROM MIN(occurred_at_ts))*1000000)::BIGINT FROM organization_audit_events WHERE event_id LIKE 'aud_legacy_%'",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(complete_since, Some(oldest_filled));

    // Second batch finishes; the malformed row is marked, never retried.
    assert_eq!(
        crate::audit::maintenance::backfill_batch(&auth.pool())
            .await
            .unwrap(),
        45
    );
    assert_eq!(
        crate::audit::maintenance::backfill_batch(&auth.pool())
            .await
            .unwrap(),
        0
    );
    let (done, remaining, marked): (bool, i64, i64) = sqlx::query_as(
        "SELECT (SELECT backfill_done FROM audit_maintenance_settings), \
         (SELECT COUNT(*) FROM organization_audit_events WHERE occurred_at_ts IS NULL AND NOT occurred_at_ts_unparsable), \
         (SELECT COUNT(*) FROM organization_audit_events WHERE occurred_at_ts_unparsable)",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert!(done);
    assert_eq!(remaining, 0);
    assert_eq!(marked, 1);
    // Every parsed instant equals the text column's value.
    let mismatches: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM organization_audit_events WHERE occurred_at_ts IS NOT NULL AND occurred_at_ts <> occurred_at::TIMESTAMPTZ",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(mismatches, 0);
    let after: Vec<(i64, String)> = sqlx::query_as(ROW_HASH)
        .fetch_all(&auth.pool())
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "backfill changed a column other than the typed instant"
    );
}

/// The range scan can use the workspace/time index: with sequential scans
/// disabled the planner picks it for the summary's scope predicate.
#[tokio::test]
async fn audit_summary_scope_is_served_by_the_workspace_time_index() {
    let auth = fixture().await;
    let now = micros_now();
    // A year of events, so a narrow range is selective enough for the planner
    // to prefer the time index over the workspace/sequence index.
    sqlx::query(
        "INSERT INTO organization_audit_events(event_id, organization_id, workspace_id, occurred_at, actor_kind, actor_id, source, action, outcome, resource_kind, resource_id, workspace_origin_id) \
         SELECT 'aud_spread_' || i, 'org_a', 'a', to_char(($1::TEXT::TIMESTAMPTZ - (i * 100 || ' minutes')::INTERVAL) AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'), \
         'user', 'owner', 'test', 'message.created', 'succeeded', 'message', 'm' || i, 'a' FROM generate_series(1, 5000) i",
    )
    .bind(now.to_rfc3339())
    .execute(&auth.pool())
    .await
    .unwrap();
    sqlx::query("ANALYZE organization_audit_events")
        .execute(&auth.pool())
        .await
        .unwrap();
    let mut tx = auth.pool().begin().await.unwrap();
    let sql = format!(
        "EXPLAIN (FORMAT TEXT) SELECT COUNT(*) {}",
        crate::audit::summary::scope_sql("")
    );
    let plan: Vec<String> = sqlx::query_scalar(&sql)
        .bind("a")
        .bind((now - Duration::hours(1)).to_rfc3339())
        .bind(now.to_rfc3339())
        .bind(None::<String>)
        .bind(None::<String>)
        .bind(None::<String>)
        .bind(None::<String>)
        .bind(None::<String>)
        .bind(None::<String>)
        .bind(None::<Vec<String>>)
        .bind(false)
        .bind(None::<Vec<String>>)
        .bind(None::<Vec<String>>)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    let plan = plan.join("\n");
    assert!(
        plan.contains("organization_audit_events_workspace_time"),
        "{plan}"
    );
    assert!(plan.contains("Index Cond"), "{plan}");
}

/// Every action a Proxy recorder writes must be in the category table (#192);
/// `other` is for drift, not for known actions.
#[test]
fn recorded_audit_actions_are_all_categorized() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    let mut literals = std::collections::BTreeSet::new();
    for file in &files {
        let name = file.to_string_lossy();
        if name.contains("test") {
            continue;
        }
        let text = std::fs::read_to_string(file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            let window: Vec<&str> = if trimmed.starts_with("action: ")
                || trimmed.starts_with("action = ")
                || trimmed.starts_with("Self::")
                    && lines[..index]
                        .iter()
                        .rev()
                        .take(6)
                        .any(|l| l.contains("fn audit_action"))
            {
                vec![line]
            } else if [
                "insert_workspace_mutation(",
                "mutation_best_effort(",
                "record_app_audit(",
                "record_policy_provider_audit(",
            ]
            .iter()
            .any(|needle| line.contains(needle))
            {
                lines[index..(index + 6).min(lines.len())].to_vec()
            } else {
                continue;
            };
            for candidate in window {
                if candidate.contains("resource_kind") {
                    continue;
                }
                for literal in dotted_literals(candidate) {
                    literals.insert(literal);
                }
            }
        }
    }
    assert!(
        literals.len() >= 30,
        "scan found too few actions: {literals:?}"
    );
    let uncategorized: Vec<&String> = literals
        .iter()
        .filter(|action| audit_category(action) == AuditCategory::Other)
        .collect();
    assert!(
        uncategorized.is_empty(),
        "actions missing from the category table: {uncategorized:?}"
    );
}

fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn dotted_literals(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else { break };
        let literal = &after[..end];
        if literal.contains('.')
            && literal
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'.')
            && !literal.starts_with('.')
            && !literal.ends_with('.')
        {
            out.push(literal.to_owned());
        }
        rest = &after[end + 1..];
    }
    out
}

/// More distinct action names than the cap is a closed failure, never a
/// partial document (security note b on daad71fe).
#[tokio::test]
async fn audit_summary_refuses_more_action_groups_than_the_cap() {
    let auth = fixture().await;
    let now = micros_now();
    sqlx::query(
        "INSERT INTO organization_audit_events(event_id, organization_id, workspace_id, occurred_at, actor_kind, actor_id, source, action, outcome, resource_kind, resource_id, workspace_origin_id) \
         SELECT 'aud_many_' || i, 'org_a', 'a', $1, 'user', 'owner', 'test', 'synthetic.action_' || i, 'succeeded', 'workspace', 'a', 'a' \
         FROM generate_series(1, $2) i",
    )
    .bind(now.to_rfc3339())
    .bind(crate::audit::summary::ACTION_GROUPS_CAP as i32 + 1)
    .execute(&auth.pool())
    .await
    .unwrap();
    let router = test_router(&auth, PolicyEngine::allow_all());
    let result = response(
        router.clone(),
        "/api/workspaces/a/audit-summary",
        Some("owner"),
    )
    .await;
    assert_eq!(result.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body(result).await["error"]["code"],
        "audit_summary_unavailable"
    );
    // One group fewer is served in full.
    sqlx::query("DELETE FROM organization_audit_events WHERE event_id = 'aud_many_1'")
        .execute(&auth.pool())
        .await
        .unwrap();
    let ok = body(response(router, "/api/workspaces/a/audit-summary", Some("owner")).await).await;
    assert_eq!(
        ok["totals"]["events"],
        crate::audit::summary::ACTION_GROUPS_CAP
    );
}
