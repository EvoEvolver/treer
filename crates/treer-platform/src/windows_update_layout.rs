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

#[derive(Clone, Copy, Debug)]
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
