//! Read-only actor custody for an allowlisted native snapshot, never a handoff.
use super::*;

impl Runtime {
    fn snapshot_idle(&self, thread: &Value) -> Result<()> {
        if !self.claude || !self.ready || !thread.is_string() || *thread != self.thread["id"] {
            bail!("snapshot_thread_unavailable");
        }
        if self.dispatch_blocked
            || self.history_failed
            || self.state_failed
            || self.transport_failed
            || self.recovering
            || self.rpc.failed()
            || !self.rpc.claude_idle_for_disposition()
            || self.thread["status"] != "idle"
            || !self.thread["activeTurnId"].is_null()
            || self.records.iter().any(receipts::pending)
            || !self.goals.goal.is_null()
            || self.claude_goal_wait.is_some()
            || self
                .terminal_tasks
                .as_array()
                .is_some_and(|tasks| tasks.iter().any(|t| t["status"] == "running"))
        {
            bail!("snapshot_requires_idle_without_obligations");
        }
        Ok(())
    }
    pub(super) async fn session_snapshot(&mut self, body: Value) -> Result<Value> {
        self.snapshot_idle(&body["threadId"])?;
        let (home, cwd) = self
            .snapshot_source
            .clone()
            .ok_or_else(|| anyhow!("snapshot_explicit_config_required"))?;
        if !home.is_absolute() {
            bail!("snapshot_explicit_config_required");
        }
        // A timed-out/cancelled blocking filesystem read cannot be forcibly
        // stopped. Its permit stays with that worker until it really exits.
        let permit = self
            .snapshot_read_slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow!("snapshot_reader_busy"))?;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (home, cwd, permit);
            bail!("snapshot_platform_unsupported");
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let thread = body["threadId"].as_str().unwrap().to_owned();
            let source_thread = thread.clone();
            let mut task = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                crate::session_transfer::managed_snapshot(&home, &cwd, &source_thread)
            });
            let deadline = tokio::time::sleep(Duration::from_secs(30));
            tokio::pin!(deadline);
            let mut changed = false;
            let mut value = loop {
                tokio::select! {
                    biased;
                    _ = &mut deadline => {task.abort();bail!("snapshot_deadline");},
                    event = self.events.recv() => {
                        changed=true;
                        let event=event.ok_or_else(||anyhow!("snapshot_transport_unavailable"))?;
                        if event["method"]=="spike/provider-exit" {self.stop_for_transport_error()?;bail!("snapshot_transport_unavailable");}
                        self.event(event).await?;
                    },
                    result = &mut task => break result.map_err(|_|anyhow!("snapshot_reader_failed"))??,
                }
            };
            // Completion must not win over a queued provider change. Bound this
            // drain as well; a continuously noisy provider refuses the snapshot.
            for _ in 0..64 {
                if deadline.is_elapsed() {
                    bail!("snapshot_deadline");
                }
                match self.events.try_recv() {
                    Ok(event) => {
                        changed = true;
                        if event["method"] == "spike/provider-exit" {
                            self.stop_for_transport_error()?;
                            bail!("snapshot_transport_unavailable");
                        }
                        self.event(event).await?;
                    }
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        bail!("snapshot_transport_unavailable")
                    }
                }
            }
            if changed {
                bail!("snapshot_provider_changed");
            }
            self.snapshot_idle(&json!(thread))?;
            self.store
                .recheck()
                .map_err(|_| anyhow!("snapshot_storage_changed"))?;
            value["runtimeSnapshot"] = json!({"schema":"treer.idle-claude-snapshot/v1","threadId":thread,"custody":"current_initialized_adapter_idle","includesLauncherState":false,"providerCheckpoint":false});
            Ok(value)
        }
    }
}
