//! Inert ordering model for two independently replaced private records.
//! No disk format, filesystem operations, lock, recovery or update authority.
//! Acknowledged sync/readback below is synthetic, not a durability guarantee.
use sha2::{Digest, Sha256};

const LIMIT: usize = 64 * 1024;
type Refusal = &'static str;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Binding {
    enrollment: [u8; 16],
    job: [u8; 16],
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    binding: Binding,
    revision: u64,
    bytes: Vec<u8>,
}

impl Record {
    fn validate(&self) -> Result<(), Refusal> {
        if self.binding.enrollment == [0; 16]
            || self.binding.job == [0; 16]
            || self.revision == 0
            || self.bytes.is_empty()
            || self.bytes.len() > LIMIT
        {
            return Err("invalid_record_model");
        }
        // Bytes are opaque: a future strict schema reader must independently
        // validate their fields against this binding/revision before use.
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Marker {
    binding: Binding,
    revision: u64,
    digest: [u8; 32],
}

impl Marker {
    fn for_record(record: &Record) -> Self {
        Self {
            binding: record.binding,
            revision: record.revision,
            digest: Sha256::digest(&record.bytes).into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Boundary {
    MarkerCreate,
    MarkerWrite,
    MarkerSync,
    MarkerPublish,
    MarkerReadback,
    ReceiptCreate,
    ReceiptWrite,
    ReceiptSync,
    ReceiptPublish,
    ReceiptReadback,
}

const ORDER: [Boundary; 10] = [
    Boundary::MarkerCreate,
    Boundary::MarkerWrite,
    Boundary::MarkerSync,
    Boundary::MarkerPublish,
    Boundary::MarkerReadback,
    Boundary::ReceiptCreate,
    Boundary::ReceiptWrite,
    Boundary::ReceiptSync,
    Boundary::ReceiptPublish,
    Boundary::ReceiptReadback,
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct Store {
    marker: Option<Marker>,
    receipt: Option<Record>,
    // A name reservation survives even before a complete staged value exists.
    marker_temporary: bool,
    receipt_temporary: bool,
    marker_staged: Option<Marker>,
    receipt_staged: Option<Record>,
    uncertain: bool,
    trace: Vec<Boundary>,
}

impl Store {
    // Only consistency inspection. Matching records do not prove quiescence,
    // transaction completion, health or permission to admit another job.
    fn inspect(&self, expected: Binding) -> Result<&Record, Refusal> {
        let record = self.receipt.as_ref().ok_or("missing_receipt")?;
        record.validate()?;
        let marker = self.marker.as_ref().ok_or("missing_marker")?;
        if record.binding != expected || marker != &Marker::for_record(record) {
            return Err("record_marker_mismatch");
        }
        Ok(record)
    }

    fn current(&self, expected: Binding) -> Result<&Record, Refusal> {
        if self.uncertain
            || self.marker_temporary
            || self.receipt_temporary
            || self.marker_staged.is_some()
            || self.receipt_staged.is_some()
        {
            return Err("record_publication_uncertain");
        }
        self.inspect(expected)
    }

    // Simulates one serialized replacement of an existing bound record. No
    // provisioning, new-job admission, automatic retry or cleanup is modeled.
    fn replace(
        &mut self,
        previous: &Record,
        next: &Record,
        stop: Option<(Boundary, bool)>, // false: before boundary; true: after
    ) -> Result<(), Refusal> {
        if self.current(previous.binding)? != previous {
            return Err("unexpected_previous_record");
        }
        next.validate()?;
        if next.binding != previous.binding
            || previous.revision.checked_add(1) != Some(next.revision)
        {
            return Err("invalid_next_record");
        }
        // Sticky in this worker on any interruption, including uncertain sync
        // or readback. This is volatile state, not a persisted recovery fence.
        self.uncertain = true;
        for boundary in ORDER {
            if stop == Some((boundary, false)) {
                return Err("injected_record_boundary");
            }
            match boundary {
                Boundary::MarkerCreate => self.marker_temporary = true,
                Boundary::MarkerWrite => self.marker_staged = Some(Marker::for_record(next)),
                Boundary::MarkerPublish => {
                    self.marker = self.marker_staged.take();
                    self.marker_temporary = false;
                }
                Boundary::MarkerReadback => {
                    if self.marker.as_ref() != Some(&Marker::for_record(next)) {
                        return Err("marker_readback_mismatch");
                    }
                }
                Boundary::ReceiptCreate => self.receipt_temporary = true,
                Boundary::ReceiptWrite => self.receipt_staged = Some(next.clone()),
                Boundary::ReceiptPublish => {
                    self.receipt = self.receipt_staged.take();
                    self.receipt_temporary = false;
                }
                Boundary::ReceiptReadback => {
                    if self.inspect(next.binding)? != next {
                        return Err("receipt_readback_mismatch");
                    }
                }
                Boundary::MarkerSync | Boundary::ReceiptSync => {}
            }
            self.trace.push(boundary);
            if stop == Some((boundary, true)) {
                return Err("injected_record_boundary");
            }
        }
        self.uncertain = false;
        Ok(())
    }
}

fn fixture() -> (Store, Record, Record) {
    let previous = Record {
        binding: Binding {
            enrollment: [1; 16],
            job: [2; 16],
        },
        revision: 1,
        bytes: b"opaque previous record".to_vec(),
    };
    let next = Record {
        revision: 2,
        bytes: b"opaque next record".to_vec(),
        ..previous.clone()
    };
    let store = Store {
        marker: Some(Marker::for_record(&previous)),
        receipt: Some(previous.clone()),
        marker_temporary: false,
        receipt_temporary: false,
        marker_staged: None,
        receipt_staged: None,
        uncertain: false,
        trace: Vec::new(),
    };
    (store, previous, next)
}

#[test]
fn marker_is_verified_before_receipt_publication() {
    let (mut store, previous, next) = fixture();
    store.replace(&previous, &next, None).unwrap();
    assert_eq!(store.trace, ORDER);
    assert_eq!(store.current(previous.binding), Ok(&next));
    let snapshot = store.clone();
    assert!(store.replace(&previous, &next, None).is_err());
    assert_eq!(store, snapshot);
}

#[test]
fn every_interrupted_boundary_preserves_evidence_and_blocks_worker_retry() {
    for (index, boundary) in ORDER.into_iter().enumerate() {
        for after in [false, true] {
            let (mut store, previous, next) = fixture();
            assert!(store
                .replace(&previous, &next, Some((boundary, after)))
                .is_err());
            let performed = index + usize::from(after);
            assert_eq!(store.trace, ORDER[..performed]);
            let marker_published = performed >= 4;
            let receipt_published = performed >= 9;
            assert_eq!(
                store.marker,
                Some(Marker::for_record(if marker_published {
                    &next
                } else {
                    &previous
                }))
            );
            assert_eq!(
                store.receipt,
                Some(if receipt_published {
                    next.clone()
                } else {
                    previous.clone()
                })
            );
            assert_eq!(store.marker_temporary, (1..4).contains(&performed));
            assert_eq!(store.receipt_temporary, (6..9).contains(&performed));
            assert_eq!(
                store.marker_staged,
                (2..4)
                    .contains(&performed)
                    .then(|| Marker::for_record(&next))
            );
            assert_eq!(
                store.receipt_staged,
                (7..9).contains(&performed).then(|| next.clone())
            );
            if marker_published && !receipt_published {
                assert!(store.inspect(previous.binding).is_err());
            }
            // Even after both names match, interrupted readback is not success.
            assert!(store.current(previous.binding).is_err());
            let snapshot = store.clone();
            assert!(store.replace(&previous, &next, None).is_err());
            assert_eq!(store, snapshot);
        }
    }
}

#[test]
fn marker_detects_old_receipt_and_same_revision_changed_bytes() {
    let (mut store, previous, next) = fixture();
    store.replace(&previous, &next, None).unwrap();
    store.receipt = Some(previous.clone());
    assert!(store.inspect(previous.binding).is_err());
    store.receipt = Some(Record {
        bytes: b"changed".to_vec(),
        ..next.clone()
    });
    assert!(store.inspect(previous.binding).is_err());
    store.receipt = Some(next);
    assert!(store
        .inspect(Binding {
            job: [3; 16],
            ..previous.binding
        })
        .is_err());
    assert!(store
        .inspect(Binding {
            enrollment: [3; 16],
            ..previous.binding
        })
        .is_err());
    store.marker = None;
    assert!(store.inspect(previous.binding).is_err());
    // Restoring BOTH records consistently cannot be detected by this protocol.
    store.marker = Some(Marker::for_record(&previous));
    store.receipt = Some(previous.clone());
    assert_eq!(store.inspect(previous.binding), Ok(&previous));
}

#[test]
fn invalid_or_unexpected_records_do_not_start_publication() {
    let (store, previous, next) = fixture();
    let mut cases = vec![
        Record {
            revision: 1,
            ..next.clone()
        },
        Record {
            revision: 3,
            ..next.clone()
        },
        Record {
            bytes: vec![],
            ..next.clone()
        },
        Record {
            bytes: vec![1; LIMIT + 1],
            ..next.clone()
        },
        Record {
            binding: Binding {
                job: [3; 16],
                ..next.binding
            },
            ..next.clone()
        },
        Record {
            binding: Binding {
                enrollment: [3; 16],
                ..next.binding
            },
            ..next.clone()
        },
    ];
    cases.push(Record {
        binding: Binding {
            job: [0; 16],
            ..next.binding
        },
        ..next.clone()
    });
    for invalid in cases {
        let mut actual = store.clone();
        assert!(actual.replace(&previous, &invalid, None).is_err());
        assert_eq!(actual, store);
    }
    let mut full = store.clone();
    full.replace(
        &previous,
        &Record {
            bytes: vec![1; LIMIT],
            ..next.clone()
        },
        None,
    )
    .unwrap();
    let maximum = Record {
        revision: u64::MAX,
        ..previous
    };
    let mut overflow = Store {
        marker: Some(Marker::for_record(&maximum)),
        receipt: Some(maximum.clone()),
        ..store
    };
    let snapshot = overflow.clone();
    assert!(overflow.replace(&maximum, &next, None).is_err());
    assert_eq!(overflow, snapshot);
}
