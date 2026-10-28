//! Audit summary route, shared list filters, category parity and backfill
//! (#191, #192). PostgreSQL-backed like the sibling module.
use super::tests::{body, fixture, owner_get, response, test_router};
use super::*;
use chrono::{DateTime, Duration, Utc};
use sqlx::Row;
use treer_protocol::{audit_category, is_notable, AuditCategory};

/// Inserts one event. `spec` is "workspace actor_kind actor action outcome
/// resource_kind resource_id", with `-` for no actor; returns its sequence.
async fn at(auth: &AuthStore, spec: &str, when: DateTime<Utc>) -> i64 {
    let f: Vec<&str> = spec.split_whitespace().collect();
    let [workspace, actor_kind, actor, action, outcome, kind, id] = f[..] else {
        panic!("bad spec {spec}");
    };
    sqlx::query_scalar(
        "INSERT INTO organization_audit_events(event_id, organization_id, workspace_id, occurred_at, \
         actor_kind, actor_id, source, action, outcome, resource_kind, resource_id, payload, workspace_origin_id) \
         VALUES($1, 'org_a', $2, $3, $4, $5, 'test', $6, $7, $8, $9, '{}'::JSONB, $2) RETURNING sequence",
    )
    .bind(format!("aud_{}", uuid::Uuid::new_v4().simple()))
    .bind(workspace)
    .bind(when.to_rfc3339())
    .bind(actor_kind)
    .bind((actor != "-").then_some(actor))
    .bind(action)
    .bind(outcome)
    .bind(kind)
    .bind(id)
    .fetch_one(&auth.pool())
    .await
    .unwrap()
}

fn hour(now: DateTime<Utc>, hours: i64) -> DateTime<Utc> {
    now - Duration::hours(hours)
}

/// PostgreSQL stores microseconds; `Utc::now()` carries nanoseconds on Linux,
/// so every instant a test compares with a stored value is truncated first.
fn micros_now() -> DateTime<Utc> {
    DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap()
}

