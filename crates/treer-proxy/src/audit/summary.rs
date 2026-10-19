//! Workspace audit summary (#191): bounded aggregates over `occurred_at_ts`
//! for the Console's charts. Categories are mapped in Rust from the versioned
//! protocol table; SQL only groups by action and joins the mapping back as a
//! bound pair array, so Rust and SQL cannot disagree on a category.
use super::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use treer_protocol::{
    audit_category, is_notable, AuditAnomalyKind, AuditCategory, AuditSummaryActor,
    AuditSummaryAnomalies, AuditSummaryAnomaly, AuditSummaryBucket, AuditSummaryBucketRow,
    AuditSummaryCategoryCount, AuditSummaryCount, AuditSummaryCoverage, AuditSummaryFilters,
    AuditSummaryNotable, AuditSummaryRange, AuditSummaryRangeEcho, AuditSummaryRanking,
    AuditSummaryResource, AuditSummaryResources, WorkspaceAuditSummary, WorkspaceAuditSummaryQuery,
    AUDIT_CATEGORY_TABLE_VERSION,
};

/// Longest explicit range.
pub(crate) const MAX_RANGE_DAYS: i64 = 180;
/// Most buckets one response may carry (31 days of hours).
pub(crate) const MAX_BUCKETS: i64 = 744;
const TOP_N: i64 = 20;
const NOTABLE_LIMIT: i64 = 100;
const NOTABLE_PER_ACTION: i64 = 20;
/// Longest id the list route accepts as a filter; longer ids are shown but not
/// drillable.
const DRILLABLE_ID_BYTES: usize = 128;

#[derive(Debug, Clone)]
pub(crate) struct ResolvedRange {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub bucket: AuditSummaryBucket,
    pub tz: String,
}

#[derive(Debug)]
pub(crate) enum RangeError {
    Invalid(&'static str),
}

/// Validates and completes the range part of the query (no database access).
pub(crate) fn resolve_range(
    query: &WorkspaceAuditSummaryQuery,
    now: DateTime<Utc>,
) -> Result<ResolvedRange, RangeError> {
    let (since, until) = match (query.range, query.since, query.until) {
        (Some(range), None, None) => {
            let span = match range {
                AuditSummaryRange::Day => chrono::Duration::hours(24),
                AuditSummaryRange::Week => chrono::Duration::days(7),
                AuditSummaryRange::Month => chrono::Duration::days(30),
            };
            (now - span, now)
        }
        (None, Some(since), Some(until)) => {
            if since >= until {
                return Err(RangeError::Invalid("since must precede until"));
            }
            if until - since > chrono::Duration::days(MAX_RANGE_DAYS) {
                return Err(RangeError::Invalid("range exceeds 180 days"));
            }
            (since, until)
        }
        (None, None, None) => (now - chrono::Duration::hours(24), now),
        _ => {
            return Err(RangeError::Invalid(
                "use either range or both since and until",
            ))
        }
    };
    let span = until - since;
    let bucket = query
        .bucket
        .unwrap_or(if span <= chrono::Duration::days(4) {
            AuditSummaryBucket::Hour
        } else if span <= chrono::Duration::days(90) {
            AuditSummaryBucket::Day
        } else {
            AuditSummaryBucket::Week
        });
    let unit_seconds = match bucket {
        AuditSummaryBucket::Hour => 3600,
        AuditSummaryBucket::Day => 86_400,
        AuditSummaryBucket::Week => 7 * 86_400,
    };
    // Alignment can add one bucket at each end.
    if span.num_seconds() / unit_seconds + 2 > MAX_BUCKETS {
        return Err(RangeError::Invalid("too many buckets for this range"));
    }
    let tz = query.tz.clone().unwrap_or_else(|| "UTC".to_owned());
    if !valid_zone_name(&tz) {
        return Err(RangeError::Invalid("invalid time zone"));
    }
    Ok(ResolvedRange {
        since,
        until,
        bucket,
        tz,
    })
}

/// IANA zone names: up to three `/`-separated segments of ASCII letters,
/// digits, `_`, `+`, `-`; no leading digit or sign in the first segment.
/// Existence is checked by the database before the summary transaction.
pub(crate) fn valid_zone_name(zone: &str) -> bool {
    if zone.is_empty() || zone.len() > 64 {
        return false;
    }
    let mut segments = zone.split('/');
    let Some(first) = segments.next() else {
        return false;
    };
    if first.is_empty() || !first.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_') {
        return false;
    }
    let mut rest = 0;
    for segment in segments {
        rest += 1;
        if rest > 2
            || segment.is_empty()
            || !segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-'))
        {
            return false;
        }
    }
    true
}

