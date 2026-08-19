use std::time::Duration;

use anyhow::{Context, Result};
use async_nats::jetstream;
use async_nats::jetstream::kv::{self, Operation, Store};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};
use treer_protocol::{
    AgentCommand, AgentServerSnapshot, AgentUi, ProtocolError, ServerStatus, WorkspaceInfo,
};
use uuid::Uuid;

use crate::state::{AppState, SocketFrame};

const LEASE_BUCKET: &str = "TREER_LIVE_OWNERS";
const SNAPSHOT_BUCKET: &str = "TREER_MACHINE_INVENTORY";
const PROJECTION_BUCKET: &str = "TREER_CONTROL_PROJECTIONS";
pub(crate) const LIVE_STATE_TTL: Duration = Duration::from_secs(30);
const CLUSTER_REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ConnectionOwner {
    pub proxy_id: String,
    pub connection_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ClusterServerSnapshot {
    pub owner: ConnectionOwner,
    pub revision: u64,
    pub snapshot: AgentServerSnapshot,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) enum ClusterSessionKind {
    Terminal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ClusterSessionDelivery {
    pub kind: ClusterSessionKind,
    pub workspace_id: String,
    pub server_id: String,
    pub session_id: String,
    pub revision: Option<u64>,
    pub close: bool,
    pub frame: SocketFrame,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum ClusterProjectionUpdate {
    WorkspaceUpsert {
        workspace: WorkspaceInfo,
    },
    ServerRenamed {
        workspace_id: String,
        server_id: String,
        name: String,
    },
    AgentRenamed {
        workspace_id: String,
        agent_id: String,
        name: String,
    },
    ServerDeleted {
        workspace_id: String,
        server_id: String,
    },
    AgentDeleted {
        workspace_id: String,
        agent_id: String,
    },
    AgentUiSet {
        ui: AgentUi,
    },
    AgentUiCleared {
        workspace_id: String,
        agent_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum ClusterRequest {
    Command {
        workspace_id: String,
        server_id: String,
        connection_id: Uuid,
        command_id: String,
        command: AgentCommand,
    },
    Socket {
        workspace_id: String,
        server_id: String,
        connection_id: Uuid,
        frame: SocketFrame,
    },
    SessionDelivery(ClusterSessionDelivery),
    NetworkDelivery {
        workspace_id: String,
        server_id: String,
        #[serde(with = "serde_bytes")]
        encoded: Vec<u8>,
    },
    WorkspaceBroadcast {
        origin_proxy: String,
        workspace_id: String,
        frame: SocketFrame,
    },
}

#[derive(Debug, Serialize, Deserialize)]
enum ClusterReply {
    Command(Result<Value, ProtocolError>),
    Ack(Result<(), ProtocolError>),
}

#[derive(Clone)]
pub(crate) struct ClusterBus {
    instance_id: String,
    backend: Option<NatsCluster>,
}

#[derive(Clone)]
struct NatsCluster {
    client: async_nats::Client,
    leases: Store,
    snapshots: Store,
    projections: Store,
    subject_prefix: String,
}

impl ClusterBus {
    pub fn standalone(instance_id: String) -> Self {
        Self {
            instance_id,
            backend: None,
        }
    }

    pub async fn connect(
        nats_url: &str,
        instance_id: String,
        subject_prefix: String,
    ) -> Result<Self> {
        validate_subject_prefix(&subject_prefix)?;
        let client = async_nats::connect(nats_url)
            .await
            .context("failed to connect cluster backplane to configured NATS server")?;
        let context = jetstream::new(client.clone());
        let leases = open_kv(
            &context,
            LEASE_BUCKET,
            "Treer expiring Controller ownership leases",
            LIVE_STATE_TTL,
            jetstream::stream::StorageType::Memory,
        )
        .await?;
        let snapshots = open_kv(
            &context,
            SNAPSHOT_BUCKET,
            "Treer retained machine inventory snapshots",
            Duration::ZERO,
            jetstream::stream::StorageType::File,
        )
        .await?;
        let projections = open_kv(
            &context,
            PROJECTION_BUCKET,
            "Treer durable control-plane projections",
            Duration::ZERO,
            jetstream::stream::StorageType::File,
        )
        .await?;
        info!(%instance_id, %subject_prefix, "NATS cluster backplane ready");
        Ok(Self {
            instance_id,
            backend: Some(NatsCluster {
                client,
                leases,
                snapshots,
                projections,
                subject_prefix,
            }),
        })
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn is_distributed(&self) -> bool {
        self.backend.is_some()
    }

    pub fn routed_id(&self, kind: &str) -> String {
        format!(
            "{kind}.{}.{}",
            URL_SAFE_NO_PAD.encode(self.instance_id.as_bytes()),
            Uuid::new_v4().simple()
        )
    }

    pub fn route_target(routed_id: &str) -> Option<String> {
        let mut parts = routed_id.split('.');
        let _kind = parts.next()?;
        let encoded = parts.next()?;
        let _unique = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        String::from_utf8(URL_SAFE_NO_PAD.decode(encoded).ok()?).ok()
    }

    pub async fn start(&self, state: AppState) -> Result<()> {
        let Some(backend) = self.backend.clone() else {
            return Ok(());
        };
        let direct_subject = backend.proxy_subject(&self.instance_id);
        let mut direct = backend
            .client
            .subscribe(direct_subject.clone())
            .await
            .with_context(|| format!("failed to subscribe to {direct_subject}"))?;
        let direct_client = backend.client.clone();
        let direct_state = state.clone();
        tokio::spawn(async move {
            while let Some(message) = direct.next().await {
                let state = direct_state.clone();
                let client = direct_client.clone();
                tokio::spawn(async move {
                    let reply = match decode::<ClusterRequest>(&message.payload) {
                        Ok(ClusterRequest::Command {
                            workspace_id,
                            server_id,
                            connection_id,
                            command_id,
                            command,
                        }) => ClusterReply::Command(
                            state
                                .handle_cluster_command(
                                    &workspace_id,
                                    &server_id,
                                    connection_id,
                                    command_id,
                                    command,
                                )
                                .await,
                        ),
                        Ok(ClusterRequest::Socket {
                            workspace_id,
                            server_id,
                            connection_id,
                            frame,
                        }) => ClusterReply::Ack(
                            state
                                .handle_cluster_socket(
                                    &workspace_id,
                                    &server_id,
                                    connection_id,
                                    frame,
                                )
                                .await,
                        ),
                        Ok(ClusterRequest::SessionDelivery(delivery)) => {
                            ClusterReply::Ack(state.handle_cluster_session_delivery(delivery).await)
                        }
                        Ok(ClusterRequest::NetworkDelivery {
                            workspace_id,
                            server_id,
                            encoded,
                        }) => ClusterReply::Ack(
                            state
                                .handle_cluster_network_delivery(&workspace_id, &server_id, encoded)
                                .await,
                        ),
                        Ok(ClusterRequest::WorkspaceBroadcast { .. }) => {
                            ClusterReply::Ack(Err(ProtocolError::new(
                                "invalid_cluster_message",
                                "workspace broadcasts must use the broadcast subject",
                            )))
                        }
                        Err(error) => ClusterReply::Ack(Err(ProtocolError::new(
                            "invalid_cluster_message",
                            error.to_string(),
                        ))),
                    };
                    let Some(reply_subject) = message.reply else {
                        return;
                    };
                    match encode(&reply) {
                        Ok(payload) => {
                            if let Err(error) = client.publish(reply_subject, payload.into()).await
                            {
                                warn!(%error, "failed to publish cluster response");
                            }
                        }
                        Err(error) => warn!(%error, "failed to encode cluster response"),
                    }
                });
            }
        });

        let broadcast_subject = backend.broadcast_subject();
        let mut broadcasts = backend
            .client
            .subscribe(broadcast_subject.clone())
            .await
            .with_context(|| format!("failed to subscribe to {broadcast_subject}"))?;
        let broadcast_state = state.clone();
        let local_instance = self.instance_id.clone();
        tokio::spawn(async move {
            while let Some(message) = broadcasts.next().await {
                match decode::<ClusterRequest>(&message.payload) {
                    Ok(ClusterRequest::WorkspaceBroadcast {
                        origin_proxy,
                        workspace_id,
                        frame,
                    }) if origin_proxy != local_instance => {
                        broadcast_state
                            .handle_cluster_workspace_broadcast(&workspace_id, frame)
                            .await;
                    }
                    Ok(ClusterRequest::WorkspaceBroadcast { .. }) => {}
                    Ok(_) => warn!("non-broadcast message received on cluster broadcast subject"),
                    Err(error) => warn!(%error, "failed to decode cluster broadcast"),
                }
            }
        });

        let mut projection_keys = backend
            .projections
            .keys()
            .await
            .context("failed to enumerate durable control projections")?;
        while let Some(key) = projection_keys.next().await {
            let key = key.context("failed to read a durable control projection key")?;
            let Some(entry) = backend
                .projections
                .entry(&key)
                .await
                .context("failed to read a durable control projection")?
            else {
                continue;
            };
            if entry.operation != Operation::Put {
                continue;
            }
            match decode::<ClusterProjectionUpdate>(&entry.value) {
                Ok(update) => state.apply_cluster_projection(update).await,
                Err(error) => {
                    warn!(key = %entry.key, %error, "failed to decode durable control projection");
                }
            }
        }

        let mut snapshot_keys = backend
            .snapshots
            .keys()
            .await
            .context("failed to enumerate retained machine snapshots")?;
        while let Some(key) = snapshot_keys.next().await {
            let key = key.context("failed to read a retained machine snapshot key")?;
            let Some(entry) = backend
                .snapshots
                .entry(&key)
                .await
                .context("failed to read a retained machine snapshot")?
            else {
                continue;
            };
            if entry.operation != Operation::Put {
                continue;
            }
            match decode::<ClusterServerSnapshot>(&entry.value) {
                Ok(snapshot) => {
                    apply_inventory_snapshot(
                        &state,
                        &backend.leases,
                        &self.instance_id,
                        entry.revision,
                        snapshot,
                    )
                    .await;
                }
                Err(error) => {
                    warn!(key = %entry.key, %error, "failed to decode retained machine snapshot");
                }
            }
        }

        let mut snapshots = backend
            .snapshots
            .watch_with_history(">")
            .await
            .context("failed to watch live machine snapshots")?;
        let snapshot_state = state.clone();
        let snapshot_leases = backend.leases.clone();
        let local_instance = self.instance_id.clone();
        tokio::spawn(async move {
            while let Some(entry) = snapshots.next().await {
                match entry {
                    Ok(entry) if entry.operation == Operation::Put => {
                        match decode::<ClusterServerSnapshot>(&entry.value) {
                            Ok(snapshot) => {
                                apply_inventory_snapshot(
                                    &snapshot_state,
                                    &snapshot_leases,
                                    &local_instance,
                                    entry.revision,
                                    snapshot,
                                )
                                .await;
                            }
                            Err(error) => {
                                warn!(key = %entry.key, %error, "failed to decode live machine snapshot");
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(error) => warn!(%error, "live machine snapshot watch failed"),
                }
            }
        });

        let mut leases = backend
            .leases
            .watch_with_history(">")
            .await
            .context("failed to watch Controller ownership leases")?;
        let lease_state = state.clone();
        tokio::spawn(async move {
            while let Some(entry) = leases.next().await {
                match entry {
                    Ok(entry) if entry.operation == Operation::Put => {
                        if let Some((workspace_id, server_id)) = decode_server_key(&entry.key) {
                            lease_state
                                .note_cluster_lease(&workspace_id, &server_id, entry.revision)
                                .await;
                        }
                    }
                    Ok(entry)
                        if matches!(entry.operation, Operation::Delete | Operation::Purge) =>
                    {
                        if let Some((workspace_id, server_id)) = decode_server_key(&entry.key) {
                            lease_state
                                .apply_cluster_disconnect(&workspace_id, &server_id, entry.revision)
                                .await;
                        }
                    }
                    Ok(_) => {}
                    Err(error) => warn!(%error, "Controller ownership watch failed"),
                }
            }
        });

        let mut projections = backend
            .projections
            .watch_with_history(">")
            .await
            .context("failed to watch durable control projections")?;
        let projection_state = state.clone();
        tokio::spawn(async move {
            while let Some(entry) = projections.next().await {
                match entry {
                    Ok(entry) if entry.operation == Operation::Put => {
                        match decode::<ClusterProjectionUpdate>(&entry.value) {
                            Ok(update) => projection_state.apply_cluster_projection(update).await,
                            Err(error) => {
                                warn!(key = %entry.key, %error, "failed to decode control projection")
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(error) => warn!(%error, "durable control projection watch failed"),
                }
            }
        });

        let expiry_state = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                expiry_state.expire_cluster_leases(LIVE_STATE_TTL).await;
            }
        });
        backend
            .client
            .flush()
            .await
            .context("failed to flush NATS cluster subscriptions")?;
        Ok(())
    }

    pub async fn claim(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        snapshot: AgentServerSnapshot,
    ) -> Result<(), ProtocolError> {
        let Some(backend) = &self.backend else {
            return Ok(());
        };
        let key = server_key(workspace_id, server_id);
        let owner = ConnectionOwner {
            proxy_id: self.instance_id.clone(),
            connection_id,
        };
        backend
            .leases
            .put(&key, encode_protocol(&owner)?.into())
            .await
            .map_err(cluster_error)?;
        let server_name = snapshot.server.name.clone();
        let agent_names = snapshot
            .agents
            .iter()
            .map(|agent| (agent.agent_id.clone(), agent.name.clone()))
            .collect::<Vec<_>>();
        if let Err(error) = self
            .broadcast_projection(ClusterProjectionUpdate::ServerRenamed {
                workspace_id: workspace_id.to_string(),
                server_id: server_id.to_string(),
                name: server_name,
            })
            .await
        {
            self.release(workspace_id, server_id, connection_id).await;
            return Err(error);
        }
        for (agent_id, name) in agent_names {
            if let Err(error) = self
                .broadcast_projection(ClusterProjectionUpdate::AgentRenamed {
                    workspace_id: workspace_id.to_string(),
                    agent_id,
                    name,
                })
                .await
            {
                self.release(workspace_id, server_id, connection_id).await;
                return Err(error);
            }
        }
        let mut update = ClusterServerSnapshot {
            owner,
            revision: 0,
            snapshot,
        };
        update.revision = backend
            .snapshots
            .put(&key, encode_protocol(&update)?.into())
            .await
            .map_err(cluster_error)?;
        Ok(())
    }

    pub async fn renew(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
    ) -> Result<bool, ProtocolError> {
        let Some(backend) = &self.backend else {
            return Ok(true);
        };
        let key = server_key(workspace_id, server_id);
        let Some(entry) = backend.leases.entry(&key).await.map_err(cluster_error)? else {
            return Ok(false);
        };
        let current: ConnectionOwner = decode_protocol(&entry.value)?;
        if current.proxy_id != self.instance_id || current.connection_id != connection_id {
            return Ok(false);
        }
        let owner = ConnectionOwner {
            proxy_id: self.instance_id.clone(),
            connection_id,
        };
        match backend
            .leases
            .update(&key, encode_protocol(&owner)?.into(), entry.revision)
            .await
        {
            Ok(_) => {}
            Err(_) => return Ok(false),
        }
        Ok(true)
    }

    pub async fn publish_snapshot(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        snapshot: AgentServerSnapshot,
    ) -> Result<(), ProtocolError> {
        let Some(backend) = &self.backend else {
            return Ok(());
        };
        let key = server_key(workspace_id, server_id);
        let entry = backend
            .leases
            .entry(&key)
            .await
            .map_err(cluster_error)?
            .ok_or_else(|| {
                ProtocolError::new("server_offline", format!("server {server_id} has no owner"))
            })?;
        let current: ConnectionOwner = decode_protocol(&entry.value)?;
        if current.proxy_id != self.instance_id || current.connection_id != connection_id {
            return Err(ProtocolError::new(
                "stale_connection",
                format!("connection for {server_id} is no longer current"),
            ));
        }
        let mut update = ClusterServerSnapshot {
            owner: current,
            revision: 0,
            snapshot,
        };
        update.revision = backend
            .snapshots
            .put(&key, encode_protocol(&update)?.into())
            .await
            .map_err(cluster_error)?;
        Ok(())
    }

    pub async fn release(&self, workspace_id: &str, server_id: &str, connection_id: Uuid) {
        let Some(backend) = &self.backend else {
            return;
        };
        let key = server_key(workspace_id, server_id);
        let Ok(Some(entry)) = backend.leases.entry(&key).await else {
            return;
        };
        let Ok(record) = decode::<ConnectionOwner>(&entry.value) else {
            return;
        };
        if record.proxy_id != self.instance_id || record.connection_id != connection_id {
            return;
        }
        if let Err(error) = backend
            .leases
            .delete_expect_revision(&key, Some(entry.revision))
            .await
        {
            warn!(%error, %server_id, "failed to release cluster connection owner");
        }
    }

    pub async fn delete_inventory(&self, workspace_id: &str, server_id: &str) {
        let Some(backend) = &self.backend else {
            return;
        };
        let key = server_key(workspace_id, server_id);
        if let Err(error) = backend.snapshots.delete(&key).await {
            warn!(%error, %server_id, "failed to delete retained machine inventory");
        }
    }

    pub async fn owner(
        &self,
        workspace_id: &str,
        server_id: &str,
    ) -> Result<Option<ConnectionOwner>, ProtocolError> {
        let Some(backend) = &self.backend else {
            return Ok(None);
        };
        let record = backend
            .leases
            .get(server_key(workspace_id, server_id))
            .await
            .map_err(cluster_error)?;
        record
            .map(|value| decode_protocol::<ConnectionOwner>(&value))
            .transpose()
    }

    pub async fn request_command(
        &self,
        owner: &ConnectionOwner,
        workspace_id: &str,
        server_id: &str,
        command_id: String,
        command: AgentCommand,
    ) -> Result<Value, ProtocolError> {
        let reply = self
            .request(
                &owner.proxy_id,
                ClusterRequest::Command {
                    workspace_id: workspace_id.to_string(),
                    server_id: server_id.to_string(),
                    connection_id: owner.connection_id,
                    command_id,
                    command,
                },
            )
            .await?;
        match reply {
            ClusterReply::Command(result) => result,
            ClusterReply::Ack(_) => Err(ProtocolError::new(
                "invalid_cluster_response",
                "expected command response",
            )),
        }
    }

    pub async fn send_socket(
        &self,
        owner: &ConnectionOwner,
        workspace_id: &str,
        server_id: &str,
        frame: SocketFrame,
    ) -> Result<(), ProtocolError> {
        let reply = self
            .request(
                &owner.proxy_id,
                ClusterRequest::Socket {
                    workspace_id: workspace_id.to_string(),
                    server_id: server_id.to_string(),
                    connection_id: owner.connection_id,
                    frame,
                },
            )
            .await?;
        match reply {
            ClusterReply::Ack(result) => result,
            ClusterReply::Command(_) => Err(ProtocolError::new(
                "invalid_cluster_response",
                "expected socket acknowledgement",
            )),
        }
    }

    pub async fn deliver_session(
        &self,
        target_proxy: &str,
        delivery: ClusterSessionDelivery,
    ) -> Result<(), ProtocolError> {
        match self
            .request(target_proxy, ClusterRequest::SessionDelivery(delivery))
            .await?
        {
            ClusterReply::Ack(result) => result,
            ClusterReply::Command(_) => Err(ProtocolError::new(
                "invalid_cluster_response",
                "expected session acknowledgement",
            )),
        }
    }

    pub async fn deliver_network(
        &self,
        target_proxy: &str,
        workspace_id: &str,
        server_id: &str,
        encoded: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        match self
            .request(
                target_proxy,
                ClusterRequest::NetworkDelivery {
                    workspace_id: workspace_id.to_string(),
                    server_id: server_id.to_string(),
                    encoded,
                },
            )
            .await?
        {
            ClusterReply::Ack(result) => result,
            ClusterReply::Command(_) => Err(ProtocolError::new(
                "invalid_cluster_response",
                "expected network acknowledgement",
            )),
        }
    }

    pub async fn broadcast_workspace(
        &self,
        workspace_id: &str,
        frame: SocketFrame,
    ) -> Result<(), ProtocolError> {
        let Some(backend) = &self.backend else {
            return Ok(());
        };
        let payload = encode_protocol(&ClusterRequest::WorkspaceBroadcast {
            origin_proxy: self.instance_id.clone(),
            workspace_id: workspace_id.to_string(),
            frame,
        })?;
        backend
            .client
            .publish(backend.broadcast_subject(), payload.into())
            .await
            .map_err(cluster_error)
    }

    pub async fn broadcast_projection(
        &self,
        update: ClusterProjectionUpdate,
    ) -> Result<(), ProtocolError> {
        let Some(backend) = &self.backend else {
            return Ok(());
        };
        let key = projection_key(&update);
        let payload = encode_protocol(&update)?;
        backend
            .projections
            .put(&key, payload.into())
            .await
            .map(|_| ())
            .map_err(cluster_error)
    }

    async fn request(
        &self,
        target_proxy: &str,
        request: ClusterRequest,
    ) -> Result<ClusterReply, ProtocolError> {
        let backend = self.backend.as_ref().ok_or_else(|| {
            ProtocolError::new(
                "cluster_unavailable",
                "NATS cluster routing is not configured",
            )
        })?;
        let subject = backend.proxy_subject(target_proxy);
        let payload = encode_protocol(&request)?;
        let response = tokio::time::timeout(
            CLUSTER_REQUEST_TIMEOUT,
            backend.client.request(subject, payload.into()),
        )
        .await
        .map_err(|_| ProtocolError::new("cluster_timeout", "cluster request timed out"))?
        .map_err(cluster_error)?;
        decode_protocol(&response.payload)
    }
}

impl NatsCluster {
    fn proxy_subject(&self, proxy_id: &str) -> String {
        format!(
            "{}.proxy.{}",
            self.subject_prefix,
            URL_SAFE_NO_PAD.encode(proxy_id.as_bytes())
        )
    }

    fn broadcast_subject(&self) -> String {
        format!("{}.broadcast", self.subject_prefix)
    }
}

async fn apply_inventory_snapshot(
    state: &AppState,
    leases: &Store,
    local_instance: &str,
    revision: u64,
    mut snapshot: ClusterServerSnapshot,
) {
    let key = server_key(
        &snapshot.snapshot.server.workspace_id,
        &snapshot.snapshot.server.server_id,
    );
    let current_owner = match leases.get(&key).await {
        Ok(Some(value)) => match decode::<ConnectionOwner>(&value) {
            Ok(owner) => Some(owner),
            Err(error) => {
                warn!(%key, %error, "failed to decode Controller ownership lease");
                return;
            }
        },
        Ok(None) => None,
        Err(error) => {
            warn!(%key, %error, "failed to read Controller ownership lease");
            return;
        }
    };
    if current_owner
        .as_ref()
        .is_some_and(|owner| owner == &snapshot.owner && owner.proxy_id == local_instance)
    {
        return;
    }
    snapshot.snapshot.server.status = if current_owner.is_some() {
        ServerStatus::Online
    } else {
        ServerStatus::Offline
    };
    snapshot.revision = revision;
    state.apply_cluster_snapshot(snapshot).await;
}

async fn open_kv(
    context: &jetstream::Context,
    bucket: &str,
    description: &str,
    max_age: Duration,
    storage: jetstream::stream::StorageType,
) -> Result<Store> {
    match context.get_key_value(bucket).await {
        Ok(store) => Ok(store),
        Err(_) => match context
            .create_key_value(kv::Config {
                bucket: bucket.to_string(),
                description: description.to_string(),
                history: 1,
                max_age,
                storage,
                ..Default::default()
            })
            .await
        {
            Ok(store) => Ok(store),
            Err(_) => context
                .get_key_value(bucket)
                .await
                .with_context(|| format!("failed to open NATS KV bucket {bucket}")),
        },
    }
}

fn server_key(workspace_id: &str, server_id: &str) -> String {
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(workspace_id.as_bytes()),
        URL_SAFE_NO_PAD.encode(server_id.as_bytes())
    )
}

fn projection_key(update: &ClusterProjectionUpdate) -> String {
    match update {
        ClusterProjectionUpdate::WorkspaceUpsert { workspace } => format!(
            "workspace.{}",
            URL_SAFE_NO_PAD.encode(workspace.workspace_id.as_bytes())
        ),
        ClusterProjectionUpdate::ServerRenamed {
            workspace_id,
            server_id,
            ..
        }
        | ClusterProjectionUpdate::ServerDeleted {
            workspace_id,
            server_id,
        } => format!(
            "server.{}.{}",
            URL_SAFE_NO_PAD.encode(workspace_id.as_bytes()),
            URL_SAFE_NO_PAD.encode(server_id.as_bytes())
        ),
        ClusterProjectionUpdate::AgentRenamed {
            workspace_id,
            agent_id,
            ..
        }
        | ClusterProjectionUpdate::AgentDeleted {
            workspace_id,
            agent_id,
        } => format!(
            "agent.{}.{}",
            URL_SAFE_NO_PAD.encode(workspace_id.as_bytes()),
            URL_SAFE_NO_PAD.encode(agent_id.as_bytes())
        ),
        ClusterProjectionUpdate::AgentUiSet { ui } => format!(
            "agent-ui.{}.{}",
            URL_SAFE_NO_PAD.encode(ui.workspace_id.as_bytes()),
            URL_SAFE_NO_PAD.encode(ui.agent_id.as_bytes())
        ),
        ClusterProjectionUpdate::AgentUiCleared {
            workspace_id,
            agent_id,
        } => format!(
            "agent-ui.{}.{}",
            URL_SAFE_NO_PAD.encode(workspace_id.as_bytes()),
            URL_SAFE_NO_PAD.encode(agent_id.as_bytes())
        ),
    }
}

fn decode_server_key(key: &str) -> Option<(String, String)> {
    let (workspace, server) = key.split_once('.')?;
    Some((
        String::from_utf8(URL_SAFE_NO_PAD.decode(workspace).ok()?).ok()?,
        String::from_utf8(URL_SAFE_NO_PAD.decode(server).ok()?).ok()?,
    ))
}

fn validate_subject_prefix(prefix: &str) -> Result<()> {
    if prefix.is_empty()
        || !prefix.split('.').all(|token| {
            !token.is_empty()
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        anyhow::bail!(
            "NATS cluster subject prefix must contain only non-empty alphanumeric, '-', or '_' tokens"
        );
    }
    Ok(())
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    rmp_serde::to_vec_named(value).context("failed to encode cluster message")
}

fn decode<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> Result<T> {
    rmp_serde::from_slice(payload).context("failed to decode cluster message")
}

fn encode_protocol<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
    encode(value).map_err(|error| ProtocolError::new("cluster_encode_error", error.to_string()))
}

fn decode_protocol<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> Result<T, ProtocolError> {
    decode(payload).map_err(|error| ProtocolError::new("cluster_decode_error", error.to_string()))
}

fn cluster_error(error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new("cluster_error", error.to_string())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use treer_protocol::{AgentInfo, AgentStatus, ServerInfo, ServerStatus};

    use super::*;

    #[test]
    fn routed_ids_round_trip_proxy_identity() {
        let bus = ClusterBus::standalone("proxy.with symbols/1".to_string());
        let id = bus.routed_id("term");
        assert_eq!(
            ClusterBus::route_target(&id).as_deref(),
            Some("proxy.with symbols/1")
        );
    }

    #[test]
    fn server_keys_round_trip_arbitrary_ids() {
        let key = server_key("workspace/alpha", "server.one");
        assert_eq!(
            decode_server_key(&key),
            Some(("workspace/alpha".to_string(), "server.one".to_string()))
        );
    }

    #[test]
    fn messagepack_cluster_frames_preserve_raw_binary_payloads() {
        let request = ClusterRequest::Socket {
            workspace_id: "alpha".to_string(),
            server_id: "server".to_string(),
            connection_id: Uuid::nil(),
            frame: SocketFrame::Binary((0..=u8::MAX).collect()),
        };
        let encoded = encode(&request).expect("encode cluster frame");
        let decoded: ClusterRequest = decode(&encoded).expect("decode cluster frame");
        assert!(matches!(
            decoded,
            ClusterRequest::Socket {
                frame: SocketFrame::Binary(payload),
                ..
            } if payload == (0..=u8::MAX).collect::<Vec<_>>()
        ));
    }

    #[tokio::test]
    async fn heartbeat_renewal_only_updates_the_small_lease_record() {
        let Ok(nats_url) = std::env::var("TREER_TEST_NATS_URL") else {
            return;
        };
        let suffix = Uuid::new_v4().simple().to_string();
        let workspace_id = format!("workspace-{suffix}");
        let server_id = format!("server-{suffix}");
        let connection_id = Uuid::new_v4();
        let bus = ClusterBus::connect(
            &nats_url,
            format!("proxy-{suffix}"),
            format!("treer.test.cluster.{suffix}"),
        )
        .await
        .expect("connect cluster bus");
        let now = Utc::now();
        bus.claim(
            &workspace_id,
            &server_id,
            connection_id,
            AgentServerSnapshot {
                server: ServerInfo {
                    server_id: server_id.clone(),
                    workspace_id: workspace_id.clone(),
                    name: "test server".to_string(),
                    hostname: "test".to_string(),
                    root: "/tmp".to_string(),
                    labels: Default::default(),
                    status: ServerStatus::Online,
                    connected_at: now,
                    last_seen_at: now,
                },
                agents: vec![AgentInfo {
                    agent_id: format!("agent-{suffix}"),
                    workspace_id: workspace_id.clone(),
                    server_id: server_id.clone(),
                    kind: "command".to_string(),
                    name: "test agent".to_string(),
                    cwd: ".".to_string(),
                    status: AgentStatus::Idle,
                    pid: None,
                    started_at: now,
                    updated_at: now,
                    exited_at: None,
                    exit_code: None,
                    output_revision: 0,
                }],
            },
        )
        .await
        .expect("claim server");

        let backend = bus.backend.as_ref().expect("NATS backend");
        let key = server_key(&workspace_id, &server_id);
        let snapshot_revision = backend
            .snapshots
            .entry(&key)
            .await
            .expect("read snapshot")
            .expect("snapshot entry")
            .revision;
        let lease_revision = backend
            .leases
            .entry(&key)
            .await
            .expect("read lease")
            .expect("lease entry")
            .revision;

        assert!(bus
            .renew(&workspace_id, &server_id, connection_id)
            .await
            .expect("renew lease"));

        assert_eq!(
            backend
                .snapshots
                .entry(&key)
                .await
                .expect("read snapshot after renewal")
                .expect("snapshot after renewal")
                .revision,
            snapshot_revision
        );
        assert!(
            backend
                .leases
                .entry(&key)
                .await
                .expect("read lease after renewal")
                .expect("lease after renewal")
                .revision
                > lease_revision
        );

        bus.release(&workspace_id, &server_id, connection_id).await;
        assert!(backend
            .leases
            .get(&key)
            .await
            .expect("read released lease")
            .is_none());
        let retained = backend
            .snapshots
            .entry(&key)
            .await
            .expect("read retained inventory")
            .expect("inventory survives lease release");
        let restored_state = AppState::new();
        apply_inventory_snapshot(
            &restored_state,
            &backend.leases,
            "restored-proxy",
            retained.revision,
            decode(&retained.value).expect("decode retained inventory"),
        )
        .await;
        let restored = restored_state
            .snapshot(&workspace_id)
            .await
            .expect("restored workspace inventory");
        assert_eq!(restored.servers.len(), 1);
        assert_eq!(restored.servers[0].status, ServerStatus::Offline);
        assert_eq!(restored.agents.len(), 1);

        bus.delete_inventory(&workspace_id, &server_id).await;
        assert!(backend
            .snapshots
            .get(&key)
            .await
            .expect("read deleted inventory")
            .is_none());
        let projection = ClusterProjectionUpdate::ServerRenamed {
            workspace_id,
            server_id,
            name: "test server".to_string(),
        };
        backend
            .projections
            .delete(&projection_key(&projection))
            .await
            .expect("clean projection");
    }
}
