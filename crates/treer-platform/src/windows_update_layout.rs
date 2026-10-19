//! Inert Windows publication model, compiled in tests on every platform.
//! No filesystem observer, journal, public API or mutation authority exists here.
//! A future observer must verify identity, content, ACL and directory binding
//! before producing Old/New, and distinguish absent from unreadable slots.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Object {
    Old,
    New,
    Absent,
    Unknown,
    Unreadable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Original,
    MoveOldIntent,
    OldMoved,
    PublishIntent,
    Published,
    MoveNewAsideIntent,
    NewAside,
    RestoreOldIntent,
    Restored,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Inspection {
    OriginalLayout,
    OldMoved,
    NewPublished,
    NewAside,
    OldRestored,
    Conflict,
}

// Return an observation, never a next step or executable recovery operation.
fn inspect(step: Step, slots: [Object; 3]) -> Inspection {
    use Object::{Absent, New, Old};
    use Step::*;
    match (step, slots) {
        (Original | MoveOldIntent, [Old, New, Absent]) => Inspection::OriginalLayout,
        (MoveOldIntent | OldMoved | PublishIntent, [Absent, New, Old]) => Inspection::OldMoved,
        (PublishIntent | Published | MoveNewAsideIntent, [New, Absent, Old]) => {
            Inspection::NewPublished
        }
        (MoveNewAsideIntent | NewAside | RestoreOldIntent, [Absent, New, Old]) => {
            Inspection::NewAside
        }
        (RestoreOldIntent | Restored, [Old, New, Absent]) => Inspection::OldRestored,
        _ => Inspection::Conflict,
    }
}

#[derive(Clone, Copy)]
struct Image {
    step: Step,
    slots: [Object; 3],
}

#[derive(Clone, Copy)]
enum Transaction {
    Prepared,
    Activating,
    CoreHealthy,
    Complete,
    RollbackCli,
    RollbackController,
    RolledBack,
}

#[derive(Debug, PartialEq, Eq)]
enum PairInspection {
    Consistent,
    Conflict,
}

// These health flags stand for already validated durable evidence in a future
// reader. This pure model verifies neither health nor the authenticity of input.
// Consistent means layout consistency only, never completion or permission.
fn inspect_pair(
    transaction: Transaction,
    controller: Image,
    cli: Image,
    new_health_recorded: bool,
    final_health_recorded: bool,
    old_health_recorded: bool,
) -> PairInspection {
    let evidence_belongs = match transaction {
        Transaction::Prepared | Transaction::Activating => {
            !new_health_recorded && !final_health_recorded && !old_health_recorded
        }
        Transaction::CoreHealthy | Transaction::Complete => {
            !old_health_recorded
                && (!final_health_recorded || matches!(transaction, Transaction::Complete))
        }
        Transaction::RollbackCli | Transaction::RollbackController | Transaction::RolledBack => {
            !final_health_recorded
        }
    };
    if !evidence_belongs {
        return PairInspection::Conflict;
    }
    let c = inspect(controller.step, controller.slots);
    let l = inspect(cli.step, cli.slots);
    if c == Inspection::Conflict || l == Inspection::Conflict {
        return PairInspection::Conflict;
    }
    let original = |image: Image| image.step == Step::Original;
    let restored = |image: Image| original(image) || image.step == Step::Restored;
    let published = |image: Image| image.step == Step::Published;
    let forward = |image: Image| {
        matches!(
            image.step,
            Step::Original
                | Step::MoveOldIntent
                | Step::OldMoved
                | Step::PublishIntent
                | Step::Published
        )
    };
    let consistent = match transaction {
        Transaction::Prepared => original(controller) && original(cli),
        Transaction::Activating => forward(controller) && original(cli),
        Transaction::CoreHealthy => published(controller) && new_health_recorded && forward(cli),
        Transaction::Complete => {
            published(controller) && published(cli) && new_health_recorded && final_health_recorded
        }
        Transaction::RollbackCli => {
            published(controller)
                && (restored(cli)
                    || matches!(
                        cli.step,
                        Step::Published
                            | Step::MoveNewAsideIntent
                            | Step::NewAside
                            | Step::RestoreOldIntent
                    ))
        }
        // Controller rollback may start from a failed forward rename, or from
        // a fully published image. The CLI must already be untouched/restored.
        Transaction::RollbackController => restored(cli),
        Transaction::RolledBack => restored(controller) && restored(cli) && old_health_recorded,
    };
    if consistent {
        PairInspection::Consistent
    } else {
        PairInspection::Conflict
    }
}