/// `Ok(false)` when PostgreSQL does not know the zone.
pub(crate) async fn zone_exists(pool: &PgPool, zone: &str) -> Result<bool, sqlx::Error> {
    match sqlx::query_scalar::<_, i64>(
        "SELECT 1::BIGINT WHERE (CURRENT_TIMESTAMP AT TIME ZONE $1) IS NOT NULL",
    )
    .bind(zone)
    .fetch_optional(pool)
    .await
    {
        Ok(row) => Ok(row.is_some()),
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("22023") => Ok(false),
        Err(error) => Err(error),
    }
}

fn micros(value: Option<i64>) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    value
        .map(|micros| {
            DateTime::from_timestamp_micros(micros)
                .ok_or_else(|| sqlx::Error::Decode("audit timestamp out of range".into()))
        })
        .transpose()
}

fn drillable(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= DRILLABLE_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Mapping of the actions seen in the range, decided in Rust.
struct ActionMap {
    actions: Vec<String>,
    categories: Vec<String>,
    /// Actions kept after the category filter.
    kept: Vec<String>,
    /// (action, outcome) pairs that are notable.
    notable_actions: Vec<String>,
    notable_outcomes: Vec<String>,
}

pub(crate) const SCOPE_FROM: &str = "FROM organization_audit_events e JOIN workspaces w \
    ON w.workspace_id = e.workspace_id AND w.organization_id = e.organization_id";

pub(crate) const SCOPE_WHERE: &str = "WHERE w.workspace_id = $1 AND w.deleted_at IS NULL \
    AND COALESCE(e.workspace_id, e.workspace_origin_id) = $1 \
    AND e.occurred_at_ts >= $2::TEXT::TIMESTAMPTZ AND e.occurred_at_ts < $3::TEXT::TIMESTAMPTZ \
    AND ($4::TEXT IS NULL OR e.actor_kind = $4) \
    AND ($5::TEXT IS NULL OR e.actor_id = $5) \
    AND ($6::TEXT IS NULL OR e.action = $6) \
    AND ($7::TEXT IS NULL OR e.resource_kind = $7) \
    AND ($8::TEXT IS NULL OR e.resource_id = $8) \
    AND ($9::TEXT IS NULL OR e.outcome = $9) \
    AND ($10::TEXT[] IS NULL OR e.action = ANY($10)) \
    AND (NOT $11::BOOLEAN OR (e.action, e.outcome) IN (SELECT a, o FROM unnest($12::TEXT[], $13::TEXT[]) AS n(a, o)))";

/// `FROM … [extra join] WHERE …` with the thirteen scope parameters.
pub(crate) fn scope_sql(extra_join: &str) -> String {
    format!("{SCOPE_FROM} {extra_join} {SCOPE_WHERE}")
}

struct Scope<'a> {
    workspace: &'a str,
    range: &'a ResolvedRange,
    query: &'a WorkspaceAuditSummaryQuery,
    kept: Option<&'a [String]>,
    notable: Option<(&'a [String], &'a [String])>,
}

