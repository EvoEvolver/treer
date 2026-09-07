use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use treer_protocol::NetworkUsageReport;

#[derive(Serialize, Deserialize)]
struct Entry {
    workspace: String,
    server: String,
    report: NetworkUsageReport,
}

#[derive(Default)]
struct State {
    pending: HashMap<String, NetworkUsageReport>,
    known: HashMap<String, NetworkUsageReport>,
}

/// Blocking filesystem operations run on Tokio's blocking pool. A persisted
/// cumulative report is removed only after the Proxy commits and acknowledges
/// that exact version. Duplicate/reordered acknowledgements are harmless.
pub(crate) struct UsageOutbox {
    directory: PathBuf,
    workspace: String,
    server: String,
    state: Mutex<State>,
}

impl UsageOutbox {
    pub fn open(directory: PathBuf, workspace: String, server: String) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&directory)?;
            let meta = fs::symlink_metadata(&directory)?;
            anyhow::ensure!(
                meta.is_dir() && meta.permissions().mode() & 0o777 == 0o700,
                "usage outbox directory must be private (0700)"
            );
        }
        #[cfg(not(unix))]
        fs::create_dir_all(&directory)?;
        let outbox = Self {
            directory,
            workspace,
            server,
            state: Mutex::new(State::default()),
        };
        let mut state = outbox.state.lock().unwrap();
        for file in fs::read_dir(&outbox.directory)? {
            let file = file?;
            let path = file.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            anyhow::ensure!(
                file.file_type()?.is_file(),
                "outbox entry is not a regular file"
            );
            let entry: Entry = serde_json::from_slice(&fs::read(&path)?)
                .with_context(|| format!("invalid usage outbox entry {}", path.display()))?;
            if entry.workspace != outbox.workspace || entry.server != outbox.server {
                continue;
            }
            anyhow::ensure!(
                path == outbox.path(&entry.report.ticket)?,
                "usage ticket filename mismatch"
            );
            state
                .known
                .insert(entry.report.ticket.clone(), entry.report.clone());
            state
                .pending
                .insert(entry.report.ticket.clone(), entry.report);
        }
        drop(state);
        Ok(outbox)
    }

    fn path(&self, ticket: &str) -> Result<PathBuf> {
        let parsed = uuid::Uuid::parse_str(ticket)?;
        anyhow::ensure!(parsed.to_string() == ticket, "noncanonical usage ticket");
        Ok(self.directory.join(format!("{ticket}.json")))
    }

    pub fn record(&self, report: NetworkUsageReport) -> Result<Option<NetworkUsageReport>> {
        let path = self.path(&report.ticket)?;
        let mut state = self.state.lock().unwrap();
        if state.known.get(&report.ticket) == Some(&report) {
            return Ok(state.pending.get(&report.ticket).cloned());
        }
        let entry = Entry {
            workspace: self.workspace.clone(),
            server: self.server.clone(),
            report: report.clone(),
        };
        atomic_write(&path, &serde_json::to_vec(&entry)?)?;
        if state.known.len() >= 16384 {
            state.known = state.pending.clone();
        }
        state.known.insert(report.ticket.clone(), report.clone());
        state.pending.insert(report.ticket.clone(), report.clone());
        Ok(Some(report))
    }

    pub fn acknowledge(&self, report: &NetworkUsageReport) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.pending.get(&report.ticket) == Some(report) {
            fs::remove_file(self.path(&report.ticket)?)?;
            state.pending.remove(&report.ticket);
        }
        Ok(())
    }

    pub fn pending(&self) -> Vec<NetworkUsageReport> {
        self.state
            .lock()
            .unwrap()
            .pending
            .values()
            .cloned()
            .collect()
    }
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        fs::File::open(path.parent().context("outbox has no parent")?)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use treer_protocol::NetworkUsageTotals;

    #[test]
    fn network_usage_survives_restart_and_old_ack_cannot_erase_new_counts() {
        let directory =
            std::env::temp_dir().join(format!("treer-usage-test-{}", uuid::Uuid::new_v4()));
        let open =
            || UsageOutbox::open(directory.clone(), "workspace".into(), "server".into()).unwrap();
        let report = NetworkUsageReport {
            finished: false,
            ticket: uuid::Uuid::new_v4().to_string(),
            totals: NetworkUsageTotals {
                sent_bytes: 7,
                ..Default::default()
            },
        };
        let box1 = open();
        box1.record(report.clone()).unwrap();
        drop(box1);
        let box2 = open();
        assert_eq!(box2.pending(), vec![report.clone()]);
        let mut newer = report.clone();
        newer.totals.sent_bytes = 10;
        box2.record(newer.clone()).unwrap();
        box2.acknowledge(&report).unwrap();
        assert_eq!(box2.pending(), vec![newer.clone()]);
        box2.acknowledge(&newer).unwrap();
        assert!(box2.record(newer.clone()).unwrap().is_none());
        let mut finished = newer.clone();
        finished.finished = true;
        box2.record(finished.clone()).unwrap();
        box2.acknowledge(&newer).unwrap();
        assert_eq!(box2.pending(), vec![finished.clone()]);
        box2.acknowledge(&finished).unwrap();
        drop(box2);
        assert!(open().pending().is_empty());
        fs::remove_dir_all(directory).unwrap();
    }
}
