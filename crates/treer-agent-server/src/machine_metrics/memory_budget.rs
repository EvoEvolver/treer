//! Inert accounting primitive, not allocator interception or runtime admission.
//! Production handles share one process-wide machine ledger. Callers must
//! charge full allocation bounds; existing allocation stages are not wired yet.
use std::ops::Deref;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, OnceLock,
};

pub const AGGREGATE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum BudgetError {
    InvalidReservation,
    Exhausted,
    AssemblyBusy,
}

struct State {
    charged: AtomicUsize,
    assembling: AtomicBool,
}

static MACHINE_BUDGET: OnceLock<MemoryBudget> = OnceLock::new();

#[derive(Clone)]
pub struct MemoryBudget(Arc<State>);

impl MemoryBudget {
    /// All machine metrics owners in this Controller process obtain clones of
    /// this ledger. No public constructor or Default creates an independent one.
    pub fn machine() -> Self {
        MACHINE_BUDGET
            .get_or_init(|| {
                Self(Arc::new(State {
                    charged: AtomicUsize::new(0),
                    assembling: AtomicBool::new(false),
                }))
            })
            .clone()
    }

    // Independent fixtures must not race with each other on the singleton.
    #[cfg(test)]
    fn isolated() -> Self {
        Self(Arc::new(State {
            charged: AtomicUsize::new(0),
            assembling: AtomicBool::new(false),
        }))
    }
}

struct Credit {
    state: Arc<State>,
    bytes: usize,
}

impl Drop for Credit {
    fn drop(&mut self) {
        let previous = self.state.charged.fetch_sub(self.bytes, Ordering::AcqRel);
        debug_assert!(previous >= self.bytes);
    }
}

/// Fields drop in declaration order: the owned value must release its memory
/// before the reservation returns. No API extracts the value without credit.
pub struct Charged<T> {
    value: T,
    _credit: Credit,
}

impl<T> Deref for Charged<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl MemoryBudget {
    pub fn charged_bytes(&self) -> usize {
        self.0.charged.load(Ordering::Acquire)
    }

    /// Reserve before invoking a fallible allocation stage. The supplied byte
    /// bound is a caller obligation; this does not measure or bound T itself.
    /// The closure must not retain a failed allocation elsewhere or spawn work
    /// that outlives this reservation. Those need separate owned reservations.
    pub fn try_build<T, E>(
        &self,
        bytes: usize,
        build: impl FnOnce() -> Result<T, E>,
    ) -> Result<Result<Charged<T>, E>, BudgetError> {
        if bytes == 0 || bytes > AGGREGATE_BYTES {
            return Err(BudgetError::InvalidReservation);
        }
        self.0
            .charged
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|next| *next <= AGGREGATE_BYTES)
            })
            .map_err(|_| BudgetError::Exhausted)?;
        let credit = Credit {
            state: self.0.clone(),
            bytes,
        };
        Ok(build().map(|value| Charged {
            value,
            _credit: credit,
        }))
    }

    pub fn try_assembly(&self) -> Result<Assembly, BudgetError> {
        self.0
            .assembling
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| BudgetError::AssemblyBusy)?;
        Ok(Assembly(Arc::new(AssemblyState {
            state: self.0.clone(),
            revoked: AtomicBool::new(false),
        })))
    }
}

struct AssemblyState {
    state: Arc<State>,
    revoked: AtomicBool,
}

impl Drop for AssemblyState {
    fn drop(&mut self) {
        self.state.assembling.store(false, Ordering::Release);
    }
}

/// Every worker/output owner must retain a clone. Revocation requests shutdown;
/// it does not reclaim a slot while ANY owner remains, even after a deadline.
#[derive(Clone)]
pub struct Assembly(Arc<AssemblyState>);

impl Assembly {
    /// Keep a stage on the same ledger as its assembly; callers cannot pair a
    /// guard with a different budget. This does not authorize work/publication.
    pub(super) fn try_build<T, E>(
        &self,
        bytes: usize,
        build: impl FnOnce() -> Result<T, E>,
    ) -> Result<Result<Charged<T>, E>, BudgetError> {
        MemoryBudget(self.0.state.clone()).try_build(bytes, build)
    }

