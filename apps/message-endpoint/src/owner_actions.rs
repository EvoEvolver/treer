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
}
impl<'a> AuditContext<'a> {
    pub(crate) fn new(actor: &'a str, now: i64) -> Result<Self, Error> {
        if !valid_id(actor) || now < 0 {
            return Err(Error::Binding);
        }
        Ok(Self { actor, now })
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
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM hub_owner_audit", [], |r| r.get(0))?;
    if count >= crate::recovery::AUDIT_LIMIT {
        return Err(StorageError::Full.into());
    }
    tx.execute("INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) VALUES(?1,?2,?3,'',?4,?5)",params![context.actor,action,endpoint,revision,context.now])?;
    Ok(())
}
