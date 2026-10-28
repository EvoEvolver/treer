//! Durable device grants. Only hashes of device and user codes are retained.
use super::*;
use serde_json::{json, Value};
fn code_hash(binding: &[u8], domain: &[u8], value: &str) -> String {
    hash(&[domain, binding, value.as_bytes()])
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn user_hash(binding: &[u8], value: &str) -> Result<String, Error> {
    let text = value.trim();
    let normalized = if text.len() == 9 && text.as_bytes()[4] == b'-' {
        format!("{}{}", &text[..4], &text[5..]).to_ascii_uppercase()
    } else {
        text.to_ascii_uppercase()
    };
    if normalized.len() != 8 || !normalized.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Invalid);
    }
    Ok(code_hash(
        binding,
        b"treer.external.device.user.v1",
        &normalized,
    ))
}
impl Store {
    pub(crate) fn device_request(
        &mut self,
        key: &str,
        name: &str,
        ttl: i64,
    ) -> Result<Value, Error> {
        self.recheck()?;
        if !external_key(key) || !valid_name(name) || !(60..=1800).contains(&ttl) {
            return Err(Error::Invalid);
        }
        let secret = random_hex(32)?;
        let short = random_hex(4)?.to_ascii_uppercase();
        let device = code_hash(&self.digest, b"treer.external.device.secret.v1", &secret);
        let user = user_hash(&self.digest, &short)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample_time(&tx)?;
        tx.execute(
            "DELETE FROM device_requests WHERE expires<=?1",
            [now.saturating_sub(DAY)],
        )?;
        let (pending,total):(i64,i64)=tx.query_row("SELECT coalesce(sum(expires>?1 AND status IN ('pending','approved')),0),count(*) FROM device_requests",[now],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if pending >= 100 || total >= 1000 {
            return Err(Error::Capacity);
        }
        let generation = generation(&tx)?;
        tx.execute("INSERT INTO device_requests(device_hash,user_hash,key,name,created,expires,poll_after,generation,status) VALUES(?1,?2,?3,?4,?5,?6,?5,?7,'pending')",params![device,user,key,name,now,now+ttl,generation])?;
        checkpoint(&tx, now)?;
        tx.commit()?;
        Ok(
            json!({"device_code":secret,"user_code":format!("{}-{}",&short[..4],&short[4..]),"expires_in":ttl,"interval":5}),
        )
    }
    fn device_owner_lookup(&mut self, owner: &Owner<'_>, code: &str) -> Result<Value, Error> {
        self.owner(owner)?;
        // All attempts, including malformed/unknown codes, consume a persistent global budget.
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample_time(&tx)?;
        tx.execute("INSERT INTO device_attempt_budget(id,minute,attempts) VALUES(1,?1,1) ON CONFLICT(id) DO UPDATE SET minute=excluded.minute,attempts=CASE WHEN minute=excluded.minute THEN attempts+1 ELSE 1 END",[now/60])?;
        let attempts: i64 = tx.query_row(
            "SELECT attempts FROM device_attempt_budget WHERE id=1",
            [],
            |r| r.get(0),
        )?;
        checkpoint(&tx, now)?;
        tx.commit()?;
        if attempts > 30 {
            return Err(Error::Capacity);
        }
        let user = user_hash(&self.digest, code)?;
        let generation: String =
            self.db
                .query_row("SELECT generation FROM settings WHERE id=1", [], |r| {
                    r.get(0)
                })?;
        let value=self.db.query_row("SELECT key,name,expires,status,(SELECT name FROM external_agents WHERE key=d.key) FROM device_requests d WHERE user_hash=?1 AND expires>?2 AND generation=?3",params![user,now,generation],|r|Ok(json!({"key":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"expires_at":r.get::<_,i64>(2)?,"status":r.get::<_,String>(3)?,"existing_name":r.get::<_,Option<String>>(4)?}))).optional()?.ok_or(Error::Invalid)?;
        Ok(value)
    }
    pub(crate) fn device_lookup(&mut self, owner: &Owner<'_>, code: &str) -> Result<Value, Error> {
        let mut value = self.device_owner_lookup(owner, code)?;
        value["scope"] = json!("workspace_operations_and_own_inbox");
        value["existing_agent"] = json!(!value["existing_name"].is_null());
        Ok(json!({"request":value}))
    }
    /// Live (pending or approved, unexpired) requests. User codes are stored only as
    /// hashes, so a decision still needs the code the requesting agent shows its human.
    pub(crate) fn device_requests(&self, owner: &Owner<'_>) -> Result<Value, Error> {
        self.owner(owner)?;
        let now = super::now_floor(&self.db)?;
        let mut st = self.db.prepare("SELECT key,name,created,expires,status,(SELECT name FROM external_agents WHERE key=d.key) FROM device_requests d WHERE expires>?1 AND status IN ('pending','approved') AND generation=(SELECT generation FROM settings WHERE id=1) ORDER BY created LIMIT 100")?;
        let rows = st
            .query_map([now], |r| {
                Ok(json!({"key":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"created_at":r.get::<_,i64>(2)?,"expires_at":r.get::<_,i64>(3)?,"status":r.get::<_,String>(4)?,"existing_name":r.get::<_,Option<String>>(5)?}))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({"requests":rows}))
    }
    pub(crate) fn device_decide(
        &mut self,
        owner: &Owner<'_>,
        code: &str,
        approve: bool,
        days: u16,
        confirmed: bool,
    ) -> Result<Value, Error> {
        if approve && (!confirmed || !matches!(days, 30 | 90)) {
            return Err(Error::Invalid);
        }
        let current = self.device_owner_lookup(owner, code)?;
        if current["status"] != "pending" {
            return Err(Error::Denied);
        }
        let user = user_hash(&self.digest, code)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample_time(&tx)?;
        let status = if approve { "approved" } else { "denied" };
        let changed=tx.execute("UPDATE device_requests SET status=?1,days=?2,approver=?3,decided_at=?5,decision=?1 WHERE user_hash=?4 AND status='pending' AND expires>?5 AND generation=?6",params![status,days,owner.subject,user,now,generation(&tx)?])?;
        if changed != 1 {
            return Err(Error::Denied);
        }
        checkpoint(&tx, now)?;
        tx.commit()?;
        Ok(json!({"status":status}))
    }
    pub(crate) fn device_poll(&mut self, secret: &str) -> Result<(u16, Value), Error> {
        self.recheck()?;
        if !hex(secret, 64) {
            return Err(Error::Denied);
        }
        let device = code_hash(&self.digest, b"treer.external.device.secret.v1", secret);
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample_time(&tx)?;
        type Grant = (
            String,
            String,
            i64,
            i64,
            String,
            Option<u16>,
            Option<String>,
            String,
        );
        let row:Option<Grant>=tx.query_row("SELECT key,name,expires,poll_after,status,days,approver,generation FROM device_requests WHERE device_hash=?1",[&device],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional()?;
        let Some((key, name, expires, next, status, days, actor, generation)) = row else {
            return Err(Error::Denied);
        };
        let current = super::generation(&tx)?;
        let response = if expires <= now || generation != current {
            Some((410, "expired_token"))
        } else if status == "claimed" {
            Some((409, "already_claimed"))
        } else if status == "denied" {
            Some((403, "access_denied"))
        } else if next > now {
            Some((429, "slow_down"))
        } else {
            None
        };
        if let Some((status, code)) = response {
            checkpoint(&tx, now)?;
            tx.commit()?;
            return Ok((status, json!({"status":code,"interval":5})));
        }
        tx.execute(
            "UPDATE device_requests SET poll_after=?1 WHERE device_hash=?2",
            params![now + 5, device],
        )?;
        checkpoint(&tx, now)?;
        tx.commit()?;
        if status == "pending" {
            return Ok((200, json!({"status":"authorization_pending","interval":5})));
        }
        let binding = self.binding.clone();
        let actor = actor.ok_or(Error::Invalid)?;
        let owner = Owner {
            binding: &binding,
            subject: &actor,
        };
        let register: bool = !self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM external_agents WHERE key=?1)",
            [&key],
            |r| r.get::<_, bool>(0),
        )?;
        let days = days.ok_or(Error::Invalid)?;
        let c = self.create_scoped_claim(
            &owner,
            &name,
            i64::from(days) * DAY,
            Some((&key, &name, register)),
            Some(&device),
        )?;
        let agent = self.external_agents(&owner)?["agents"]
            .as_array()
            .and_then(|a| a.iter().find(|a| a["key"] == key))
            .cloned()
            .ok_or(Error::Invalid)?;
        Ok((
            200,
            json!({"status":"approved","access_token":c.into_plaintext(),"token_type":"Bearer","expires_in":i64::from(days)*DAY,"agent":agent}),
        ))
    }
}
