//! Decode-once staging assembler; not a public response or runtime route.
use super::*;
use std::collections::{BTreeMap, HashSet};
use treer_protocol::{
    MachineMetricsGpu, MACHINE_METRICS_MAX_GPUS, MACHINE_METRICS_MAX_SERIES,
    MACHINE_METRICS_MAX_TOTAL_BUCKETS,
};

/// Aggregate entry cap across all requested fields, independent of viewers.
/// This bounds entries, not allocator overhead or process RSS.
pub const MAX_INDEX_ACQUISITIONS: usize = 262_144;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldKey {
    pub kind: MachineMetricKind,
    pub gpu_id: Option<String>,
}

pub struct MultiFieldHistory {
    pub epoch: u64,
    /// Best-effort maintenance edge; epoch is authoritative across Busy calls.
    pub reset: bool,
    /// Preserves request order. Missing fields still have empty partial buckets.
    pub fields: Vec<(FieldKey, Buckets)>,
    /// Conservative: inventory anywhere in the retained view, not just range.
    pub has_gpu_inventory: bool,
    /// Range acquisitions only; sorted opaque ids, presence deliberately false.
    /// This is historical inventory, not the current/range response union.
    pub retained_gpus: Vec<MachineMetricsGpu>,
    /// An overlapping frame omitted inventory, even if current is unknown.
    pub historical_inventory_incomplete: bool,
    /// None means current enumeration is unknown, NOT an empty union.
    pub inventory: Option<MergedInventory>,
}

pub struct MergedInventory {
    pub gpus: Vec<MachineMetricsGpu>,
    /// Derived exclusively from a fresh complete current enumeration.
    pub gpu_status: treer_protocol::MachineMetricStatus,
}