impl Scope<'_> {
    fn bind<'q>(
        &'q self,
        statement: sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments>,
    ) -> sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments> {
        let (notable_actions, notable_outcomes) = match self.notable {
            Some((a, o)) => (Some(a), Some(o)),
            None => (None, None),
        };
        statement
            .bind(self.workspace)
            .bind(self.range.since.to_rfc3339())
            .bind(self.range.until.to_rfc3339())
            .bind(&self.query.actor_kind)
            .bind(&self.query.actor_id)
            .bind(&self.query.action)
            .bind(&self.query.resource_kind)
            .bind(&self.query.resource_id)
            .bind(&self.query.outcome)
            .bind(self.kept)
            // The discovery pass has no notable pairs yet; it must see all rows.
            .bind(self.query.notable && self.notable.is_some())
            .bind(notable_actions)
            .bind(notable_outcomes)
    }
}

/// Computes the summary in one repeatable-read, read-only transaction with a
/// per-statement timeout. The caller bounds the wall time and concurrency.
pub(crate) async fn summarize(
    pool: &PgPool,
    workspace: &str,
    query: &WorkspaceAuditSummaryQuery,
    range: &ResolvedRange,
) -> Result<WorkspaceAuditSummary, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout = '2s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL TIME ZONE 'UTC'")
        .execute(&mut *tx)
        .await?;

    // Step 1: actions and outcomes in range, before the category and notable
    // filters (which are decided here in Rust from the result).
    let discovery = Scope {
        workspace,
        range,
        query,
        kept: None,
        notable: None,
    };
    let rows = discovery
        .bind(sqlx::query(&format!(
            "SELECT e.action, e.outcome, COUNT(*) AS events, \
             (extract(epoch FROM MIN(e.occurred_at_ts))*1000000)::BIGINT AS oldest \
             {} GROUP BY e.action, e.outcome",
            scope_sql("")
        )))
        .fetch_all(&mut *tx)
        .await?;
    let mut map = ActionMap {
        actions: Vec::new(),
        categories: Vec::new(),
        kept: Vec::new(),
        notable_actions: Vec::new(),
        notable_outcomes: Vec::new(),
    };
    let mut seen = HashSet::new();
    let mut totals = AuditSummaryCount::default();
    let mut by_category: BTreeMap<AuditCategory, AuditSummaryCount> = BTreeMap::new();
    let mut oldest: Option<DateTime<Utc>> = None;
    for row in &rows {
        let action: String = row.get("action");
        let outcome: String = row.get("outcome");
        let events: i64 = row.get("events");
        let failed = outcome == "failed";
        let category = audit_category(&action);
        if query.category.is_some_and(|wanted| wanted != category) {
            continue;
        }
        let notable = is_notable(&action, failed);
        if query.notable && !notable {
            continue;
        }
        if notable {
            map.notable_actions.push(action.clone());
            map.notable_outcomes.push(outcome.clone());
        }
        if seen.insert(action.clone()) {
            map.actions.push(action.clone());
            map.categories.push(category.as_str().to_owned());
            map.kept.push(action.clone());
        }
        totals.events += events;
        let entry = by_category.entry(category).or_default();
        entry.events += events;
        if failed {
            totals.failed += events;
            entry.failed += events;
        }
        let row_oldest = micros(row.get::<Option<i64>, _>("oldest"))?;
        oldest = match (oldest, row_oldest) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    let categories = AuditCategory::ALL
        .into_iter()
        .map(|category| {
            let count = by_category.get(&category).copied().unwrap_or_default();
            AuditSummaryCategoryCount {
                category,
                events: count.events,
                failed: count.failed,
            }
        })
        .collect();
    let scope = Scope {
        workspace,
        range,
        query,
        kept: Some(&map.kept),
        notable: Some((&map.notable_actions, &map.notable_outcomes)),
    };

    // Step 2: series with empty buckets, in the requested zone.
    let bucket = range.bucket.as_str();
    let series_rows = scope
        .bind(sqlx::query(&format!(
            "WITH buckets AS (SELECT gs AS bucket_local FROM generate_series( \
                date_trunc($14::TEXT, $2::TEXT::TIMESTAMPTZ AT TIME ZONE $15::TEXT), \
                ($3::TEXT::TIMESTAMPTZ - INTERVAL '1 microsecond') AT TIME ZONE $15::TEXT, \
                ('1 ' || $14::TEXT)::INTERVAL) AS gs), \
             counts AS (SELECT date_trunc($14::TEXT, e.occurred_at_ts AT TIME ZONE $15::TEXT) AS bucket_local, \
                m.category, e.outcome, COUNT(*) AS events \
                {} GROUP BY 1, 2, 3) \
             SELECT (extract(epoch FROM (b.bucket_local AT TIME ZONE $15::TEXT))*1000000)::BIGINT AS bucket_start, \
                (extract(epoch FROM ((b.bucket_local + ('1 ' || $14::TEXT)::INTERVAL) AT TIME ZONE $15::TEXT))*1000000)::BIGINT AS bucket_end, \
                c.category, c.outcome, c.events \
             FROM buckets b LEFT JOIN counts c ON c.bucket_local = b.bucket_local \
             ORDER BY b.bucket_local LIMIT $18",
            scope_sql("JOIN unnest($16::TEXT[], $17::TEXT[]) AS m(action, category) ON m.action = e.action")
        )))
        .bind(bucket)
        .bind(&range.tz)
        .bind(&map.actions)
        .bind(&map.categories)
        .bind(MAX_BUCKETS + 1)
        .fetch_all(&mut *tx)
        .await?;
    if series_rows.len() as i64 > MAX_BUCKETS {
        return Err(sqlx::Error::Protocol(
            "audit summary bucket cap exceeded".into(),
        ));
    }
    let mut series: Vec<AuditSummaryBucketRow> = Vec::new();
    for row in &series_rows {
        let start = micros(row.get::<Option<i64>, _>("bucket_start"))?
            .ok_or_else(|| sqlx::Error::Decode("bucket start".into()))?;
        let end = micros(row.get::<Option<i64>, _>("bucket_end"))?
            .ok_or_else(|| sqlx::Error::Decode("bucket end".into()))?;
        if series.last().is_none_or(|last| last.bucket_start != start) {
            series.push(AuditSummaryBucketRow {
                bucket_start: start,
                bucket_end: end,
                from: start.max(range.since),
                to: end.min(range.until),
                events: 0,
                failed: 0,
                by_category: BTreeMap::new(),
            });
        }
        let current = series.last_mut().expect("pushed");
        if let Some(category) = row.get::<Option<String>, _>("category") {
            let events: i64 = row.get("events");
            let outcome: String = row.get("outcome");
            current.events += events;
            if outcome == "failed" {
                current.failed += events;
            }
            if let Some(category) = AuditCategory::parse(&category) {
                *current.by_category.entry(category).or_default() += events;
            }
        }
    }

    // Step 3: actors.
    let actor_rows = scope
        .bind(sqlx::query(&format!(
            "SELECT actor_kind, actor_id, events, failed, COUNT(*) OVER () AS distinct_keys, \
                SUM(events) OVER ()::BIGINT AS all_events FROM ( \
                SELECT e.actor_kind, e.actor_id, COUNT(*) AS events, \
                COUNT(*) FILTER (WHERE e.outcome = 'failed') AS failed {} \
                GROUP BY e.actor_kind, e.actor_id) g \
             ORDER BY events DESC, actor_kind, actor_id NULLS LAST LIMIT $14",
            scope_sql("")
        )))
        .bind(TOP_N)
        .fetch_all(&mut *tx)
        .await?;
    let mut actors = AuditSummaryRanking {
        top: Vec::new(),
        distinct: 0,
        omitted_events: 0,
    };
    let mut listed = 0;
    for row in &actor_rows {
        actors.distinct = row.get("distinct_keys");
        actors.omitted_events = row.get("all_events");
        let events: i64 = row.get("events");
        listed += events;
        let actor_id: Option<String> = row.get("actor_id");
        actors.top.push(AuditSummaryActor {
            actor_kind: row.get("actor_kind"),
            drillable: actor_id.as_deref().is_some_and(drillable),
            actor_id,
            events,
            failed: row.get("failed"),
        });
    }
    actors.omitted_events -= listed;

    // Step 4: resources per kind.
    let mut resources = AuditSummaryResources {
        agent: empty_ranking(),
        machine: empty_ranking(),
        app: empty_ranking(),
    };
    let mut name_lookup: Vec<(String, String)> = Vec::new();
    for kind in ["agent", "machine", "app"] {
        let rows = scope
            .bind(sqlx::query(&format!(
                "SELECT resource_id, events, failed, COUNT(*) OVER () AS distinct_keys, \
                    SUM(events) OVER ()::BIGINT AS all_events FROM ( \
                    SELECT e.resource_id, COUNT(*) AS events, \
                    COUNT(*) FILTER (WHERE e.outcome = 'failed') AS failed {} \
                    AND e.resource_kind = $14 GROUP BY e.resource_id) g \
                 ORDER BY events DESC, resource_id LIMIT $15",
                scope_sql("")
            )))
            .bind(kind)
            .bind(TOP_N)
            .fetch_all(&mut *tx)
            .await?;
        let ranking = match kind {
            "agent" => &mut resources.agent,
            "machine" => &mut resources.machine,
            _ => &mut resources.app,
        };
        let mut listed = 0;
        for row in &rows {
            ranking.distinct = row.get("distinct_keys");
            ranking.omitted_events = row.get("all_events");
            let events: i64 = row.get("events");
            listed += events;
            let resource_id: String = row.get("resource_id");
            name_lookup.push((kind.to_owned(), resource_id.clone()));
            ranking.top.push(AuditSummaryResource {
                drillable: drillable(&resource_id),
                resource_id,
                resource_name: None,
                events,
                failed: row.get("failed"),
            });
        }
        ranking.omitted_events -= listed;
    }

    // Step 5: notable events, newest first, capped per action.
    let notable_rows = scope
        .bind(sqlx::query(&format!(
            "WITH n AS (SELECT e.sequence, e.occurred_at_ts, e.action, e.actor_kind, e.actor_id, \
                e.outcome, e.resource_kind, e.resource_id, \
                row_number() OVER (PARTITION BY e.action ORDER BY e.sequence DESC) AS rn, \
                COUNT(*) OVER (PARTITION BY e.action) AS per_action, COUNT(*) OVER () AS total \
                {} AND (e.action, e.outcome) IN (SELECT a, o FROM unnest($14::TEXT[], $15::TEXT[]) AS p(a, o))) \
             SELECT sequence, (extract(epoch FROM occurred_at_ts)*1000000)::BIGINT AS occurred_at, \
                left(action, 128) AS action, left(actor_kind, 32) AS actor_kind, left(actor_id, 128) AS actor_id, \
                left(outcome, 32) AS outcome, left(resource_kind, 64) AS resource_kind, \
                left(resource_id, 256) AS resource_id, per_action, total \
             FROM n WHERE rn <= $16 ORDER BY sequence DESC LIMIT $17",
            scope_sql("")
        )))
        .bind(&map.notable_actions)
        .bind(&map.notable_outcomes)
        .bind(NOTABLE_PER_ACTION)
        .bind(NOTABLE_LIMIT)
        .fetch_all(&mut *tx)
        .await?;
    let mut notable = Vec::new();
    let mut notable_total = 0;
    let mut notable_capped = BTreeMap::new();
    for row in &notable_rows {
        notable_total = row.get("total");
        let action: String = row.get("action");
        let per_action: i64 = row.get("per_action");
        if per_action > NOTABLE_PER_ACTION {
            notable_capped.insert(action.clone(), per_action - NOTABLE_PER_ACTION);
        }
        let resource_kind: String = row.get("resource_kind");
        let resource_id: String = row.get("resource_id");
        if matches!(resource_kind.as_str(), "agent" | "machine" | "app") {
            name_lookup.push((resource_kind.clone(), resource_id.clone()));
        }
        notable.push(AuditSummaryNotable {
            sequence: row.get("sequence"),
            occurred_at: micros(row.get::<Option<i64>, _>("occurred_at"))?
                .ok_or_else(|| sqlx::Error::Decode("notable instant".into()))?,
            category: audit_category(&action),
            action,
            actor_kind: row.get("actor_kind"),
            actor_id: row.get("actor_id"),
            outcome: row.get("outcome"),
            resource_kind,
            resource_id,
            resource_name: None,
        });
    }
    if notable_rows.is_empty() && query.notable {
        notable_total = totals.events;
    }

    // Step 6: display names for the listed agents, machines and Apps.
    name_lookup.sort();
    name_lookup.dedup();
    let names = if name_lookup.is_empty() {
        HashMap::new()
    } else {
        let (kinds, ids): (Vec<String>, Vec<String>) = name_lookup.into_iter().unzip();
        sqlx::query(
            "SELECT k.kind, k.id, left(n.name, 128) AS name FROM unnest($2::TEXT[], $3::TEXT[]) AS k(kind, id) \
             JOIN LATERAL ( \
                SELECT name FROM agent_names WHERE k.kind = 'agent' AND workspace_id = $1 AND agent_id = k.id \
                UNION ALL SELECT name FROM machine_names WHERE k.kind = 'machine' AND workspace_id = $1 AND server_id = k.id \
                UNION ALL SELECT name FROM app_deployments WHERE k.kind = 'app' AND workspace_id = $1 AND app_id = k.id \
                LIMIT 1) n ON TRUE",
        )
        .bind(workspace)
        .bind(&kinds)
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|row| {
            (
                (row.get::<String, _>("kind"), row.get::<String, _>("id")),
                row.get::<String, _>("name"),
            )
        })
        .collect::<HashMap<_, _>>()
    };
    for (kind, ranking) in [
        ("agent", &mut resources.agent),
        ("machine", &mut resources.machine),
        ("app", &mut resources.app),
    ] {
        for resource in &mut ranking.top {
            resource.resource_name = names
                .get(&(kind.to_owned(), resource.resource_id.clone()))
                .cloned();
        }
    }
    for event in &mut notable {
        event.resource_name = names
            .get(&(event.resource_kind.clone(), event.resource_id.clone()))
            .cloned();
    }

    // Step 7: coverage from the backfill watermark.
    let watermark = sqlx::query(
        "SELECT backfill_done, (extract(epoch FROM backfill_complete_since)*1000000)::BIGINT AS complete_since \
         FROM audit_maintenance_settings WHERE singleton",
    )
    .fetch_one(&mut *tx)
    .await?;
    let backfill_done: bool = watermark.get("backfill_done");
    let complete_since = micros(watermark.get::<Option<i64>, _>("complete_since"))?;
    tx.commit().await?;
    let maintenance = maintenance::metadata(pool, None, Some(workspace), false).await?;

    Ok(WorkspaceAuditSummary {
        workspace_id: workspace.to_owned(),
        category_table_version: AUDIT_CATEGORY_TABLE_VERSION,
        range: AuditSummaryRangeEcho {
            since: range.since,
            until: range.until,
            bucket: range.bucket,
            tz: range.tz.clone(),
        },
        filters: AuditSummaryFilters {
            actor_kind: query.actor_kind.clone(),
            actor_id: query.actor_id.clone(),
            action: query.action.clone(),
            category: query.category,
            resource_kind: query.resource_kind.clone(),
            resource_id: query.resource_id.clone(),
            outcome: query.outcome.clone(),
            notable: query.notable,
        },
        totals,
        categories,
        anomalies: anomalies(&series),
        series,
        actors,
        resources,
        notable_total,
        notable,
        notable_capped,
        maintenance,
        coverage: AuditSummaryCoverage {
            oldest_in_range: oldest,
            backfill_complete: backfill_done
                || complete_since.is_some_and(|instant| instant <= range.since),
            complete_since: if backfill_done { None } else { complete_since },
        },
    })
}

