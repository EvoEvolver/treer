use super::*;
use axum::{
    body::Body,
    http::{Request, Response},
    routing::any,
    Router,
};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const VERSION: &str = "v0.1.13";
const PLATFORM: &str = "darwin-aarch64";
const CHANNEL: &str = "channels/stable.json";
const MANIFEST: &str = "releases/v0.1.13/manifest.json";
const CONTROLLER: &str = "releases/v0.1.13/darwin-aarch64/treer-agent-server";
const CLI: &str = "releases/v0.1.13/darwin-aarch64/treer";
const BINARY: &[u8] = b"a verified binary";

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

fn signature(bytes: &[u8]) -> Vec<u8> {
    let key = signing_key();
    serde_json::to_vec(&json!({
        "schema_version": 1, "algorithm": "ed25519",
        "key_id": key_id(&key.verifying_key()).unwrap(),
        "signature": STANDARD.encode(key.sign(bytes).to_bytes()),
    }))
    .unwrap()
}

type Objects = HashMap<String, Vec<u8>>;
fn put_signed(objects: &mut Objects, path: &str, bytes: Vec<u8>) {
    objects.insert(format!("{path}.sig"), signature(&bytes));
    objects.insert(path.into(), bytes);
}

fn fixture() -> Objects {
    let mut objects = Objects::new();
    let manifest = json!({
        "schema_version": 2, "version": VERSION,
        "signing_key_id": key_id(&signing_key().verifying_key()).unwrap(),
        "git_commit": "test-commit", "builds": [],
        "artifacts": [
            { "binary": "treer-agent-server", "platform": PLATFORM, "path": CONTROLLER, "size": BINARY.len(), "sha256": digest(BINARY) },
            { "binary": "treer", "platform": PLATFORM, "path": CLI, "size": BINARY.len(), "sha256": digest(BINARY) },
        ],
    });
    put_signed(
        &mut objects,
        MANIFEST,
        serde_json::to_vec(&manifest).unwrap(),
    );
    resign_channel(&mut objects);
    objects.insert(CONTROLLER.into(), BINARY.to_vec());
    objects.insert(CLI.into(), BINARY.to_vec());
    objects
}

fn resign_channel(objects: &mut Objects) {
    put_signed(
        objects,
        CHANNEL,
        serde_json::to_vec(&json!({
            "schema_version": 1, "channel": "stable", "version": VERSION,
            "manifest_path": MANIFEST, "manifest_sha256": digest(&objects[MANIFEST]),
        }))
        .unwrap(),
    );
}

fn change_manifest(objects: &mut Objects, change: impl FnOnce(&mut Value)) {
    let mut manifest: Value = serde_json::from_slice(&objects[MANIFEST]).unwrap();
    change(&mut manifest);
    put_signed(objects, MANIFEST, serde_json::to_vec(&manifest).unwrap());
    resign_channel(objects);
}