#[test]
fn stray_health_records_cannot_hide_a_reordered_receipt_write() {
    use Object::{Absent, New, Old};
    use Transaction::*;
    let original = Image {
        step: Step::Original,
        slots: [Old, New, Absent],
    };
    let published = Image {
        step: Step::Published,
        slots: [New, Absent, Old],
    };
    let restored = Image {
        step: Step::Restored,
        ..original
    };
    // Bit positions are new/final/old health. Enumerate all eight possibilities
    // against otherwise valid layouts, including retained new-health evidence
    // during rollback. Old-health evidence cannot coexist with Complete.
    for (tx, controller, cli, accepted) in [
        (Prepared, original, original, &[0][..]),
        (Activating, published, original, &[0][..]),
        (CoreHealthy, published, original, &[1][..]),
        (Complete, published, published, &[3][..]),
        (RollbackCli, published, restored, &[0, 1, 4, 5][..]),
        (RollbackController, restored, restored, &[0, 1, 4, 5][..]),
        (RolledBack, restored, restored, &[4, 5][..]),
    ] {
        for flags in 0..8 {
            let result = inspect_pair(
                tx,
                controller,
                cli,
                flags & 1 != 0,
                flags & 2 != 0,
                flags & 4 != 0,
            );
            assert_eq!(
                result == PairInspection::Consistent,
                accepted.contains(&flags)
            );
        }
    }
}

#[test]
fn cross_image_order_and_health_evidence_are_independent_of_matching_files() {
    use Object::{Absent, New, Old};
    use PairInspection::{Conflict, Consistent};
    use Transaction::*;
    let original = Image {
        step: Step::Original,
        slots: [Old, New, Absent],
    };
    let published = Image {
        step: Step::Published,
        slots: [New, Absent, Old],
    };
    let restored = Image {
        step: Step::Restored,
        ..original
    };
    let gap = Image {
        step: Step::PublishIntent,
        slots: [Absent, New, Old],
    };
    for (tx, c, l, new_health, final_health, old_health) in [
        (Prepared, original, original, false, false, false),
        (Activating, gap, original, false, false, false),
        (CoreHealthy, published, gap, true, false, false),
        (Complete, published, published, true, true, false),
        (RollbackCli, published, restored, false, false, false),
        (RollbackController, gap, restored, false, false, false),
        (RolledBack, restored, original, false, false, true),
    ] {
        assert_eq!(
            inspect_pair(tx, c, l, new_health, final_health, old_health),
            Consistent
        );
    }
    for tx in [Prepared, Activating] {
        assert_eq!(
            inspect_pair(tx, published, published, false, false, false),
            Conflict
        );
    }
    assert_eq!(
        inspect_pair(CoreHealthy, published, original, false, false, false),
        Conflict
    );
    assert_eq!(
        inspect_pair(Complete, published, published, true, false, false),
        Conflict
    );
    assert_eq!(
        inspect_pair(RollbackController, restored, published, true, false, false),
        Conflict
    );
    assert_eq!(
        inspect_pair(RolledBack, restored, original, false, false, false),
        Conflict
    );
    // A pre-move intent can have identical files but is not a restored record.
    let ambiguous = Image {
        step: Step::MoveOldIntent,
        ..original
    };
    assert_eq!(
        inspect_pair(RolledBack, ambiguous, original, false, false, true),
        Conflict
    );
    // The same physical gap has forward and reverse interpretations; the
    // durable rollback substep must not allow a new CLI publication intent.
    assert_eq!(
        inspect_pair(RollbackCli, published, gap, false, false, false),
        Conflict
    );
}