fn empty_ranking<T>() -> AuditSummaryRanking<T> {
    AuditSummaryRanking {
        top: Vec::new(),
        distinct: 0,
        omitted_events: 0,
    }
}

fn median(values: &mut [i64]) -> f64 {
    values.sort_unstable();
    let n = values.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        values[n / 2] as f64
    } else {
        (values[n / 2 - 1] + values[n / 2]) as f64 / 2.0
    }
}

/// `volume_spike`: a bucket's events exceed max(10, 3 × median of the other
/// non-empty buckets); `failure_burst`: the same with failed events and a
/// floor of 5. A rule is evaluated only with at least three other non-empty
/// buckets, so a short or quiet range never flags.
pub(crate) fn anomalies(series: &[AuditSummaryBucketRow]) -> AuditSummaryAnomalies {
    let mut result = AuditSummaryAnomalies::default();
    type Measure = fn(&AuditSummaryBucketRow) -> i64;
    let rules: [(AuditAnomalyKind, f64, Measure); 2] = [
        (AuditAnomalyKind::VolumeSpike, 10.0, |row| row.events),
        (AuditAnomalyKind::FailureBurst, 5.0, |row| row.failed),
    ];
    for (kind, floor, value) in rules {
        let non_empty: Vec<(usize, i64)> = series
            .iter()
            .enumerate()
            .map(|(i, row)| (i, value(row)))
            .filter(|(_, v)| *v > 0)
            .collect();
        let evaluated = non_empty.len() >= 4;
        match kind {
            AuditAnomalyKind::VolumeSpike => result.volume_evaluated = evaluated,
            AuditAnomalyKind::FailureBurst => result.failure_evaluated = evaluated,
        }
        if !evaluated {
            continue;
        }
        for &(index, observed) in &non_empty {
            let mut others: Vec<i64> = non_empty
                .iter()
                .filter(|(i, _)| *i != index)
                .map(|(_, v)| *v)
                .collect();
            let baseline = median(&mut others);
            let threshold = floor.max(3.0 * baseline);
            if observed as f64 > threshold {
                result.items.push(AuditSummaryAnomaly {
                    kind,
                    bucket_start: series[index].bucket_start,
                    observed,
                    baseline,
                    threshold,
                });
            }
        }
    }
    result.items.sort_by_key(|a| a.bucket_start);
    result
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn row(start_hour: i64, events: i64, failed: i64) -> AuditSummaryBucketRow {
        let start = DateTime::from_timestamp(start_hour * 3600, 0).unwrap();
        AuditSummaryBucketRow {
            bucket_start: start,
            bucket_end: start + chrono::Duration::hours(1),
            from: start,
            to: start + chrono::Duration::hours(1),
            events,
            failed,
            by_category: BTreeMap::new(),
        }
    }

    #[test]
    fn anomaly_rules_need_three_other_buckets_and_use_floors() {
        // Three non-empty buckets: nothing is evaluated.
        let short = anomalies(&[row(0, 2, 0), row(1, 2, 0), row(2, 900, 0)]);
        assert!(!short.volume_evaluated && short.items.is_empty());
        // Four buckets of 2 plus one of 100: median of others is 2 → threshold 10.
        let series: Vec<_> = [(0, 2, 0), (1, 2, 0), (2, 2, 0), (3, 2, 0), (4, 100, 6)]
            .into_iter()
            .map(|(h, e, f)| row(h, e, f))
            .collect();
        let result = anomalies(&series);
        assert!(result.volume_evaluated);
        assert!(!result.failure_evaluated, "only one bucket has failures");
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].kind, AuditAnomalyKind::VolumeSpike);
        assert_eq!(result.items[0].observed, 100);
        assert_eq!(result.items[0].baseline, 2.0);
        assert_eq!(result.items[0].threshold, 10.0);
        // A bucket of exactly the floor does not flag.
        let series: Vec<_> = [(0, 1, 0), (1, 1, 0), (2, 1, 0), (3, 1, 0), (4, 10, 0)]
            .into_iter()
            .map(|(h, e, f)| row(h, e, f))
            .collect();
        assert!(anomalies(&series).items.is_empty());
        // Failure burst with floor 5: failed 6 over others of 1.
        let series: Vec<_> = [(0, 5, 1), (1, 5, 1), (2, 5, 1), (3, 5, 1), (4, 6, 6)]
            .into_iter()
            .map(|(h, e, f)| row(h, e, f))
            .collect();
        let result = anomalies(&series);
        assert!(result.failure_evaluated);
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].kind, AuditAnomalyKind::FailureBurst);
        assert_eq!(result.items[0].threshold, 5.0);
    }

    #[test]
    fn range_resolution_defaults_and_limits() {
        let now = DateTime::parse_from_rfc3339("2026-10-19T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let query = WorkspaceAuditSummaryQuery::default();
        let resolved = resolve_range(&query, now).unwrap();
        assert_eq!(resolved.until, now);
        assert_eq!(resolved.since, now - chrono::Duration::hours(24));
        assert_eq!(resolved.bucket, AuditSummaryBucket::Hour);
        assert_eq!(resolved.tz, "UTC");
        let month = WorkspaceAuditSummaryQuery {
            range: Some(AuditSummaryRange::Month),
            ..Default::default()
        };
        assert_eq!(
            resolve_range(&month, now).unwrap().bucket,
            AuditSummaryBucket::Day
        );
        let long = WorkspaceAuditSummaryQuery {
            since: Some(now - chrono::Duration::days(120)),
            until: Some(now),
            ..Default::default()
        };
        assert_eq!(
            resolve_range(&long, now).unwrap().bucket,
            AuditSummaryBucket::Week
        );
        let hourly_long = WorkspaceAuditSummaryQuery {
            bucket: Some(AuditSummaryBucket::Hour),
            ..long.clone()
        };
        assert!(resolve_range(&hourly_long, now).is_err(), "4320 buckets");
        let too_long = WorkspaceAuditSummaryQuery {
            since: Some(now - chrono::Duration::days(181)),
            until: Some(now),
            ..Default::default()
        };
        assert!(resolve_range(&too_long, now).is_err());
        let both = WorkspaceAuditSummaryQuery {
            range: Some(AuditSummaryRange::Day),
            since: Some(now - chrono::Duration::days(1)),
            until: Some(now),
            ..Default::default()
        };
        assert!(resolve_range(&both, now).is_err());
        let inverted = WorkspaceAuditSummaryQuery {
            since: Some(now),
            until: Some(now - chrono::Duration::days(1)),
            ..Default::default()
        };
        assert!(resolve_range(&inverted, now).is_err());
        for zone in [
            "UTC",
            "Asia/Shanghai",
            "America/Argentina/Buenos_Aires",
            "Etc/GMT+8",
        ] {
            assert!(valid_zone_name(zone), "{zone}");
        }
        for zone in [
            "",
            "1/2",
            "Asia/Shanghai/x/y",
            "a b",
            "x;drop",
            "../etc",
            "-x",
        ] {
            assert!(!valid_zone_name(zone), "{zone}");
        }
    }
}
