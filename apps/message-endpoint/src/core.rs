//! Fixed CLI operations under this process's injected workload identity.
//! No shell, caller-supplied argv, credential inspection or raw error disclosure.
use crate::protocol::{SenderKind, MAX_BODY};
use crate::store::valid_id;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use thiserror::Error;

const OUTPUT_LIMIT: usize = 1024 * 1024;
#[derive(Debug, Error)]
pub enum Error {
    #[error("core_cli_unavailable")]
    Unavailable,
    #[error("core_cli_timeout")]
    Timeout,
    #[error("core_cli_output_limit")]
    OutputLimit,
    #[error("core_cli_rejected")]
    Rejected,
    #[error("core_cli_invalid_response")]
    Invalid,
}

/// Deliberately a projection of Core AgentInfo, not an invented folder/profile API.
#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct AgentMetadata {
    pub agent_id: String,
    pub workspace_id: String,
    pub server_id: String,
    pub kind: String,
    pub cwd: String,
    pub status: String,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Deserialize)]
pub struct Principal {
    pub kind: SenderKind,
    pub id: String,
}
#[derive(Deserialize)]
pub struct Delivery {
    pub delivery_id: String,
    pub recipient: Principal,
    pub message: Message,
}
#[derive(Deserialize)]
pub struct Message {
    pub message_id: String,
    pub sender: Principal,
    pub body: String,
    #[serde(default)]
    pub context_ids: Vec<String>,
}

#[derive(Deserialize)]
pub struct ReceiveBatch {
    pub deliveries: Vec<Delivery>,
    pub remaining_unacknowledged: u64,
}

#[derive(Clone)]
pub struct Core {
    executable: PathBuf,
    own_id: String,
}

impl Core {
    pub(crate) fn is_identity(&self, id: &str) -> bool {
        self.own_id == id
    }
    pub fn new(executable: &Path, own_id: &str) -> Result<Self, Error> {
        if !executable.is_absolute() || !valid_id(own_id) {
            return Err(Error::Invalid);
        }
        Ok(Self {
            executable: executable.into(),
            own_id: own_id.into(),
        })
    }

    /// Stable IDs bypass CLI Tree enumeration. Never resolve a user-supplied path.
    pub fn agent_metadata(&self, id: &str) -> Result<AgentMetadata, Error> {
        if !(id.starts_with("ag_") || id.starts_with("app_")) || !valid_id(id) {
            return Err(Error::Invalid);
        }
        let result: AgentMetadata = serde_json::from_value(run(
            &self.executable,
            &["agent".into(), "show".into(), "--".into(), id.into()],
            None,
            Duration::from_secs(15),
        )?)
        .map_err(|_| Error::Invalid)?;
        if result.agent_id != id
            || !valid_id(&result.workspace_id)
            || !valid_id(&result.server_id)
            || result.cwd.len() > 4096
            || result.cwd.chars().any(char::is_control)
            || result.status.len() > 64
            || result.kind.len() > 64
            || result
                .path
                .as_ref()
                .is_some_and(|p| p.len() > 4096 || p.chars().any(char::is_control))
        {
            return Err(Error::Invalid);
        }
        Ok(result)
    }

    pub fn receive(&self, wait_ms: u32) -> Result<Vec<Delivery>, Error> {
        Ok(self.receive_batch(wait_ms)?.deliveries)
    }