#[test]
fn cross_image_checks_cannot_hide_an_unreadable_or_impossible_image() {
    use Object::{Absent, New, Old, Unreadable};
    let original = Image {
        step: Step::Original,
        slots: [Old, New, Absent],
    };
    let damaged = Image {
        slots: [Old, New, Unreadable],
        ..original
    };
    let impossible = Image {
        slots: [Old, New, Old],
        ..original
    };
    for tx in [
        Transaction::Prepared,
        Transaction::Activating,
        Transaction::CoreHealthy,
        Transaction::Complete,
        Transaction::RollbackCli,
        Transaction::RollbackController,
        Transaction::RolledBack,
    ] {
        for bad in [damaged, impossible] {
            for flags in 0..8 {
                let (new, final_health, old) = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
                assert_eq!(
                    inspect_pair(tx, bad, original, new, final_health, old),
                    PairInspection::Conflict
                );
                assert_eq!(
                    inspect_pair(tx, original, bad, new, final_health, old),
                    PairInspection::Conflict
                );
            }
        }
    }
}

#[test]
fn every_rename_interruption_preserves_both_sides_of_its_intent() {
    use Inspection::{Conflict, NewPublished, OldRestored, OriginalLayout};
    use Object::{Absent, New, Old};
    use Step::*;
    // Replay physical moves separately from durable record writes. Interrupt
    // before and after each move, including failed second publication followed
    // by direct restoration of the old image.
    let original = [Old, New, Absent];
    let gap = [Absent, New, Old];
    let published = [New, Absent, Old];
    for (step, before, after, expected_before, expected_after) in [
        (
            MoveOldIntent,
            original,
            gap,
            OriginalLayout,
            Inspection::OldMoved,
        ),
        (
            PublishIntent,
            gap,
            published,
            Inspection::OldMoved,
            NewPublished,
        ),
        (
            MoveNewAsideIntent,
            published,
            gap,
            NewPublished,
            Inspection::NewAside,
        ),
        (
            RestoreOldIntent,
            gap,
            original,
            Inspection::NewAside,
            OldRestored,
        ),
    ] {
        assert_eq!(inspect(step, before), expected_before);
        assert_eq!(inspect(step, after), expected_after);
    }
    assert_eq!(inspect(Original, original), OriginalLayout);
    assert_eq!(inspect(Step::OldMoved, gap), Inspection::OldMoved);
    assert_eq!(inspect(Published, published), NewPublished);
    assert_eq!(inspect(Step::NewAside, gap), Inspection::NewAside);
    assert_eq!(inspect(Restored, original), OldRestored);
    // A durable forward intent is not a durable rollback intent.
    assert_eq!(inspect(PublishIntent, original), Conflict);
}

#[test]
fn uncertainty_and_duplicate_objects_never_become_absence_or_success() {
    use Object::*;
    use Step::*;
    let steps = [
        Original,
        MoveOldIntent,
        OldMoved,
        PublishIntent,
        Published,
        MoveNewAsideIntent,
        NewAside,
        RestoreOldIntent,
        Restored,
    ];
    let layouts = [[Old, New, Absent], [Absent, New, Old], [New, Absent, Old]];
    for step in steps {
        for layout in layouts {
            for slot in 0..3 {
                for uncertain in [Unknown, Unreadable] {
                    let mut damaged = layout;
                    damaged[slot] = uncertain;
                    assert_eq!(inspect(step, damaged), Inspection::Conflict);
                }
            }
        }
        // Occupied destination, missing candidate/backup, or an impossible
        // second reference to the very same recorded object must be refused.
        for layout in [
            [Old, New, Old],
            [New, New, Old],
            [Absent, Absent, Old],
            [Old, Absent, Absent],
            [New, Absent, Absent],
            [Old, Old, Absent],
        ] {
            assert_eq!(inspect(step, layout), Inspection::Conflict);
        }
    }
}