const SUMMARY: &str = "/api/workspaces/a/audit-summary";

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
            "a user owner message.created succeeded message m1",
            hour(now, h),
        )
        .await;
    }
    for _ in 0..12 {
        at(
            &auth,
            "a agent ag_1 agent.started succeeded agent ag_1",
            hour(now, 1),
        )
        .await;
    }
    let policy_seq = at(
        &auth,
        "a user owner policy.rule.changed failed workspace a",
        now,
    )
    .await;
    at(&auth, "a system - weird.thing succeeded workspace a", now).await;
    let exec_seq = at(
        &auth,
        "a user member machine.exec failed machine srv_1",
        now,
    )
    .await;
    at(
        &auth,
        "a user member machine.exec succeeded machine srv_1",
        now,
    )
    .await;
    at(
        &auth,
        "b user owner agent.created succeeded agent ag_b",
        now,
    )
    .await;
    // A legacy row without the typed instant is invisible to the summary and
    // visible in coverage until the backfill marks completion.
    at(
        &auth,
        "a user owner message.created succeeded message legacy",
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
    let denied = response(router.clone(), SUMMARY, Some("member")).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(denied.headers()[header::CACHE_CONTROL], "no-store");

    let summary = owner_get(&router, &format!("{SUMMARY}?range=24h")).await;
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
        count("machines_files"),
        json!({"category": "machines_files", "events": 2, "failed": 1})
    );
    assert_eq!(count("tree")["events"], 0);

    let series = summary["series"].as_array().unwrap();
    assert!(series.len() == 24 || series.len() == 25, "{}", series.len());
    let total: i64 = series.iter().map(|b| b["events"].as_i64().unwrap()).sum();
    assert_eq!(total, 20);
    let spike = series.iter().find(|b| b["events"] == 12).unwrap();
    assert_eq!(spike["by_category"], json!({"agent_lifecycle": 12}));
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
    assert_eq!(items[0]["bucket_start"], spike["bucket_start"]);

    let actors = &summary["actors"];
    assert_eq!(actors["distinct"], 4);
    let top = actors["top"].as_array().unwrap();
    assert_eq!(
        (&top[0]["actor_id"], &top[0]["events"], &top[0]["drillable"]),
        (&json!("ag_1"), &json!(12), &json!(true))
    );
    assert_eq!(
        top[1],
        json!({"actor_kind": "user", "actor_id": "owner", "drillable": true, "events": 5, "failed": 1})
    );
    let system = top.iter().find(|a| a["actor_kind"] == "system").unwrap();
    assert!(system.get("actor_id").is_none());
    assert_eq!(system["drillable"], false);
    assert_eq!(
        summary["resources"]["agent"]["top"][0],
        json!({"resource_id": "ag_1", "drillable": true, "resource_name": "Builder", "events": 12, "failed": 0})
    );
    assert_eq!(
        summary["resources"]["app"],
        json!({"top": [], "distinct": 0, "omitted_events": 0})
    );

    assert_eq!(summary["notable_total"], 2);
    let notable = summary["notable"].as_array().unwrap();
    assert_eq!(notable[0]["sequence"], exec_seq);
    assert_eq!(notable[0]["category"], "machines_files");
    assert_eq!(notable[0]["resource_name"], "Mac");
    assert_eq!(notable[1]["sequence"], policy_seq);
    assert!(notable[1].get("resource_name").is_none());

    assert_eq!(summary["maintenance"]["retention_days"], 180);
    assert_eq!(summary["coverage"]["backfill_complete"], false);
    assert_eq!(
        summary["coverage"]["oldest_in_range"],
        hour(now, 5).to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
    );

    // Backfill completes; the legacy row becomes visible.
    assert_eq!(
        crate::audit::maintenance::backfill_batch(&auth.pool())
            .await
            .unwrap(),
        1
    );
    let after = owner_get(&router, &format!("{SUMMARY}?range=24h")).await;
    assert_eq!(after["coverage"]["backfill_complete"], true);
    assert_eq!(after["totals"]["events"], 21);

    // Filters apply to the whole response.
    let messages = owner_get(&router, &format!("{SUMMARY}?range=24h&category=messages")).await;
    assert_eq!(messages["totals"]["events"], 5);
    assert_eq!(messages["filters"]["category"], "messages");
    assert_eq!(messages["notable_total"], 0);
    let notable_only = owner_get(&router, &format!("{SUMMARY}?range=24h&notable=true")).await;
    assert_eq!(notable_only["totals"], json!({"events": 2, "failed": 2}));
    let failed = owner_get(
        &router,
        &format!("{SUMMARY}?range=24h&outcome=failed&actor_kind=user"),
    )
    .await;
    assert_eq!(failed["totals"]["events"], 2);

    // Zones and day buckets: boundaries follow the zone.
    let shanghai = owner_get(
        &router,
        &format!("{SUMMARY}?range=7d&tz=Asia/Shanghai&bucket=day"),
    )
    .await;
    assert_eq!(shanghai["range"]["tz"], "Asia/Shanghai");
    let days = shanghai["series"].as_array().unwrap();
    assert!(days.len() >= 8 && days.len() <= 9, "{}", days.len());
    assert!(days
        .iter()
        .all(|b| b["bucket_start"].as_str().unwrap().contains("T16:00:00")));

    for query in [
        "tz=Mars/Base",
        "tz=a%20b",
        "range=7d&since=2026-01-01T00:00:00Z&until=2026-01-02T00:00:00Z",
        "since=2026-01-01T00:00:00Z",
        "since=2026-01-01T00:00:00Z&until=2026-08-01T00:00:00Z",
        "since=2026-01-01T00:00:00Z&until=2026-04-01T00:00:00Z&bucket=hour",
        "range=48h",
        "outcome=bogus",
        "actor_kind=alien",
        "resource_id=has%20space",
        "unknown=1",
    ] {
        let result = response(router.clone(), &format!("{SUMMARY}?{query}"), Some("owner")).await;
        assert_eq!(result.status(), StatusCode::BAD_REQUEST, "{query}");
    }
    let explicit = owner_get(
        &router,
        &format!(
            "{SUMMARY}?since={}&until={}",
            hour(now, 2).to_rfc3339().replace('+', "%2B"),
            (now + Duration::seconds(1))
                .to_rfc3339()
                .replace('+', "%2B")
        ),
    )
    .await;
    assert_eq!(explicit["totals"]["events"], 17, "{explicit}");
    assert_eq!(explicit["series"].as_array().unwrap().len(), 3);

    // Per-caller rate limit, then the next caller is unaffected.
    let mut limited = None;
    for _ in 0..SUMMARIES_PER_CALLER_PER_MINUTE {
        let result = response(router.clone(), SUMMARY, Some("owner")).await;
        if result.status() == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(body(result).await);
            break;
        }
    }
    let limited = limited.expect("rate limit reached within the budget");
    assert_eq!(limited["error"]["code"], "audit_summary_rate_limited");
    assert_eq!(
        response(router.clone(), SUMMARY, Some("local-owner"))
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
        "a agent ag_1 agent.started succeeded agent ag_1",
        now,
    )
    .await;
    let policy_seq = at(
        &auth,
        "a user owner policy.rule.changed failed workspace a",
        now,
    )
    .await;
    at(
        &auth,
        "a user member machine.exec failed machine srv_1",
        now,
    )
    .await;
    at(
        &auth,
        "a user member machine.exec succeeded machine srv_1",
        now,
    )
    .await;
    at(&auth, "a system - weird.thing failed workspace a", now).await;
    let router = test_router(&auth, PolicyEngine::allow_all());
    for (query, expected) in [
        ("", 5),
        ("category=policy", 1),
        ("category=machines_files", 2),
        ("category=other", 1),
        // Notable: failed policy, failed machine.exec, failed other.
        ("notable=true", 3),
        ("notable=true&category=machines_files", 1),
        ("actor_kind=agent", 1),
        ("outcome=failed", 3),
        ("resource_id=srv_1", 2),
        ("resource_id=srv_1&outcome=succeeded", 1),
    ] {
        let page = owner_get(&router, &format!("/api/workspaces/a/audit-events?{query}")).await;
        assert_eq!(page["category_table_version"], 1);
        assert_eq!(
            page["events"].as_array().unwrap().len(),
            expected,
            "{query}"
        );
    }
    let one = owner_get(
        &router,
        &format!("/api/workspaces/a/audit-events?sequence={policy_seq}"),
    )
    .await;
    assert_eq!(one["events"].as_array().unwrap().len(), 1);
    assert_eq!(one["events"][0]["action"], "policy.rule.changed");
    for query in [
        "outcome=bogus",
        "actor_kind=alien",
        "sequence=0",
        "category=nonsense",
        "notable=maybe",
    ] {
        let uri = format!("/api/workspaces/a/audit-events?{query}");
        assert_eq!(
            response(router.clone(), &uri, Some("owner")).await.status(),
            StatusCode::BAD_REQUEST,
            "{query}"
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
        let short = &prefix[..prefix.len() - 1];
        probes.extend([
            (*prefix).to_owned(),
            format!("{prefix}."),
            format!("{prefix}.x"),
            format!("{prefix}.x.y"),
            format!("{prefix}x.y"),
            format!("{prefix}_x.y"),
            short.to_owned(),
            format!("{short}.x"),
        ]);
    }
    for action in treer_protocol::NOTABLE_ACTIONS
        .iter()
        .chain(treer_protocol::NOTABLE_WHEN_FAILED)
    {
        probes.extend([(*action).to_owned(), format!("{action}.x")]);
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
    // $1..$14 are the list filters (all unset); the category terms follow.
    let sql = format!(
        "SELECT {} AS category, {} AS notable FROM (SELECT $22::TEXT AS action, $23::TEXT AS outcome) e \
         WHERE COALESCE($1::TEXT, $3, $4, $5, $6, $7, $9, $10, $11, $13) IS NULL \
         AND COALESCE($2::BIGINT, $8, $12) IS NULL AND NOT $14::BOOLEAN",
        crate::audit::CATEGORY_SQL,
        crate::audit::NOTABLE_SQL
    );
    for action in &probes {
        for outcome in ["succeeded", "failed"] {
            let mut statement = sqlx::query(&sql);
            for index in 1..=13 {
                statement = if [2, 8, 12].contains(&index) {
                    statement.bind(None::<i64>)
                } else {
                    statement.bind(None::<String>)
                };
            }
            let row = crate::audit::bind_category_terms(statement.bind(false))
                .bind(action)
                .bind(outcome)
                .fetch_one(&auth.pool())
                .await
                .unwrap();
            assert_eq!(
                row.get::<String, _>("category"),
                audit_category(action).as_str(),
                "category of {action:?}"
            );
            assert_eq!(
                row.get::<bool, _>("notable"),
                is_notable(action, outcome == "failed"),
                "notable of {action:?} {outcome}"
            );
        }
    }
    assert!(probes.len() > 150);
}

/// The insert trigger fills the typed instant (marking malformed text), and
/// the backfill finishes legacy rows in batches without touching any other
/// column.
#[tokio::test]
async fn audit_instant_trigger_and_backfill_touch_only_the_typed_column() {
    let auth = fixture().await;
    let now = micros_now();
    let good = at(
        &auth,
        "a user owner agent.created succeeded agent ag_1",
        now,
    )
    .await;
    let ts: Option<i64> = sqlx::query_scalar(
        "SELECT (extract(epoch FROM occurred_at_ts)*1000000)::BIGINT FROM organization_audit_events WHERE sequence = $1",
    )
    .bind(good)
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(ts, Some(now.timestamp_micros()));
    sqlx::query("INSERT INTO organization_audit_events(event_id, organization_id, workspace_id, occurred_at, actor_kind, source, action, outcome, resource_kind, resource_id) \
                 VALUES('aud_bad', 'org_a', 'a', 'not a time', 'system', 'test', 'agent.created', 'succeeded', 'agent', 'ag_1')")
        .execute(&auth.pool()).await.unwrap();
    // 300 legacy rows (no typed instant) plus the malformed row.
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
    for expected in [256, 45, 0] {
        assert_eq!(
            crate::audit::maintenance::backfill_batch(&auth.pool())
                .await
                .unwrap(),
            expected
        );
    }
    let (done, remaining, marked, mismatches): (bool, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT backfill_done FROM audit_maintenance_settings), \
         (SELECT COUNT(*) FROM organization_audit_events WHERE occurred_at_ts IS NULL AND NOT occurred_at_ts_unparsable), \
         (SELECT COUNT(*) FROM organization_audit_events WHERE occurred_at_ts_unparsable), \
         (SELECT COUNT(*) FROM organization_audit_events WHERE occurred_at_ts IS NOT NULL AND occurred_at_ts <> occurred_at::TIMESTAMPTZ)",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert!(done);
    assert_eq!((remaining, marked, mismatches), (0, 1, 0));
    let after: Vec<(i64, String)> = sqlx::query_as(ROW_HASH)
        .fetch_all(&auth.pool())
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "backfill changed a column other than the typed instant"
    );
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
    sqlx::query(
        "INSERT INTO organization_audit_events(event_id, organization_id, workspace_id, occurred_at, actor_kind, actor_id, source, action, outcome, resource_kind, resource_id, workspace_origin_id) \
         SELECT 'aud_many_' || i, 'org_a', 'a', $1, 'user', 'owner', 'test', 'synthetic.action_' || i, 'succeeded', 'workspace', 'a', 'a' \
         FROM generate_series(1, $2) i",
    )
    .bind(micros_now().to_rfc3339())
    .bind(crate::audit::summary::ACTION_GROUPS_CAP as i32 + 1)
    .execute(&auth.pool())
    .await
    .unwrap();
    let router = test_router(&auth, PolicyEngine::allow_all());
    let result = response(router.clone(), SUMMARY, Some("owner")).await;
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
    let ok = owner_get(&router, SUMMARY).await;
    assert_eq!(
        ok["totals"]["events"],
        crate::audit::summary::ACTION_GROUPS_CAP
    );
}