    /// Exact backlog metadata from Core; the scheduler can drain a bounded number
    /// of zero-wait pages, yielding between ticks to maintenance and other work.
    pub fn receive_batch(&self, wait_ms: u32) -> Result<ReceiveBatch, Error> {
        if wait_ms > 1000 {
            return Err(Error::Invalid);
        }
        let result: ReceiveBatch = serde_json::from_value(run(
            &self.executable,
            &[
                "message".into(),
                "receive".into(),
                format!("--wait={wait_ms}"),
                "--limit=10".into(),
            ],
            None,
            Duration::from_secs(15),
        )?)
        .map_err(|_| Error::Invalid)?;
        if result.deliveries.len() > 10
            || result.deliveries.iter().any(|d| {
                d.recipient.kind != SenderKind::Agent
                    || d.recipient.id != self.own_id
                    || !valid_id(&d.delivery_id)
                    || !valid_id(&d.message.message_id)
                    || !valid_id(&d.message.sender.id)
                    || d.message.body.len() > MAX_BODY
                    || d.message.context_ids.len() > 32
                    || !d.message.context_ids.iter().all(|id| valid_id(id))
            })
        {
            return Err(Error::Invalid);
        }
        Ok(result)
    }

    /// Owner-recovery network primitive, not an owner authorization check.
    /// Expected message/endpoint/workspace are from a durable rejection receipt,
    /// never from arbitrary browser JSON. Core supplies recipient membership.
    pub fn get_bound_message(
        &self,
        message_id: &str,
        endpoint: &str,
        workspace: &str,
    ) -> Result<Message, Error> {
        if !valid_id(message_id)
            || !valid_id(endpoint)
            || !valid_id(workspace)
            || !(endpoint.starts_with("ag_") || endpoint.starts_with("app_"))
        {
            return Err(Error::Invalid);
        }
        #[derive(Deserialize)]
        struct BoundMessage {
            message_id: String,
            workspace_id: String,
            sender: Principal,
            recipients: Vec<Principal>,
            body: String,
            #[serde(default)]
            context_ids: Vec<String>,
        }
        #[derive(Deserialize)]
        struct Response {
            message: BoundMessage,
        }
        let result: Response = serde_json::from_value(run(
            &self.executable,
            &[
                "message".into(),
                "get".into(),
                "--".into(),
                message_id.into(),
            ],
            None,
            Duration::from_secs(15),
        )?)
        .map_err(|_| Error::Invalid)?;
        let m = result.message;
        if m.message_id != message_id
            || m.workspace_id != workspace
            || m.sender.kind != SenderKind::Agent
            || m.sender.id != endpoint
            || m.recipients.len() > 32
            || !m
                .recipients
                .iter()
                .any(|p| p.kind == SenderKind::Agent && p.id == self.own_id)
            || m.body.is_empty()
            || m.body.len() > MAX_BODY
            || m.context_ids.len() > 32
            || !m.context_ids.iter().all(|id| valid_id(id))
        {
            return Err(Error::Invalid);
        }
        Ok(Message {
            message_id: m.message_id,
            sender: m.sender,
            body: m.body,
            context_ids: m.context_ids,
        })
    }

    /// Context IDs are option=value arguments; body and external metadata are
    /// exclusively stdin, as the real CLI's envelope contract requires.
    pub fn send(
        &self,
        recipient: &str,
        key: &str,
        body: &str,
        contexts: &[String],
        external_source: Option<Value>,
    ) -> Result<String, Error> {
        if !valid_id(recipient)
            || !valid_id(key)
            || body.trim().is_empty()
            || body.len() > MAX_BODY
            || contexts.len() > 32
            || !contexts.iter().all(|id| valid_id(id))
        {
            return Err(Error::Invalid);
        }
        let mut arguments = vec![
            "message".into(),
            "send".into(),
            format!("--to={recipient}"),
            format!("--idempotency-key={key}"),
            "--stdin-envelope".into(),
        ];
        for context in contexts {
            arguments.push(format!("--context={context}"));
        }
        let envelope = serde_json::to_vec(&json!({"body":body,"external_source":external_source}))
            .map_err(|_| Error::Invalid)?;
        #[derive(Deserialize)]
        struct Response {
            message: Message,
        }
        let result: Response = serde_json::from_value(run(
            &self.executable,
            &arguments,
            Some(&envelope),
            Duration::from_secs(15),
        )?)
        .map_err(|_| Error::Invalid)?;
        if result.message.sender.kind != SenderKind::Agent
            || result.message.sender.id != self.own_id
            || !valid_id(&result.message.message_id)
            || result.message.body != body
        {
            return Err(Error::Invalid);
        }
        Ok(result.message.message_id)
    }

