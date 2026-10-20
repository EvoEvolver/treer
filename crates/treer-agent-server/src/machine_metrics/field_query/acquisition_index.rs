//! Exact identity index with one owned key per field, never per acquisition.
use super::*;

/// Candidate device churn is an availability bound, independent of the
/// eventual response's 100-series bound. No eviction on exhaustion.
pub(super) const MAX_CANDIDATE_FIELDS: usize = 4096;

pub(super) struct AcquisitionIndex {
    keys: HashMap<FieldKey, usize>,
    counts: Vec<usize>,
    entries: HashMap<(u64, u64), (usize, Observation)>,
    limit: usize,
}

impl AcquisitionIndex {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            keys: HashMap::new(),
            counts: Vec::new(),
            entries: HashMap::new(),
            limit: limit.min(MAX_INDEX_ACQUISITIONS),
        }
    }

    pub(super) fn insert(
        &mut self,
        key: &FieldKey,
        observation: Observation,
    ) -> Result<(), FieldQueryError> {
        let identity = (observation.epoch, observation.tick);
        // Compare before any allocation or budget check: duplicate references
        // do not spend budget, and conflicts remain conflicts at capacity.
        if let Some(&(position, previous)) = self.entries.get(&identity) {
            return if self.keys.get(key) == Some(&position) && previous == observation {
                Ok(())
            } else {
                Err(FieldQueryError::ConflictingAcquisition)
            };
        }
        if self.entries.len() >= self.limit {
            return Err(FieldQueryError::IndexLimit);
        }
        let position = if let Some(&position) = self.keys.get(key) {
            position
        } else {
            if self.keys.len() >= MAX_CANDIDATE_FIELDS {
                return Err(FieldQueryError::IndexLimit);
            }
            // Validate before cloning a potentially caller-owned string.
            field_unit(key.kind, key.gpu_id.as_deref()).map_err(FieldQueryError::Cache)?;
            let position = self.counts.len();
            self.keys.insert(key.clone(), position);
            self.counts.push(0);
            position
        };
        if self.counts[position] >= MAX_RECORDS {
            return Err(FieldQueryError::IndexLimit);
        }
        self.entries.insert(identity, (position, observation));
        self.counts[position] += 1;
        Ok(())
    }

    /// Consumes the index; no second identity map is built. The map allocation
    /// and output vectors can overlap while draining, accounted for in docs.
    pub(super) fn into_selected(
        self,
        fields: &[FieldKey],
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Vec<Vec<Observation>>, FieldQueryError> {
        let mut selected = vec![None; self.counts.len()];
        let mut output = Vec::with_capacity(fields.len());
        for (target, field) in fields.iter().enumerate() {
            aggregation::check_cancelled(cancelled).map_err(FieldQueryError::Aggregation)?;
            let count = if let Some(&source) = self.keys.get(field) {
                selected[source] = Some(target);
                self.counts[source]
            } else {
                0
            };
            output.push(Vec::with_capacity(count));
        }
        for (_, (source, observation)) in self.entries {
            aggregation::check_cancelled(cancelled).map_err(FieldQueryError::Aggregation)?;
            if let Some(target) = selected[source] {
                output[target].push(observation);
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use treer_protocol::MachineMetricStatus;

    fn key(id: usize) -> FieldKey {
        FieldKey {
            kind: MachineMetricKind::GpuUtilization,
            gpu_id: Some(format!("{id:064x}")),
        }
    }
    fn observation(tick: u64) -> Observation {
        Observation {
            epoch: 1,
            tick,
            at: Utc.timestamp_opt(1_800_000_000, 0).unwrap(),
            status: MachineMetricStatus::Ok,
            value: Some(25.0),
        }
    }

    #[test]
    fn exact_index_preserves_conflicts_and_deduplicates_at_capacity() {
        let mut index = AcquisitionIndex::new(1);
        index.insert(&key(0), observation(1)).unwrap();
        index.insert(&key(0), observation(1)).unwrap();
        assert_eq!(
            index.insert(&key(1), observation(1)),
            Err(FieldQueryError::ConflictingAcquisition)
        );
        let mut changed = observation(1);
        changed.value = Some(26.0);
        assert_eq!(
            index.insert(&key(0), changed),
            Err(FieldQueryError::ConflictingAcquisition)
        );
        assert_eq!(
            index.insert(&key(0), observation(2)),
            Err(FieldQueryError::IndexLimit)
        );
        let grouped = index
            .into_selected(&[key(1), key(0)], &mut || false)
            .unwrap();
        assert!(grouped[0].is_empty());
        assert_eq!(grouped[1], vec![observation(1)]);
    }

    #[test]
    fn exact_index_caps_field_churn_and_per_field_acquisitions() {
        let mut index = AcquisitionIndex::new(MAX_INDEX_ACQUISITIONS);
        for id in 0..MAX_CANDIDATE_FIELDS {
            index.insert(&key(id), observation(id as u64)).unwrap();
        }
        assert_eq!(
            index.insert(
                &key(MAX_CANDIDATE_FIELDS),
                observation(MAX_CANDIDATE_FIELDS as u64)
            ),
            Err(FieldQueryError::IndexLimit)
        );
        let mut index = AcquisitionIndex::new(MAX_INDEX_ACQUISITIONS);
        for tick in 0..MAX_RECORDS {
            index.insert(&key(0), observation(tick as u64)).unwrap();
        }
        assert_eq!(index.keys.len(), 1);
        assert_eq!(
            index.insert(&key(0), observation(MAX_RECORDS as u64)),
            Err(FieldQueryError::IndexLimit)
        );
        assert!(matches!(
            index.into_selected(&[key(0)], &mut || true),
            Err(FieldQueryError::Aggregation(AggregationError::Cancelled))
        ));
    }

    #[test]
    fn exact_index_max_shape_capacity_payload_measurement() {
        for field_count in [32, MAX_CANDIDATE_FIELDS] {
            let mut index = AcquisitionIndex::new(MAX_INDEX_ACQUISITIONS);
            for tick in 0..MAX_INDEX_ACQUISITIONS {
                index
                    .insert(&key(tick % field_count), observation(tick as u64))
                    .unwrap();
            }
            let entry_bytes = index.entries.capacity()
                * std::mem::size_of::<((u64, u64), (usize, Observation))>();
            let key_bytes = index.keys.capacity() * std::mem::size_of::<(FieldKey, usize)>()
                + index
                    .keys
                    .keys()
                    .filter_map(|k| k.gpu_id.as_ref())
                    .map(String::capacity)
                    .sum::<usize>();
            let regroup_bytes = MAX_INDEX_ACQUISITIONS * std::mem::size_of::<Observation>();
            println!("candidate_index_capacity fields={field_count} entries={} entry_bytes={entry_bytes} key_bytes={key_bytes} regroup_bytes={regroup_bytes}", index.entries.capacity());
            assert_eq!(index.keys.len(), field_count);
            assert_eq!(index.entries.len(), MAX_INDEX_ACQUISITIONS);
            assert_eq!(
                index.insert(&key(0), observation(MAX_INDEX_ACQUISITIONS as u64)),
                Err(FieldQueryError::IndexLimit)
            );
        }
    }

    #[cfg(feature = "metrics-allocation-measurement")]
    #[test]
    fn exact_index_measured_live_heap_peak() {
        // The measurement crate counts the current thread only. Keep this
        // workload synchronous; run this exact test in its own test process.
        assert_eq!(
            (MAX_INDEX_ACQUISITIONS, MAX_RECORDS, MAX_CANDIDATE_FIELDS),
            (262_144, 17_280, 4096)
        );
        for field_count in [32, MAX_CANDIDATE_FIELDS] {
            let measured = allocation_counter::measure(|| {
                let mut index = AcquisitionIndex::new(MAX_INDEX_ACQUISITIONS);
                for tick in 0..MAX_INDEX_ACQUISITIONS {
                    index
                        .insert(&key(tick % field_count), observation(tick as u64))
                        .unwrap();
                }
                let fields: Vec<_> = (0..field_count.min(100)).map(key).collect();
                let selected = index.into_selected(&fields, &mut || false).unwrap();
                assert_eq!(selected.len(), fields.len());
                std::hint::black_box(&selected);
                drop(selected);
            });
            assert_eq!(measured.bytes_current, 0);
            assert_eq!(measured.count_current, 0);
            // Regression ceiling for this prerequisite, not a runtime promise.
            assert!(measured.bytes_max < 64 * 1024 * 1024);
            println!("candidate_index_measured fields={field_count} peak_live_bytes={} total_allocated_bytes={} final_live_bytes={}", measured.bytes_max, measured.bytes_total, measured.bytes_current);
        }
    }
}
