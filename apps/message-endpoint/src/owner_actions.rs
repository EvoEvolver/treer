//! Internal state mutation audit boundary. HTTP must derive actor from a fresh
//! verified owner and check Origin/confirmation/revision before calling.
use crate::{
    hub::Error,
    store::{valid_id, Error as StorageError},
};
use rusqlite::{params, Transaction};
pub(crate) struct AuditContext<'a> {
    actor: &'a str,
    now: i64,
    detail: Option<&'a str>,
}
impl<'a> AuditContext<'a> {
    #[cfg(feature = "hub")]
    pub(crate) fn with_detail(actor: &'a str, now: i64, detail: &'a str) -> Result<Self, Error> {
        if detail.len() > 4096 {
            return Err(Error::Binding);
        }
        let mut context = Self::new(actor, now)?;
        context.detail = Some(detail);
        Ok(context)
    }
    pub(crate) fn new(actor: &'a str, now: i64) -> Result<Self, Error> {
        if !valid_id(actor) || now < 0 {
            return Err(Error::Binding);
        }
        Ok(Self {
            actor,
            now,
            detail: None,
        })
    }
}
pub(crate) fn record(
    tx: &Transaction<'_>,
    context: Option<&AuditContext<'_>>,
    action: &str,
    endpoint: &str,
    revision: i64,
) -> Result<(), Error> {
    let Some(context) = context else {
        return Ok(());
    };
    // HTTP also rate-limits admission, but a slow Core readback must not bypass
    // the commit-time budget. Audit rows cannot be cleared by an owner request.
    if action == "resume_binding" {
        let recent: i64 = tx.query_row("SELECT COUNT(*) FROM hub_owner_audit WHERE actor=?1 AND action='resume_binding' AND at>?2",params![context.actor,context.now.saturating_sub(60)],|r|r.get(0))?;
        if recent >= 10 {
            return Err(StorageError::Full.into());
        }
    }
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM hub_owner_audit", [], |r| r.get(0))?;
    if count >= crate::recovery::AUDIT_LIMIT {
        if action == "pause_binding" {
            // One fixed row per existing binding (at most 20), no append-only
            // emergency quota to exhaust. Keep the latest actor/revision/time
            // and a saturating count in the same pause transaction.
            tx.execute("INSERT INTO hub_emergency_pauses(endpoint,actor,revision,at,count) VALUES(?1,?2,?3,?4,1) ON CONFLICT(endpoint) DO UPDATE SET actor=excluded.actor,revision=excluded.revision,at=excluded.at,count=CASE WHEN count<9223372036854775807 THEN count+1 ELSE count END",params![endpoint,context.actor,revision,context.now])?;
            #[cfg(feature = "hub")]
            {
                let notice_id: i64 = tx.query_row(
                    "SELECT -rowid FROM endpoints WHERE id=?1",
                    [endpoint],
                    |r| r.get(0),
                )?;
                crate::management::changed(
                    tx,
                    context.actor,
                    action,
                    endpoint,
                    revision,
                    notice_id,
                )?;
            }
            return Ok(());
        }
        return Err(StorageError::Full.into());
    }
    #[cfg(feature = "hub")]
    let masked = if crate::management::agent(tx, context.actor)? {
        context
            .detail
            .map(|d| crate::management::masked_detail(tx, d))
            .transpose()?
    } else {
        None
    };
    #[cfg(feature = "hub")]
    let detail = masked.as_deref().or(context.detail).unwrap_or("");
    #[cfg(not(feature = "hub"))]
    let detail = context.detail.unwrap_or("");
    tx.execute("INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) VALUES(?1,?2,?3,?4,?5,?6)",params![context.actor,action,endpoint,detail,revision,context.now])?;
    #[cfg(feature = "hub")]
    crate::management::changed(
        tx,
        context.actor,
        action,
        endpoint,
        revision,
        tx.last_insert_rowid(),
    )?;
    Ok(())
}