    pub fn ack(&self, delivery: &str) -> Result<(), Error> {
        if !valid_id(delivery) {
            return Err(Error::Invalid);
        }
        let key = crate::protocol::digest_id("ack", &self.own_id, 1, delivery);
        let value = run(
            &self.executable,
            &[
                "message".into(),
                "ack".into(),
                format!("--operation-id=ep-ack-{key}"),
                "--".into(),
                delivery.into(),
            ],
            None,
            Duration::from_secs(15),
        )?;
        #[derive(Deserialize)]
        struct Response {
            acknowledged_delivery_ids: Vec<String>,
            already_acknowledged_delivery_ids: Vec<String>,
        }
        let result: Response = serde_json::from_value(value).map_err(|_| Error::Invalid)?;
        if !result
            .acknowledged_delivery_ids
            .iter()
            .chain(&result.already_acknowledged_delivery_ids)
            .any(|id| id == delivery)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

fn nonblocking(fd: &impl std::os::fd::AsFd) -> Result<(), Error> {
    let flags = rustix::fs::fcntl_getfl(fd).map_err(|_| Error::Unavailable)?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)
        .map_err(|_| Error::Unavailable)
}

fn drain(
    stream: &mut impl Read,
    total: &mut usize,
    output: Option<&mut Vec<u8>>,
) -> Result<bool, Error> {
    let mut buffer = [0u8; 8192];
    match stream.read(&mut buffer) {
        Ok(0) => Ok(true),
        Ok(size) => {
            *total += size;
            if *total > OUTPUT_LIMIT {
                return Err(Error::OutputLimit);
            }
            if let Some(output) = output {
                output.extend_from_slice(&buffer[..size]);
            }
            Ok(false)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(_) => Err(Error::Unavailable),
    }
}

fn run(
    executable: &Path,
    arguments: &[String],
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Value, Error> {
    if input.is_some_and(|b| b.len() > 256 * 1024) {
        return Err(Error::Invalid);
    }
    let mut child = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| Error::Unavailable)?;
    let result = (|| {
        let stdin = child.stdin.take().ok_or(Error::Unavailable)?;
        let mut stdout = child.stdout.take().ok_or(Error::Unavailable)?;
        let mut stderr = child.stderr.take().ok_or(Error::Unavailable)?;
        nonblocking(&stdin)?;
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let mut input = input.unwrap_or_default();
        let mut stdin = if input.is_empty() {
            drop(stdin);
            None
        } else {
            Some(stdin)
        };
        let mut output = Vec::new();
        let mut total = 0;
        let mut out_done = false;
        let mut err_done = false;
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() >= deadline {
                return Err(Error::Timeout);
            }
            if let Some(writer) = stdin.as_mut() {
                match writer.write(input) {
                    Ok(0) => return Err(Error::Unavailable),
                    Ok(size) => input = &input[size..],
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return Err(Error::Unavailable),
                }
                if input.is_empty() {
                    stdin = None;
                }
            }
            if !out_done {
                out_done = drain(&mut stdout, &mut total, Some(&mut output))?;
            }
            if !err_done {
                err_done = drain(&mut stderr, &mut total, None)?;
            }
            if let Some(status) = child.try_wait().map_err(|_| Error::Unavailable)? {
                if out_done && err_done {
                    if !status.success() {
                        return Err(Error::Rejected);
                    }
                    return serde_json::from_slice(&output).map_err(|_| Error::Invalid);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    // Always reap only this exact child. No process-name/group kills or leaked
    // reader threads if an untrusted CLI response never closes a pipe.
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    fn fake(code: &str) -> (tempfile::TempDir, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let executable = temporary.path().join("fake-treer");
        fs::write(&executable, format!("#!/usr/bin/python3\n{code}\n")).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        (temporary, executable)
    }
    #[test]
    fn fixed_operations_match_real_cli_contract_and_keep_body_off_argv() {
        let apps = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let python_path = serde_json::to_string(&apps.to_string_lossy()).unwrap();
        let code = format!(
            r#"
import json,sys
from pathlib import Path
sys.path.insert(0,{python_path})
from cli_contract import violation
args=sys.argv[1:]
assert violation(args) is None
assert not any('sensitive' in a for a in args)
principal={{'kind':'agent','id':'app_own','name':'App display name','path':None,'role':'member'}}
if args[:2]==['message','send']:
 envelope=json.load(sys.stdin)
 assert set(envelope)=={{'body','external_source'}}
 assert envelope['body']=='sensitive body'
 assert '--to=ag_target' in args
 assert '--context=msg_context' in args
 result={{'message':{{'message_id':'msg_sent','sender':principal,'body':envelope['body'],'context_ids':[]}}}}
elif args[:2]==['message','receive']:
 result={{'remaining_unacknowledged':27,'deliveries':[{{'delivery_id':'del_received','recipient':principal,'message':{{'message_id':'msg_received','sender':principal,'body':'sensitive inbound','context_ids':[]}}}}]}}
elif args[:2]==['message','ack']:
 assert args[-2:]==['--','del_received']
 result={{'acknowledged_delivery_ids':['del_received'],'already_acknowledged_delivery_ids':[]}}
elif args[:2]==['agent','show']:
 assert args[2:]==['--','ag_target']
 result={{'agent_id':'ag_target','workspace_id':'ws_test','server_id':'machine_test','kind':'command','cwd':'/private/endpoint','status':'running','path':'/team/endpoint'}}
else: raise AssertionError('unexpected operation')
print(json.dumps(result))
"#
        );
        let (_temporary, executable) = fake(&code);
        let core = Core::new(&executable, "app_own").unwrap();
        assert_eq!(
            core.send(
                "ag_target",
                "stable-key",
                "sensitive body",
                &["msg_context".into()],
                None
            )
            .unwrap(),
            "msg_sent"
        );
        let batch = core.receive_batch(0).unwrap();
        assert_eq!(batch.remaining_unacknowledged, 27);
        let deliveries = batch.deliveries;
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].recipient.id, "app_own");
        core.ack("del_received").unwrap();
        assert_eq!(
            core.agent_metadata("ag_target").unwrap().path.as_deref(),
            Some("/team/endpoint")
        );
        assert!(core.agent_metadata("/team/endpoint").is_err());
    }
    #[test]
    fn bounded_process_kills_and_reaps_timeout_and_output_flood() {
        let (_temporary, executable) = fake("import time\ntime.sleep(10)");
        let started = Instant::now();
        assert!(matches!(
            run(&executable, &[], None, Duration::from_millis(50)),
            Err(Error::Timeout)
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        let (_temporary, executable) = fake("import sys\nsys.stderr.write('sensitive'*150000)");
        assert!(matches!(
            run(&executable, &[], None, Duration::from_secs(5)),
            Err(Error::OutputLimit)
        ));
        let (_temporary, executable) = fake("import sys\nprint('sensitive')\nsys.exit(2)");
        assert!(matches!(
            run(&executable, &[], None, Duration::from_secs(5)),
            Err(Error::Rejected)
        ));
    }
    #[test]
    fn refetch_get_uses_fixed_cli_and_verifies_message_workspace_sender_and_recipient() {
        let response = json!({"message":{"message_id":"msg_recover","workspace_id":"ws_test","sender":{"kind":"agent","id":"ag_endpoint","path":"/display/only"},"recipients":[{"kind":"agent","id":"app_own"}],"body":"synthetic recovered fragment","context_ids":[]}});
        let code = format!(
            "import sys,json\nassert sys.argv[1:]==['message','get','--','msg_recover']\nprint({})",
            serde_json::to_string(&response.to_string()).unwrap()
        );
        let (_tmp, executable) = fake(&code);
        let core = Core::new(&executable, "app_own").unwrap();
        assert_eq!(
            core.get_bound_message("msg_recover", "ag_endpoint", "ws_test")
                .unwrap()
                .body,
            "synthetic recovered fragment"
        );
        for (field, value) in [
            ("message_id", json!("msg_other")),
            ("workspace_id", json!("ws_other")),
            ("sender", json!({"kind":"human","id":"ag_endpoint"})),
            ("sender", json!({"kind":"agent","id":"ag_other"})),
            ("recipients", json!([{"kind":"human","id":"app_own"}])),
            ("recipients", json!([{"kind":"agent","id":"app_other"}])),
            ("body", json!("x".repeat(MAX_BODY + 1))),
        ] {
            let mut invalid = response.clone();
            invalid["message"][field] = value;
            let (_tmp, executable) = fake(&format!(
                "print({})",
                serde_json::to_string(&invalid.to_string()).unwrap()
            ));
            assert!(matches!(
                Core::new(&executable, "app_own")
                    .unwrap()
                    .get_bound_message("msg_recover", "ag_endpoint", "ws_test"),
                Err(Error::Invalid)
            ));
        }
    }
    #[test]
    fn receive_batch_refuses_missing_negative_or_malformed_backlog_metadata() {
        for response in [
            json!({"deliveries":[]}),
            json!({"deliveries":[],"remaining_unacknowledged":-1}),
            json!({"deliveries":[],"remaining_unacknowledged":"1"}),
        ] {
            let (_tmp, executable) = fake(&format!(
                "print({})",
                serde_json::to_string(&response.to_string()).unwrap()
            ));
            assert!(matches!(
                Core::new(&executable, "app_own").unwrap().receive_batch(0),
                Err(Error::Invalid)
            ));
        }
    }
    #[test]
    fn malformed_and_wrong_identity_output_fail_closed() {
        for output in [
            "not-json",
            r#"{"deliveries":[{"delivery_id":"d","recipient":{"kind":"agent","id":"app_other"},"message":{"message_id":"m","sender":{"kind":"agent","id":"app_own"},"body":"body"}}]}"#,
        ] {
            let code = format!("print({})", serde_json::to_string(output).unwrap());
            let (_temporary, executable) = fake(&code);
            assert!(matches!(
                Core::new(&executable, "app_own").unwrap().receive(0),
                Err(Error::Invalid)
            ));
        }
        let (_temporary, executable) = fake(
            r#"print('{"message":{"message_id":"msg","sender":{"kind":"human","id":"app_own"},"body":"body"}}')"#,
        );
        assert!(matches!(
            Core::new(&executable, "app_own")
                .unwrap()
                .send("ag_target", "key", "body", &[], None),
            Err(Error::Invalid)
        ));
    }
    #[test]
    fn large_stdin_never_deadlocks_and_output_errors_are_fixed() {
        let (_temporary, executable) =
            fake("import sys,json\ns=sys.stdin.read()\nprint(json.dumps({'size':len(s)}))");
        let data = vec![b'x'; 200_000];
        let value = run(&executable, &[], Some(&data), Duration::from_secs(5)).unwrap();
        assert_eq!(value["size"], 200_000);
        assert!(matches!(
            run(
                Path::new("/nonexistent/treer"),
                &[],
                None,
                Duration::from_millis(10)
            ),
            Err(Error::Unavailable)
        ));
        for error in [
            Error::Unavailable,
            Error::Timeout,
            Error::OutputLimit,
            Error::Rejected,
            Error::Invalid,
        ] {
            assert!(error.to_string().starts_with("core_cli_"));
        }
    }
}