async fn server(objects: Objects) -> (Url, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let objects = Arc::new(objects);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let app = Router::new().route(
        "/{*path}",
        any(move |request: Request<Body>| {
            let objects = objects.clone();
            let observed = observed.clone();
            async move {
                let path = request.uri().path().trim_start_matches('/');
                observed.lock().unwrap().push(path.to_owned());
                match objects.get(path) {
                    Some(bytes) => Response::builder().body(Body::from(bytes.clone())).unwrap(),
                    None => Response::builder().status(404).body(Body::empty()).unwrap(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, requests, task)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
}

#[test]
fn release_trust_resolution_preserves_explicit_and_existing_installation_keys() {
    use ed25519_dalek::pkcs8::EncodePublicKey;
    let directory =
        std::env::temp_dir().join(format!("treer-release-trust-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let local = directory.join("release-signing-key.pub.pem");
    let explicit = directory.join("explicit.pub.pem");
    let official = Url::parse("https://releases.treer.ai/").unwrap();
    let custom = Url::parse("https://self-hosted.example/releases/").unwrap();
    let bundled = signing_key()
        .verifying_key()
        .to_public_key_pem(Default::default())
        .unwrap();
    let other = SigningKey::from_bytes(&[8; 32]).verifying_key();
    let other_pem = other.to_public_key_pem(Default::default()).unwrap();

    assert_eq!(
        resolve_public_key(&official, None, &local, Some(&bundled)).unwrap(),
        signing_key().verifying_key()
    );
    assert!(resolve_public_key(&custom, None, &local, Some(&bundled)).is_err());
    assert!(resolve_public_key(
        &Url::parse("https://releases.treer.ai/another-publisher/").unwrap(),
        None,
        &local,
        Some(&bundled),
    )
    .is_err());
    assert!(resolve_public_key(&official, None, &local, None).is_err());
    assert!(resolve_public_key(&official, None, &local, Some("invalid bundled key")).is_err());
    assert!(resolve_public_key(&official, Some(&explicit), &local, Some(&bundled)).is_err());

    std::fs::write(&local, &other_pem).unwrap();
    assert_eq!(
        resolve_public_key(&official, None, &local, Some(&bundled)).unwrap(),
        other
    );
    assert_eq!(
        resolve_public_key(&custom, None, &local, Some(&bundled)).unwrap(),
        other
    );
    std::fs::write(&explicit, &bundled).unwrap();
    assert_eq!(
        resolve_public_key(&custom, Some(&explicit), &local, None).unwrap(),
        signing_key().verifying_key()
    );
    std::fs::write(&local, "invalid local key").unwrap();
    assert!(resolve_public_key(&official, None, &local, Some(&bundled)).is_err());
    std::fs::write(&explicit, "invalid explicit key").unwrap();
    assert!(resolve_public_key(&official, Some(&explicit), &local, Some(&bundled)).is_err());

    // The bundled key does not confer trust on another publisher's signatures.
    let selected =
        resolve_public_key(&official, None, &directory.join("absent"), Some(&other_pem)).unwrap();
    assert!(verify_signed(b"channel", &signature(b"channel"), &selected).is_err());
    std::fs::remove_file(local).unwrap();
    std::fs::remove_file(explicit).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn release_trust_resolution_does_not_ignore_broken_local_key_links() {
    let directory =
        std::env::temp_dir().join(format!("treer-release-trust-link-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let local = directory.join("key.pub.pem");
    std::os::unix::fs::symlink(directory.join("absent.pem"), &local).unwrap();
    let official = Url::parse("https://releases.treer.ai/").unwrap();
    assert!(resolve_public_key(&official, None, &local, None)
        .unwrap_err()
        .to_string()
        .starts_with("cannot read trusted release public key"));
    std::fs::remove_file(local).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn release_verifier_accepts_node_crypto_publisher_wire_format() {
    // Generated with Node crypto from the public test seed [7; 32], using the
    // publisher's SPKI key ID and detached signature format (not a release key).
    let key = VerifyingKey::from_public_key_pem("-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA6kpsY+KcUgq+9VB7Ey7F+ZVHdq6+vnuSQh7qaRRG0iw=\n-----END PUBLIC KEY-----\n").unwrap();
    let bytes = b"{\"schema_version\":1,\"channel\":\"stable\"}\n";
    let detached = serde_json::to_vec(&json!({
        "schema_version": 1, "algorithm": "ed25519", "key_id": "324be2dea8bc4446",
        "signature": "MKkMahEu8T2nbto7BHFtlEeOUaLz0aPEuWneBP2TEcB4PpRzf3ARYNrwd/4Xq7/B7U+/5iARdbJmDGOvMVv+Cw==",
    })).unwrap();
    verify_signed(bytes, &detached, &key).unwrap();
    assert!(verify_signed(b"tampered", &detached, &key).is_err());
    assert!(verify_signed(
        bytes,
        &detached,
        &SigningKey::from_bytes(&[8; 32]).verifying_key()
    )
    .is_err());
}

#[tokio::test]
async fn signed_release_download_verifies_both_binaries() {
    let (base, requests, task) = server(fixture()).await;
    let (controller, cli) = download(
        &client(),
        base,
        "stable",
        &signing_key().verifying_key(),
        PLATFORM,
    )
    .await
    .unwrap();
    assert_eq!(controller.bytes(), BINARY);
    assert_eq!(cli.bytes(), BINARY);
    assert!(requests.lock().unwrap().contains(&CONTROLLER.to_owned()));
    task.abort();
}

#[tokio::test]
async fn signed_release_rejects_bad_metadata_before_fetching_executables() {
    for case in [
        "channel tamper",
        "missing signature",
        "forged signature",
        "manifest tamper",
        "manifest hash",
        "channel binding",
        "version path",
        "artifact path",
        "duplicate",
        "missing binary",
        "oversize",
        "metadata limit",
    ] {
        let mut objects = fixture();
        match case {
            "channel tamper" => {
                objects.get_mut(CHANNEL).unwrap().push(b' ');
            }
            "missing signature" => {
                objects.remove(&format!("{CHANNEL}.sig"));
            }
            "forged signature" => {
                let mut detached: Value =
                    serde_json::from_slice(&objects[&format!("{CHANNEL}.sig")]).unwrap();
                detached["signature"] = json!(STANDARD.encode(
                    SigningKey::from_bytes(&[8; 32])
                        .sign(&objects[CHANNEL])
                        .to_bytes()
                ));
                objects.insert(
                    format!("{CHANNEL}.sig"),
                    serde_json::to_vec(&detached).unwrap(),
                );
            }
            "manifest tamper" => {
                objects.get_mut(MANIFEST).unwrap().push(b' ');
            }
            "manifest hash" => {
                let bytes = objects[MANIFEST].clone();
                put_signed(&mut objects, MANIFEST, [bytes, vec![b' ']].concat());
            }
            "channel binding" | "version path" => {
                let mut channel: Value = serde_json::from_slice(&objects[CHANNEL]).unwrap();
                if case == "channel binding" {
                    channel["channel"] = json!("canary");
                } else {
                    channel["manifest_path"] = json!("https://untrusted.example/manifest.json");
                }
                put_signed(&mut objects, CHANNEL, serde_json::to_vec(&channel).unwrap());
            }
            "artifact path" => change_manifest(&mut objects, |m| {
                m["artifacts"][0]["path"] = json!("../evil")
            }),
            "duplicate" => change_manifest(&mut objects, |m| {
                let duplicate = m["artifacts"][0].clone();
                m["artifacts"].as_array_mut().unwrap().push(duplicate);
            }),
            "missing binary" => change_manifest(&mut objects, |m| {
                m["artifacts"].as_array_mut().unwrap().pop();
            }),
            "oversize" => change_manifest(&mut objects, |m| {
                m["artifacts"][0]["size"] = json!(MAX_BINARY_BYTES + 1)
            }),
            "metadata limit" => {
                objects.insert(CHANNEL.into(), vec![b' '; MAX_METADATA_BYTES + 1]);
            }
            _ => unreachable!(),
        }
        let (base, requests, task) = server(objects).await;
        let result = download(
            &client(),
            base,
            "stable",
            &signing_key().verifying_key(),
            PLATFORM,
        )
        .await;
        assert!(result.is_err(), "accepted {case}");
        assert!(
            !requests
                .lock()
                .unwrap()
                .iter()
                .any(|path| path == CONTROLLER || path == CLI),
            "fetched executable before rejecting {case}"
        );
        task.abort();
    }
}

#[tokio::test]
async fn signed_release_rejects_corrupt_or_wrong_size_binary() {
    for bytes in [
        vec![b'x'; BINARY.len()],
        vec![b'x'; BINARY.len() - 1],
        vec![b'x'; BINARY.len() + 1],
    ] {
        let mut objects = fixture();
        objects.insert(CLI.into(), bytes);
        let (base, _, task) = server(objects).await;
        assert!(download(
            &client(),
            base,
            "stable",
            &signing_key().verifying_key(),
            PLATFORM
        )
        .await
        .is_err());
        task.abort();
    }
}

#[test]
fn update_rejects_channel_path_injection_and_non_ed25519_signatures() {
    for channel in ["", "../stable", "Stable", "stable?x=1", "https://evil/"] {
        assert!(validate_channel_name(channel).is_err());
    }
    let key = signing_key().verifying_key();
    for value in [
        json!({}),
        json!({"schema_version": 1, "algorithm": "ed25519", "key_id": key_id(&key).unwrap(), "signature": "not-base64"}),
    ] {
        assert!(verify_signed(b"test", &serde_json::to_vec(&value).unwrap(), &key).is_err());
    }
}
