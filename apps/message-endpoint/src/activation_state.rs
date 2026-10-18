//! Durable activation fences and body-free counters, not delivery attestations.
use crate::{
    hub::{Directory, Error, Hub},
    relay::Mailbox,
};
use rusqlite::OptionalExtension;
use serde::Serialize;
#[cfg(feature = "hub")]
use {
    crate::owner_actions::{record, AuditContext},
    rusqlite::params,
};
#[derive(Default, Serialize)]
pub struct Activity {
    pub activated_at: Option<i64>,
    pub inbound_updates: i64,
    pub outbound_chunks: i64,
    pub stale_updates: i64,
}
impl<M: Mailbox + Directory> Hub<M> {
    pub fn activation_activity(&self, id: &str) -> Result<Activity, Error> {
        Ok(self.store.db.query_row("SELECT a.activated_at,a.inbound_updates,a.outbound_chunks,a.stale_updates FROM hub_activity a JOIN endpoints e ON e.id=a.endpoint AND e.generation=a.generation WHERE a.endpoint=?1",[id],|r|Ok(Activity {activated_at:{let at=r.get::<_,i64>(0)?;if at>0 {Some(at)} else {None}},inbound_updates:r.get(1)?,outbound_chunks:r.get(2)?,stale_updates:r.get(3)?})).optional()?.unwrap_or_default())
    }
    /// Called before starting any transport/worker, never from browser JSON.
    #[cfg(feature = "hub")]
    pub(crate) fn enforce_startup_activation_gate(
        &mut self,
        enabled: bool,
        now: i64,
    ) -> Result<(), Error> {
        self.store.recheck_private_storage()?;
        self.recovery_schema()?;
        let mut q=self.store.db.prepare("SELECT b.endpoint,b.revision FROM hub_bindings b JOIN endpoints e ON e.id=b.endpoint LEFT JOIN hub_activity a ON a.endpoint=e.id AND a.generation=e.generation WHERE e.paused=0 AND (?1=0 OR COALESCE(a.activated_at,0)<=0) ORDER BY b.endpoint LIMIT 21")?;
        let rows = q
            .query_map([enabled], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(q);
        if rows.len() > 20 {
            return Err(Error::Binding);
        }
        let context = AuditContext::new("system_activation_gate", now)?;
        let tx = self.store.db.transaction()?;
        for (id, revision) in rows {
            record(&tx, Some(&context), "pause_binding", &id, revision)?;
            tx.execute("UPDATE endpoints SET paused=1 WHERE id=?1", [&id])?;
            tx.execute(
                "UPDATE hub_bindings SET revision=revision+1,reason=?2 WHERE endpoint=?1",
                params![
                    id,
                    if enabled {
                        "activation_required"
                    } else {
                        "operator_activation_disabled"
                    }
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
#[cfg(all(test, feature = "hub"))]
mod tests {
    use crate::{
        recovery::tests::fixture,
        store::{Limits, Store},
    };
    #[test]
    fn startup_gate_off_pauses_existing_active_with_system_audit_and_no_repeat_rows() {
        let (mut hub, _) = fixture(Store::memory(Limits::default()).unwrap());
        hub.store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        hub.enforce_startup_activation_gate(false, 1_800_000_001)
            .unwrap();
        let v = hub.view("ag_endpoint").unwrap();
        assert!(v.paused);
        assert_eq!(v.reason, "operator_activation_disabled");
        assert_eq!(v.revision, 5);
        let events = hub.owner_audit(None, 25).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].actor_id, "system_activation_gate");
        hub.enforce_startup_activation_gate(false, 1_800_000_002)
            .unwrap();
        assert_eq!(hub.owner_audit(None, 25).unwrap().len(), 1);
    }
    #[test]
    fn legacy_active_without_activation_fence_is_paused_even_when_operator_enables_gate() {
        let (mut hub, _) = fixture(Store::memory(Limits::default()).unwrap());
        hub.store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        hub.enforce_startup_activation_gate(true, 1_800_000_001)
            .unwrap();
        assert!(!hub.view("ag_endpoint").unwrap().paused);
        hub.store
            .db
            .execute("DELETE FROM hub_activity", [])
            .unwrap();
        hub.enforce_startup_activation_gate(true, 1_800_000_002)
            .unwrap();
        assert!(hub.view("ag_endpoint").unwrap().paused);
        assert_eq!(
            hub.view("ag_endpoint").unwrap().reason,
            "activation_required"
        );
    }
    #[test]
    fn full_audit_still_allows_startup_kill_switch_via_bounded_emergency_evidence() {
        let (mut hub, _) = fixture(Store::memory(Limits::default()).unwrap());
        hub.recovery_schema().unwrap();
        hub.store.db.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) SELECT 'human_owner','fixture','ag_endpoint','',1,1 FROM n; UPDATE endpoints SET paused=0;").unwrap();
        hub.enforce_startup_activation_gate(false, 1_800_000_001)
            .unwrap();
        assert!(hub.view("ag_endpoint").unwrap().paused);
        assert_eq!(hub.owner_audit_capacity().unwrap(), (true, 1));
        assert_eq!(
            hub.store
                .db
                .query_row("SELECT actor FROM hub_emergency_pauses", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "system_activation_gate"
        );
    }
}
