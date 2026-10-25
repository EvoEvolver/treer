//! Exercise the actual production executable's pre-runtime private entry.
//! These refusal cases never create a job, worker, runtime, or service.
#![cfg(unix)]

use std::{os::unix::fs::PermissionsExt, process::Command};

#[test]
fn self_update_private_entry_refuses_missing_foreign_or_extra_arguments() {
    let root = std::env::temp_dir().join(format!("treer-update-entry-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let foreign = uuid::Uuid::new_v4().to_string();
    for arguments in [
        vec![],
        vec!["../../foreign"],
        vec![foreign.as_str()],
        vec![foreign.as_str(), "--execute-cli"],
        vec![foreign.as_str(), "metrics-gpu-probe"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_treer-agent-server"))
            .arg("--controller-update-worker")
            .args(arguments)
            .env_clear()
            .env("HOME", &root)
            .env("XDG_CONFIG_HOME", &root)
            .env("TREER_STATE_DIR", &root)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap().trim(),
            "Error: update_worker_bootstrap_failed"
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }
    std::fs::remove_dir(root).unwrap();
}
