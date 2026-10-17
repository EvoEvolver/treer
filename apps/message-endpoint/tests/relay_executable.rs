#![cfg(unix)]
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct OwnChild(Child);
impl Drop for OwnChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn real_relay_binary_uses_only_fake_contract_checked_cli_and_stays_running() {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path().canonicalize().unwrap();
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
    let fake = base.join("fake-treer");
    let apps = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let import_path = serde_json::to_string(&apps.to_string_lossy()).unwrap();
    let code = format!(
        r#"#!/usr/bin/python3
import json,os,sys,time
from pathlib import Path
sys.path.insert(0,{import_path})
from cli_contract import violation
args=sys.argv[1:]
assert violation(args) is None
assert not any('synthetic' in arg for arg in args)
state_file=Path(__file__).with_name('fake-state.json')
state=json.loads(state_file.read_text()) if state_file.exists() else {{'acked':False,'startup':False,'forwarded':False}}
principal={{'kind':'agent','id':'ag_endpoint','name':'Test endpoint','path':'/test/endpoint'}}
if args[:2]==['message','send']:
 envelope=json.load(sys.stdin)
 body=json.loads(envelope['body'])
 assert body['protocol'] in ['treer.endpoint.status/v1','treer.endpoint/v1']
 if body['protocol']=='treer.endpoint.status/v1':
  state['startup']=True
  assert len(body['actual_sha256'])==64
 else:
  state['forwarded']=True
  assert body['direction']=='outbound'
 assert '--to=app_hub' in args
 result={{'message':{{'message_id':'msg_forwarded','sender':principal,'body':envelope['body']}}}}
elif args[:2]==['message','receive']:
 time.sleep(0.02)
 result={{'deliveries':[] if state['acked'] else [{{'delivery_id':'del_user','recipient':principal,
  'message':{{'message_id':'msg_user','sender':{{'kind':'agent','id':'ag_sender','name':'Sender'}},'body':'synthetic only','context_ids':[]}}}}]}}
elif args[:2]==['message','ack']:
 assert args[-2:]==['--','del_user']
 assert state['forwarded']
 state['acked']=True
 result={{'acknowledged_delivery_ids':['del_user'],'already_acknowledged_delivery_ids':[]}}
else: raise AssertionError('unexpected command')
updated=state_file.with_suffix('.new')
updated.write_text(json.dumps(state))
os.replace(updated,state_file)
print(json.dumps(result))
"#
    );
    fs::write(&fake, code).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    let config = base.join("endpoint.json");
    fs::write(&config,json!({"endpoint_id":"ag_endpoint","hub_id":"app_hub","generation":1,
        "recipients":["ag_target"],"cli":fake,"state_directory":base.join("state"),"release_sha256":"a".repeat(64)}).to_string()).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_treer-endpoint"))
        .env_clear()
        .env("HOME", &base)
        .env("TMPDIR", &base)
        .env("PATH", "/usr/bin:/bin")
        .env("TREER_AGENT_ID", "ag_endpoint")
        .current_dir(&base)
        .arg("--config")
        .arg(&config)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut child = OwnChild(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "relay exited unexpectedly"
        );
        if let Ok(bytes) = fs::read(base.join("fake-state.json")) {
            let state: Value = serde_json::from_slice(&bytes).unwrap();
            if state["acked"] == true {
                assert_eq!(state["startup"], true);
                assert_eq!(state["forwarded"], true);
                break;
            }
        }
        assert!(Instant::now() < deadline, "synthetic relay did not ack");
        std::thread::sleep(Duration::from_millis(10));
    }
    // No normal exit/completion after draining; only our own test process is stopped.
    assert!(child.0.try_wait().unwrap().is_none());
}

#[test]
fn invalid_startup_exits_nonzero_with_fixed_error_only() {
    let result = Command::new(env!("CARGO_BIN_EXE_treer-endpoint"))
        .env_clear()
        .arg("--invalid")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert_eq!(
        String::from_utf8(result.stderr).unwrap(),
        "relay_configuration_unavailable\n"
    );
}
