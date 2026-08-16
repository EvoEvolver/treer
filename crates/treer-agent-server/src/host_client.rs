use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex};
use treer_host_protocol::{
    HostCommand, HostError, HostMessage, HostOutputChunk, HostProcessInfo, HostRequest,
    HostResponse, OutputCursor, HOST_PROTOCOL_VERSION,
};
use uuid::Uuid;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
type PendingRequests = HashMap<String, oneshot::Sender<Result<HostResponse, HostError>>>;

#[derive(Clone)]
pub struct HostClient {
    outgoing: mpsc::UnboundedSender<HostRequest>,
    pending: Arc<Mutex<PendingRequests>>,
}

pub struct HostEvents {
    pub output: broadcast::Receiver<HostOutputChunk>,
    pub processes: broadcast::Receiver<HostProcessInfo>,
    pub disconnected: watch::Receiver<bool>,
}

impl HostClient {
    pub async fn connect(path: &Path) -> Result<(Self, HostEvents)> {
        let stream = UnixStream::connect(path)
            .await
            .with_context(|| format!("failed to connect to host {}", path.display()))?;
        let (reader, mut writer) = stream.into_split();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<HostRequest>();
        let pending = Arc::new(Mutex::new(PendingRequests::new()));
        let (output_tx, output_rx) = broadcast::channel(2048);
        let (process_tx, process_rx) = broadcast::channel(512);
        let (disconnected_tx, disconnected_rx) = watch::channel(false);

        tokio::spawn(async move {
            while let Some(request) = outgoing_rx.recv().await {
                let mut encoded = match serde_json::to_vec(&request) {
                    Ok(encoded) => encoded,
                    Err(_) => break,
                };
                encoded.push(b'\n');
                if writer.write_all(&encoded).await.is_err() {
                    break;
                }
            }
        });

        let reader_pending = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            loop {
                let line = match lines.next_line().await {
                    Ok(Some(line)) => line,
                    Ok(None) | Err(_) => break,
                };
                let Ok(message) = serde_json::from_str::<HostMessage>(&line) else {
                    break;
                };
                match message {
                    HostMessage::Response {
                        request_id,
                        response,
                        error,
                    } => {
                        if let Some(sender) = reader_pending.lock().await.remove(&request_id) {
                            let result = match (response, error) {
                                (Some(response), None) => Ok(response),
                                (_, Some(error)) => Err(error),
                                _ => Err(HostError::new(
                                    "invalid_response",
                                    "host response had no result",
                                )),
                            };
                            let _ = sender.send(result);
                        }
                    }
                    HostMessage::Output { chunk } => {
                        let _ = output_tx.send(chunk);
                    }
                    HostMessage::Process { process } => {
                        let _ = process_tx.send(process);
                    }
                }
            }
            let _ = disconnected_tx.send(true);
            let mut pending = reader_pending.lock().await;
            for (_, sender) in pending.drain() {
                let _ = sender.send(Err(HostError::new(
                    "host_disconnected",
                    "host connection closed",
                )));
            }
        });

        Ok((
            Self {
                outgoing: outgoing_tx,
                pending,
            },
            HostEvents {
                output: output_rx,
                processes: process_rx,
                disconnected: disconnected_rx,
            },
        ))
    }

    pub async fn sync(&self, cursors: BTreeMap<String, OutputCursor>) -> Result<HostResponse> {
        self.request(HostCommand::Sync { cursors }, None).await
    }

    pub async fn request(
        &self,
        command: HostCommand,
        operation_id: Option<String>,
    ) -> Result<HostResponse> {
        let request_id = format!("req_{}", Uuid::new_v4().simple());
        let request = HostRequest {
            protocol: HOST_PROTOCOL_VERSION,
            request_id: request_id.clone(),
            operation_id,
            command,
        };
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(request_id.clone(), sender);
        if self.outgoing.send(request).is_err() {
            self.pending.lock().await.remove(&request_id);
            return Err(anyhow!("host request channel closed"));
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, receiver).await {
            Ok(Ok(Ok(response))) => Ok(response),
            Ok(Ok(Err(error))) => Err(anyhow!("{}: {}", error.code, error.message)),
            Ok(Err(_)) => Err(anyhow!("host response channel closed")),
            Err(_) => {
                self.pending.lock().await.remove(&request_id);
                Err(anyhow!("host request {request_id} timed out"))
            }
        }
    }
}
