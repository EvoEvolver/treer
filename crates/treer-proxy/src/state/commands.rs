use super::*;

impl AppState {
    pub async fn send_command(
        &self,
        workspace_id: &str,
        server_id: &str,
        command: AgentCommand,
    ) -> Result<Value, ProtocolError> {
        let command_id = format!("cmd_{}", Uuid::new_v4().simple());
        if !self.inner.cluster.is_distributed() {
            return self
                .send_local_command(workspace_id, server_id, None, command_id, command)
                .await;
        }
        let owner = self
            .inner
            .cluster
            .owner(workspace_id, server_id)
            .await?
            .ok_or_else(|| ProtocolError::new("server_offline", server_id))?;
        if owner.proxy_id == self.inner.cluster.instance_id() {
            self.send_local_command(
                workspace_id,
                server_id,
                Some(owner.connection_id),
                command_id,
                command,
            )
            .await
        } else {
            self.inner
                .cluster
                .request_command(&owner, workspace_id, server_id, command_id, command)
                .await
        }
    }

    pub(super) async fn send_local_command(
        &self,
        workspace_id: &str,
        server_id: &str,
        expected_connection_id: Option<Uuid>,
        command_id: String,
        command: AgentCommand,
    ) -> Result<Value, ProtocolError> {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        let outgoing = self
            .inner
            .connections
            .read()
            .await
            .get(&key)
            .filter(|connection| {
                expected_connection_id.is_none_or(|expected| expected == connection.connection_id)
            })
            .map(|connection| (connection.outgoing.clone(), connection.capabilities.clone()))
            .ok_or_else(|| ProtocolError::new("server_offline", server_id))?;

        let required_capability = command.required_capability();
        if let Some(required) = required_capability {
            if !outgoing.1.contains(required) {
                return Err(ProtocolError::new(
                    "unsupported_command",
                    format!("machine {server_id} does not advertise capability {required}"),
                ));
            }
        }
        let outgoing = outgoing.0;

        let envelope = CommandEnvelope {
            command_id: command_id.clone(),
            workspace_id: workspace_id.to_string(),
            command,
        };
        let encoded =
            serde_json::to_string(&ProxyMessage::Command { envelope }).map_err(|err| {
                ProtocolError::new("encode_error", format!("failed to encode command: {err}"))
            })?;
        let (result_tx, result_rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(
            command_id.clone(),
            PendingCommand {
                server: key,
                encoded: encoded.clone(),
                required_capability,
                result: result_tx,
            },
        );
        if outgoing.send(SocketFrame::Text(encoded)).is_err() {
            self.inner.pending.lock().await.remove(&command_id);
            return Err(ProtocolError::new("server_offline", server_id));
        }

        let result = match tokio::time::timeout(COMMAND_TIMEOUT, result_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                self.inner.pending.lock().await.remove(&command_id);
                return Err(ProtocolError::new(
                    "command_cancelled",
                    "agent server disconnected before returning a result",
                ));
            }
            Err(_) => {
                self.inner.pending.lock().await.remove(&command_id);
                return Err(ProtocolError::new(
                    "command_timeout",
                    format!("command {command_id} timed out"),
                ));
            }
        };
        if let Some(error) = result.error {
            Err(error)
        } else {
            Ok(result.data.unwrap_or(Value::Null))
        }
    }

    pub(crate) async fn handle_cluster_command(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        command_id: String,
        command: AgentCommand,
    ) -> Result<Value, ProtocolError> {
        self.send_local_command(
            workspace_id,
            server_id,
            Some(connection_id),
            command_id,
            command,
        )
        .await
    }

    pub async fn complete_command(&self, result: CommandResult) {
        if let Some(pending) = self.inner.pending.lock().await.remove(&result.command_id) {
            let _ = pending.result.send(result);
        }
    }

    pub(super) async fn resend_pending(&self, workspace_id: &str, server_id: &str) {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        let Some((outgoing, capabilities)) = self
            .inner
            .connections
            .read()
            .await
            .get(&key)
            .map(|connection| (connection.outgoing.clone(), connection.capabilities.clone()))
        else {
            return;
        };
        let (commands, rejected) = {
            let mut pending = self.inner.pending.lock().await;
            let rejected_ids = pending
                .iter()
                .filter(|(_, pending)| {
                    pending.server == key
                        && pending
                            .required_capability
                            .is_some_and(|required| !capabilities.contains(required))
                })
                .map(|(command_id, _)| command_id.clone())
                .collect::<Vec<_>>();
            let rejected = rejected_ids
                .into_iter()
                .filter_map(|command_id| {
                    pending
                        .remove(&command_id)
                        .map(|pending| (command_id, pending))
                })
                .collect::<Vec<_>>();
            let commands = pending
                .values()
                .filter(|pending| pending.server == key)
                .map(|pending| pending.encoded.clone())
                .collect::<Vec<_>>();
            (commands, rejected)
        };
        for (command_id, pending) in rejected {
            let required = pending.required_capability.unwrap_or("unknown");
            let _ = pending.result.send(CommandResult::failure(
                command_id,
                ProtocolError::new(
                    "unsupported_command",
                    format!("machine {server_id} does not advertise capability {required}"),
                ),
            ));
        }
        for command in commands {
            let _ = outgoing.send(SocketFrame::Text(command));
        }
    }
}