impl DetachedFieldQuery {
    pub fn query_fields(
        &mut self,
        query: &MachineMetricsQuery,
        fields: &[FieldKey],
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<MultiFieldHistory, FieldQueryError> {
        self.query_fields_bounded(query, fields, cancelled, MAX_INDEX_ACQUISITIONS)
    }

    pub(in crate::machine_metrics) fn query_fields_bounded(
        &mut self,
        query: &MachineMetricsQuery,
        fields: &[FieldKey],
        cancelled: &mut impl FnMut() -> bool,
        limit: usize,
    ) -> Result<MultiFieldHistory, FieldQueryError> {
        let limit = limit.min(MAX_INDEX_ACQUISITIONS);
        let check = |cancelled: &mut _| {
            aggregation::check_cancelled(cancelled).map_err(FieldQueryError::Aggregation)
        };
        check(cancelled)?;
        if fields.is_empty() || fields.len() > MACHINE_METRICS_MAX_SERIES {
            return Err(FieldQueryError::InvalidFields);
        }
        let mut keys = HashSet::new();
        let mut units = Vec::with_capacity(fields.len());
        for field in fields {
            let unit =
                field_unit(field.kind, field.gpu_id.as_deref()).map_err(FieldQueryError::Cache)?;
            if !keys.insert((field.kind, field.gpu_id.as_deref())) {
                return Err(FieldQueryError::InvalidFields);
            }
            units.push(unit);
        }
        // Validate the original request BEFORE reducing its per-series budget.
        if !query.valid_at(self.clock.wall) {
            return Err(FieldQueryError::Aggregation(AggregationError::InvalidQuery));
        }
        let query = MachineMetricsQuery {
            max_points: query
                .max_points
                .min((MACHINE_METRICS_MAX_TOTAL_BUCKETS / fields.len()) as u16),
            ..query.clone()
        };
        aggregation::aggregate_cancellable(&query, self.clock.wall, units[0], [], cancelled)
            .map_err(FieldQueryError::Aggregation)?;
        let positions: HashMap<_, _> = fields
            .iter()
            .enumerate()
            .map(|(index, field)| ((field.kind, field.gpu_id.as_deref()), index))
            .collect();
        // Collector ticks are globally unique across fields in one epoch.
        // Store each original acquisition once, including cached references
        // outside the requested range so identity conflicts cannot be hidden.
        let mut index: HashMap<(u64, u64), (usize, Observation)> = HashMap::new();
        let mut counts = vec![0; fields.len()];
        let mut has_gpu_inventory = false;
        let mut retained_gpus = BTreeMap::new();
        let mut incomplete_inventory = false;
        for (clock, bytes) in self.history.frames() {
            check(cancelled)?;
            let stored =
                StoredFrame::decode(bytes, self.history.server_id(), clock, self.history.epoch())
                    .map_err(FieldQueryError::History)?;
            has_gpu_inventory |= !stored.frame.gpus.is_empty();
            // Actual acquisition time, not the cached frame's append time,
            // decides whether a device belongs to this half-open range.
            let range_ids: HashSet<_> = stored
                .fields
                .iter()
                .filter(|field| {
                    field.observation.at >= query.from && field.observation.at < query.to
                })
                .filter_map(|field| field.gpu_id.as_deref())
                .collect();
            if stored
                .fields
                .iter()
                .any(|field| field.observation.at >= query.from && field.observation.at < query.to)
            {
                incomplete_inventory |= stored.frame.inventory_truncated;
            }
            for gpu in &stored.frame.gpus {
                check(cancelled)?;
                if range_ids.contains(gpu.id.as_str()) {
                    retain_gpu(&mut retained_gpus, gpu)?;
                }
            }
            for field in stored.fields {
                check(cancelled)?;
                let Some(&position) = positions.get(&(field.kind, field.gpu_id.as_deref())) else {
                    continue;
                };
                let observation = field.observation;
                let identity = (observation.epoch, observation.tick);
                if let Some(previous) = index.get(&identity) {
                    if *previous != (position, observation) {
                        return Err(FieldQueryError::ConflictingAcquisition);
                    }
                } else {
                    if index.len() >= limit || counts[position] >= MAX_RECORDS {
                        return Err(FieldQueryError::IndexLimit);
                    }
                    index.insert(identity, (position, observation));
                    counts[position] += 1;
                }
            }
        }
        check(cancelled)?;
        let inventory = self
            .inventory
            .as_ref()
            .map(|current| merge_inventory(current, &retained_gpus, incomplete_inventory))
            .transpose()?;
        let mut observations: Vec<Vec<Observation>> =
            counts.into_iter().map(Vec::with_capacity).collect();
        for (_, (position, observation)) in index {
            check(cancelled)?;
            observations[position].push(observation);
        }
        let mut output = Vec::with_capacity(fields.len());
        for ((field, unit), observations) in fields.iter().zip(units).zip(observations) {
            check(cancelled)?;
            let buckets = aggregation::aggregate_cancellable(
                &query,
                self.clock.wall,
                unit,
                observations,
                cancelled,
            )
            .map_err(FieldQueryError::Aggregation)?;
            output.push((field.clone(), buckets));
        }
        check(cancelled)?;
        Ok(MultiFieldHistory {
            epoch: self.history.epoch(),
            reset: self.reset,
            fields: output,
            has_gpu_inventory,
            retained_gpus: retained_gpus.into_values().collect(),
            historical_inventory_incomplete: incomplete_inventory,
            inventory,
        })
    }
}

fn merge_inventory(
    current: &crate::machine_metrics::gpu_inventory::InventorySnapshot,
    retained: &BTreeMap<String, MachineMetricsGpu>,
    incomplete: bool,
) -> Result<MergedInventory, FieldQueryError> {
    if incomplete {
        return Err(FieldQueryError::IncompleteInventory);
    }
    // Inputs originate in the bounded cache and decoded historical map. Check
    // the combined cap before cloning any output; never trim current or past.
    let extra = current
        .gpus
        .iter()
        .filter(|gpu| !retained.contains_key(&gpu.id))
        .count();
    if retained.len() + extra > MACHINE_METRICS_MAX_GPUS {
        return Err(FieldQueryError::InventoryLimit);
    }
    let mut union = retained.clone();
    for gpu in &current.gpus {
        // Current capture alone supplies present=true AND the display name.
        union.insert(gpu.id.clone(), gpu.clone());
    }
    Ok(MergedInventory {
        gpus: union.into_values().collect(),
        gpu_status: if current.gpus.is_empty() {
            treer_protocol::MachineMetricStatus::NoDevice
        } else {
            treer_protocol::MachineMetricStatus::Ok
        },
    })
}

// Inputs have already passed StoredFrame's shared descriptor validation.
fn retain_gpu(
    inventory: &mut BTreeMap<String, MachineMetricsGpu>,
    gpu: &MachineMetricsGpu,
) -> Result<(), FieldQueryError> {
    if !inventory.contains_key(&gpu.id) && inventory.len() >= MACHINE_METRICS_MAX_GPUS {
        return Err(FieldQueryError::InventoryLimit);
    }
    // Last retained frame wins for display text, never for identity/presence.
    let mut historical = gpu.clone();
    historical.present = false;
    inventory.insert(gpu.id.clone(), historical);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_inventory_cap_counts_overlap_and_refuses_incomplete_history() {
        use crate::machine_metrics::gpu_inventory::InventorySnapshot;
        let mut retained = BTreeMap::new();
        for id in 0..MACHINE_METRICS_MAX_GPUS {
            retain_gpu(
                &mut retained,
                &MachineMetricsGpu {
                    id: format!("{id:064x}"),
                    name: "historical".into(),
                    present: true,
                },
            )
            .unwrap();
        }
        let mut current = InventorySnapshot {
            sampled_at: chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
            gpus: vec![MachineMetricsGpu {
                id: format!("{:064x}", 0),
                name: "current wins".into(),
                present: true,
            }],
        };
        let merged = merge_inventory(&current, &retained, false).unwrap();
        assert_eq!(merged.gpus.len(), MACHINE_METRICS_MAX_GPUS);
        assert_eq!(merged.gpus[0].name, "current wins");
        assert!(merged.gpus[0].present);
        assert!(merged.gpus[1..].iter().all(|gpu| !gpu.present));
        current.gpus[0].id = format!("{:064x}", MACHINE_METRICS_MAX_GPUS);
        assert!(matches!(
            merge_inventory(&current, &retained, false),
            Err(FieldQueryError::InventoryLimit)
        ));
        assert!(matches!(
            merge_inventory(&current, &retained, true),
            Err(FieldQueryError::IncompleteInventory)
        ));
        assert_eq!(retained.len(), MACHINE_METRICS_MAX_GPUS);
        assert!(retained.values().all(|gpu| !gpu.present));
    }

    #[test]
    fn retained_inventory_is_bounded_sorted_and_never_claims_presence() {
        let mut inventory = BTreeMap::new();
        for index in (0..MACHINE_METRICS_MAX_GPUS).rev() {
            retain_gpu(
                &mut inventory,
                &MachineMetricsGpu {
                    id: format!("{index:064x}"),
                    name: "old name".into(),
                    present: true,
                },
            )
            .unwrap();
        }
        let id = format!("{:064x}", 0);
        retain_gpu(
            &mut inventory,
            &MachineMetricsGpu {
                id: id.clone(),
                name: "new name".into(),
                present: true,
            },
        )
        .unwrap();
        assert_eq!(inventory[&id].name, "new name");
        assert!(inventory.values().all(|gpu| !gpu.present));
        assert_eq!(inventory.keys().next(), Some(&id));
        let before = inventory.clone();
        assert_eq!(
            retain_gpu(
                &mut inventory,
                &MachineMetricsGpu {
                    id: format!("{:064x}", MACHINE_METRICS_MAX_GPUS),
                    name: "overflow".into(),
                    present: true,
                }
            ),
            Err(FieldQueryError::InventoryLimit)
        );
        assert_eq!(inventory, before);
    }
}
