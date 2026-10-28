//! Explicit offline same-user operator commands; never HTTP owner impersonation.
use super::{controller::Controller, Config};
use crate::{Error, Limits, Owner, Store};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    config_sha256: String,
    expected_generation: String,
    confirmed: bool,
    operation: Operation,
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Enable,
    CreateTestToken { output: PathBuf },
    RevokeTestToken { id: String },
}
const ACTOR: &str = "deployment_operator";
const TEST_NAME: &str = "deployment-test-15min";
/// Caller must validate the same managed environment as ordinary runtime startup.
pub fn run(
    config: Config,
    config_digest: &str,
    credential: String,
    path: &Path,
) -> Result<Value, Error> {
    let parent = path.parent().ok_or(Error::Invalid)?;
    let directory = crate::private_store::open_directory(parent, false)?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(|_| Error::Invalid)?;
    let m = file.metadata().map_err(|_| Error::Invalid)?;
    if !m.is_file()
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o077 != 0
        || m.nlink() != 1
        || m.len() > 4096
    {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Invalid)?;
    if bytes.len() > 4096 {
        return Err(Error::Invalid);
    }
    let request: Request = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
    if !request.confirmed || request.config_sha256 != config_digest {
        return Err(Error::Denied);
    }
    let binding = config.binding(&credential)?;
    Controller::new(config.clone(), credential)?.preflight()?;
    let mut store = Store::open(&config.state, binding.clone(), Limits::default())?;
    let owner = Owner {
        binding: &binding,
        subject: ACTOR,
    };
    if store.state(&owner)?["generation"] != request.expected_generation {
        return Err(Error::Denied);
    }
    // Exclusive intent prevents accidental startup/retry replay, including ambiguous failures.
    let mut intent = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path.with_extension("consumed"))
        .map_err(|_| Error::Denied)?;
    writeln!(intent, "{:x}", Sha256::digest(&bytes)).map_err(|_| Error::PrivateStorage)?;
    intent.sync_all().map_err(|_| Error::PrivateStorage)?;
    rustix::fs::fsync(&directory).map_err(|_| Error::PrivateStorage)?;
    match request.operation {
        Operation::Enable => {
            store.policy_reviewed(&owner, true)?;
            Ok(json!({"actor":ACTOR,"enabled":true,"generation":request.expected_generation}))
        }
        Operation::CreateTestToken { output } => {
            let parent = output.parent().ok_or(Error::Invalid)?;
            let dir = crate::private_store::open_directory(parent, false)?;
            let leaf = output.file_name().ok_or(Error::Invalid)?;
            let fd = rustix::fs::openat(
                &dir,
                leaf,
                rustix::fs::OFlags::WRONLY
                    | rustix::fs::OFlags::CREATE
                    | rustix::fs::OFlags::EXCL
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::from_raw_mode(0o600),
            )
            .map_err(|_| Error::PrivateStorage)?;
            let mut output = std::fs::File::from(fd);
            let created = store.create_with_ttl(&owner, TEST_NAME, 900)?;
            let id = created.record.id.clone();
            if output
                .write_all(created.into_plaintext().as_bytes())
                .and_then(|_| output.sync_all())
                .is_err()
            {
                store.revoke(&owner, &id)?;
                return Err(Error::PrivateStorage);
            }
            rustix::fs::fsync(&dir).map_err(|_| Error::PrivateStorage)?;
            Ok(json!({"actor":ACTOR,"test_token_id":id,"expires_in_seconds":900}))
        }
        Operation::RevokeTestToken { id } => {
            if !store
                .list(&owner)?
                .iter()
                .any(|r| r.id == id && r.name == TEST_NAME)
            {
                return Err(Error::Denied);
            }
            let creator: String = store.db_creator(&id)?;
            if creator != ACTOR {
                return Err(Error::Denied);
            }
            store.revoke(&owner, &id)?;
            Ok(json!({"actor":ACTOR,"revoked_test_token":id}))
        }
    }
}
