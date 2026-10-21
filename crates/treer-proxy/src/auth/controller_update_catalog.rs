//! Operator-provisioned, immutable release ordering. This is not a browser API
//! or an artifact downloader; no URL, command, path or trust override is stored.
use std::collections::HashSet;

use serde::Deserialize;
use sqlx::Row;
use treer_protocol::controller_update::{Platform, Target, MAX_TARGETS};

use super::{AuthFailure, AuthStore};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provisioning {
    schema_version: u8,
    catalogs: Vec<Catalog>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    platform: Platform,
    revision: u64,
    entries: Vec<Entry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    release_sequence: u64,
    target: Target,
}
fn invalid() -> AuthFailure {
    AuthFailure::conflict(
        "controller_update_catalog_invalid",
        "Invalid provisioned update catalog",
    )
}
impl AuthStore {
    /// Only the local Proxy startup configuration calls this writer. Import is
    /// atomic across platforms and cannot reinterpret or decrease a revision.
    pub(crate) async fn provision_controller_update_catalog(
        &self,
        bytes: &[u8],
    ) -> Result<(), AuthFailure> {
        if bytes.len() > 64 * 1024 {
            return Err(invalid());
        }
        let mut file: Provisioning = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if file.schema_version != 1 || file.catalogs.is_empty() || file.catalogs.len() > 5 {
            return Err(invalid());
        }
        let mut platforms = HashSet::new();
        for catalog in &mut file.catalogs {
            let key = serde_json::to_value(catalog.platform).map_err(|_| invalid())?;
            if !platforms.insert(key.to_string())
                || catalog.revision == 0
                || catalog.revision >= 1 << 53
                || catalog.entries.is_empty()
                || catalog.entries.len() > MAX_TARGETS + 1
            {
                return Err(invalid());
            }
            let mut sequences = HashSet::new();
            let mut ids = HashSet::new();
            let mut identities = HashSet::new();
            for entry in &catalog.entries {
                entry.target.validate().map_err(|_| invalid())?;
                if entry.target.platform != catalog.platform
                    || entry.release_sequence == 0
                    || entry.release_sequence >= 1 << 53
                    || !sequences.insert(entry.release_sequence)
                    || !ids.insert(&entry.target.target_id)
                    || !identities.insert(
                        serde_json::to_string(&entry.target.identity).map_err(|_| invalid())?,
                    )
                {
                    return Err(invalid());
                }
            }
            catalog.entries.sort_by_key(|e| e.release_sequence);
        }
        // Deterministic lock order across concurrent startup replicas.
        file.catalogs.sort_by_key(|c| {
            serde_json::to_value(c.platform)
                .expect("enum serialization")
                .to_string()
        });
        let mut tx = self.pool.begin().await.map_err(AuthFailure::database)?;
        for catalog in file.catalogs {
            let key = serde_json::to_value(catalog.platform)
                .map_err(|_| invalid())?
                .as_str()
                .ok_or_else(invalid)?
                .to_owned();
            sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
                .bind(format!("controller-update-catalog:{key}"))
                .execute(&mut *tx)
                .await
                .map_err(AuthFailure::database)?;
            let floor: i64 = sqlx::query_scalar("SELECT GREATEST(COALESCE((SELECT revision FROM controller_update_catalog_floor WHERE platform=$1),0),COALESCE((SELECT MAX(revision) FROM controller_update_catalog_entries WHERE platform=$1),0))")
                .bind(&key).fetch_one(&mut *tx).await.map_err(AuthFailure::database)?;
            if catalog.revision < floor as u64 {
                return Err(invalid());
            }
            let old = sqlx::query("SELECT release_sequence,target FROM controller_update_catalog_entries WHERE platform=$1 AND revision=$2 ORDER BY release_sequence")
                .bind(&key).bind(catalog.revision as i64).fetch_all(&mut *tx).await.map_err(AuthFailure::database)?;
            if !old.is_empty() {
                if old.len() != catalog.entries.len() {
                    return Err(invalid());
                }
                for (row, entry) in old.iter().zip(&catalog.entries) {
                    let sequence: i64 = row
                        .try_get("release_sequence")
                        .map_err(AuthFailure::database)?;
                    let target: serde_json::Value =
                        row.try_get("target").map_err(AuthFailure::database)?;
                    if sequence != entry.release_sequence as i64
                        || target != serde_json::to_value(&entry.target).map_err(|_| invalid())?
                    {
                        return Err(invalid());
                    }
                }
            } else {
                // A missing revision at the persisted floor is not permission to
                // repopulate different bytes after an incomplete/manual restore.
                if catalog.revision == floor as u64 {
                    return Err(invalid());
                }
                for entry in catalog.entries {
                    sqlx::query("INSERT INTO controller_update_catalog_entries(platform,revision,release_sequence,target) VALUES($1,$2,$3,$4)")
                        .bind(&key).bind(catalog.revision as i64).bind(entry.release_sequence as i64)
                        .bind(serde_json::to_value(entry.target).map_err(|_| invalid())?).execute(&mut *tx).await.map_err(AuthFailure::database)?;
                }
            }
            sqlx::query("INSERT INTO controller_update_catalog_floor(platform,revision) VALUES($1,$2) ON CONFLICT(platform) DO UPDATE SET revision=GREATEST(controller_update_catalog_floor.revision,EXCLUDED.revision)")
                .bind(&key).bind(catalog.revision as i64).execute(&mut *tx).await.map_err(AuthFailure::database)?;
            // Old operations retain their own full target/binding. Only the
            // current catalog is needed; its floor survives this bounded prune.
            sqlx::query(
                "DELETE FROM controller_update_catalog_entries WHERE platform=$1 AND revision<$2",
            )
            .bind(key)
            .bind(catalog.revision as i64)
            .execute(&mut *tx)
            .await
            .map_err(AuthFailure::database)?;
        }
        tx.commit().await.map_err(AuthFailure::database)
    }
}
