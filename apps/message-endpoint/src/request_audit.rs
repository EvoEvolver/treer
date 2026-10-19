//! Nonblocking bounded request metadata ring, flushed off the message hub mutex.
//! Unlike authoritative mutation audit, up to one batch can be lost on a crash.
use crate::store::Store;
use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
struct Row {
    id: String,
    actor: Option<String>,
    status: u16,
    at: i64,
}
pub(crate) struct RequestAudit {
    ring: Arc<Mutex<VecDeque<Row>>>,
    healthy: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl RequestAudit {
    pub(crate) fn start(path: &Path) -> Result<Self, ()> {
        let mut store = Store::open(path, Default::default()).map_err(|_| ())?;
        store.db.execute_batch("CREATE TABLE IF NOT EXISTS hub_management_requests(id INTEGER PRIMARY KEY AUTOINCREMENT,request_id TEXT NOT NULL UNIQUE,actor TEXT NOT NULL,status INTEGER NOT NULL,at INTEGER NOT NULL);").map_err(|_|())?;
        store
            .db
            .busy_timeout(Duration::from_millis(25))
            .map_err(|_| ())?;
        let ring = Arc::new(Mutex::new(VecDeque::<Row>::new()));
        let healthy = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let (q, h, s) = (ring.clone(), healthy.clone(), stop.clone());
        let worker=thread::Builder::new().name("hub-request-audit".into()).spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(100));
                let rows=match q.lock(){Ok(mut queue)=>queue.drain(..).collect::<Vec<_>>(),Err(_)=>{h.store(false,Ordering::Relaxed);break;}};
                let result=(||->Result<(),()>{
                    store.recheck_private_storage().map_err(|_|())?;
                    if rows.is_empty(){return Ok(());}
                    let tx=store.db.transaction().map_err(|_|())?;
                    for r in &rows {
                        tx.execute("INSERT INTO hub_management_requests(request_id,actor,status,at) VALUES(?1,?2,?3,?4) ON CONFLICT(request_id) DO UPDATE SET actor=excluded.actor,status=excluded.status,at=excluded.at",rusqlite::params![r.id,r.actor.as_deref().unwrap_or("unverified"),r.status,r.at]).map_err(|_|())?;
                    }
                    tx.execute("DELETE FROM hub_management_requests WHERE id NOT IN (SELECT id FROM hub_management_requests ORDER BY id DESC LIMIT 1000)",[]).map_err(|_|())?;
                    tx.commit().map_err(|_|())
                })();
                h.store(result.is_ok(),Ordering::Relaxed);
                if result.is_err(){
                    if let Ok(mut queue)=q.lock(){
                        // Preserve newest records under pressure; no caller waits.
                        for row in rows.into_iter().rev(){if queue.len()>=1000{break;}queue.push_front(row);}
                    }
                }
                if s.load(Ordering::Relaxed){break;}
            }
        }).map_err(|_|())?;
        Ok(Self {
            ring,
            healthy,
            stop,
            worker: Some(worker),
        })
    }
    pub(crate) fn record(&self, id: &str, actor: Option<&str>, status: u16) -> Result<(), ()> {
        if !self.healthy.load(Ordering::Relaxed) {
            return Err(());
        }
        let at = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| ())?
                .as_secs(),
        )
        .map_err(|_| ())?;
        let mut queue = self.ring.lock().map_err(|_| ())?;
        if queue.len() == 1000 {
            queue.pop_front();
        }
        queue.push_back(Row {
            id: id.into(),
            actor: actor.map(str::to_owned),
            status,
            at,
        });
        Ok(())
    }
}
impl Drop for RequestAudit {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