    pub fn revoke(&self) {
        self.0.revoked.store(true, Ordering::Release);
    }
    pub fn is_revoked(&self) -> bool {
        self.0.revoked.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod response_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    fn build(budget: &MemoryBudget, bytes: usize) -> Charged<()> {
        budget
            .try_build(bytes, || Ok::<_, ()>(()))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn shared_budget_refuses_before_builder_and_returns_after_last_owner() {
        let budget = MemoryBudget::isolated();
        let full = Arc::new(build(&budget, AGGREGATE_BYTES));
        assert!(matches!(
            budget.clone().try_build(1, || -> Result<(), ()> {
                panic!("over-budget allocation entered")
            }),
            Err(BudgetError::Exhausted)
        ));
        for bytes in [0, AGGREGATE_BYTES + 1, usize::MAX] {
            assert!(matches!(
                budget.try_build(bytes, || -> Result<(), ()> {
                    panic!("invalid allocation entered")
                }),
                Err(BudgetError::InvalidReservation)
            ));
        }
        let output = full.clone();
        drop(full);
        assert_eq!(budget.charged_bytes(), AGGREGATE_BYTES);
        drop(output);
        assert_eq!(budget.charged_bytes(), 0);
    }

    #[test]
    fn failed_and_panicking_stages_restore_baseline_and_drop_value_before_credit() {
        let budget = MemoryBudget::isolated();
        let baseline = build(&budget, 128);
        let failed = budget
            .try_build(1024, || Err::<(), _>("injected stage error"))
            .unwrap();
        assert!(failed.is_err());
        assert_eq!(budget.charged_bytes(), 128);
        assert!(std::panic::catch_unwind(
            || budget.try_build::<(), ()>(1024, || panic!("injected allocation stage panic"))
        )
        .is_err());
        assert_eq!(budget.charged_bytes(), 128);
        struct Probe(MemoryBudget);
        impl Drop for Probe {
            fn drop(&mut self) {
                assert_eq!(self.0.charged_bytes(), 1152);
            }
        }
        let probe = budget
            .try_build(1024, || Ok::<_, ()>(Probe(budget.clone())))
            .unwrap()
            .unwrap();
        drop(probe);
        assert_eq!(budget.charged_bytes(), 128);
        drop(baseline);
        assert_eq!(budget.charged_bytes(), 0);
    }

    #[test]
    fn revocation_never_reclaims_live_worker_or_response_slot() {
        let budget = MemoryBudget::isolated();
        let owner = budget.try_assembly().unwrap();
        let worker = owner.clone();
        let response = owner.clone();
        let bytes = Arc::new(build(&budget, 4096));
        owner.revoke();
        assert!(worker.is_revoked() && response.is_revoked());
        drop(owner);
        assert!(matches!(
            budget.clone().try_assembly(),
            Err(BudgetError::AssemblyBusy)
        ));
        drop(worker);
        assert!(matches!(
            budget.try_assembly(),
            Err(BudgetError::AssemblyBusy)
        ));
        assert_eq!(budget.charged_bytes(), 4096);
        drop(bytes);
        assert_eq!(budget.charged_bytes(), 0);
        drop(response);
        let next = budget.try_assembly().unwrap();
        assert!(!next.is_revoked());
    }

    #[test]
    fn concurrent_handles_share_one_counter() {
        let budget = MemoryBudget::isolated();
        let ready = Arc::new(Barrier::new(5));
        let release = Arc::new(Barrier::new(5));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let budget = budget.clone();
                let ready = ready.clone();
                let release = release.clone();
                std::thread::spawn(move || {
                    let credit = build(&budget, AGGREGATE_BYTES / 4);
                    ready.wait();
                    release.wait();
                    drop(credit);
                })
            })
            .collect();
        ready.wait();
        assert_eq!(budget.charged_bytes(), AGGREGATE_BYTES);
        assert!(matches!(
            budget.try_build(1, || Ok::<_, ()>(())),
            Err(BudgetError::Exhausted)
        ));
        release.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(budget.charged_bytes(), 0);
    }

    #[test]
    fn concurrent_handles_admit_one_assembly_and_unwind_releases_it() {
        let budget = MemoryBudget::isolated();
        let start = Arc::new(Barrier::new(4));
        let finish = Arc::new(Barrier::new(4));
        let admitted = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let budget = budget.clone();
                let start = start.clone();
                let finish = finish.clone();
                let admitted = admitted.clone();
                std::thread::spawn(move || {
                    start.wait();
                    let slot = budget.try_assembly();
                    if slot.is_ok() {
                        admitted.fetch_add(1, Ordering::AcqRel);
                    }
                    finish.wait();
                    drop(slot);
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(admitted.load(Ordering::Acquire), 1);
        assert!(std::panic::catch_unwind(|| {
            let _slot = budget.try_assembly().unwrap();
            panic!("injected assembly panic");
        })
        .is_err());
        assert!(budget.try_assembly().is_ok());
    }

    #[test]
    fn independently_obtained_machine_handles_share_credit_and_revoked_slot() {
        let budget = MemoryBudget::machine();
        let slot = budget.try_assembly().unwrap();
        let full = build(&budget, AGGREGATE_BYTES);
        slot.revoke();
        let worker_slot = slot.clone();
        std::thread::spawn(move || {
            let independently_obtained = MemoryBudget::machine();
            assert!(Arc::ptr_eq(&budget.0, &independently_obtained.0));
            assert!(worker_slot.is_revoked());
            assert!(matches!(
                independently_obtained.try_assembly(),
                Err(BudgetError::AssemblyBusy)
            ));
            assert!(matches!(
                independently_obtained.try_build(1, || -> Result<(), ()> {
                    panic!("second accessor bypassed aggregate credit")
                }),
                Err(BudgetError::Exhausted)
            ));
        })
        .join()
        .unwrap();
        drop(full);
        drop(slot);
        let next = MemoryBudget::machine();
        assert_eq!(next.charged_bytes(), 0);
        assert!(!next.try_assembly().unwrap().is_revoked());
    }
}
