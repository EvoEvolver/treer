use super::*;
use std::{
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "treer-worker-lock-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        crate::secure_directory(&path).unwrap();
        for name in ["treer-agent-server.exe", "treer.exe"] {
            crate::windows::create_private_update_file(&path.join(name))
                .unwrap()
                .write_all(b"inert fixture, never executed")
                .unwrap();
        }
        Self(path)
    }
    fn installation(&self) -> LockedInstallation {
        PinnedPair::open(&self.0)
            .unwrap()
            .lock_installation()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct OwnedFixture(Child);
impl OwnedFixture {
    async fn wait_success(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    assert!(status.success(), "owned fixture failed: {status}");
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned fixture exit deadline");
    }
}
impl Drop for OwnedFixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn connect_fixture(
    installation: &LockedInstallation,
    mode: &str,
) -> (
    OwnedFixture,
    tokio::net::windows::named_pipe::NamedPipeServer,
) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let selector = format!(
        "{:016x}{:016x}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let pipe = crate::windows_update_pipe::CreatedChildPipe::bind(&selector).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "windows_update_inputs::tests::inherited_lock_fixture_child",
            "--ignored",
            "--nocapture",
        ])
        .env_clear()
        .env("TREER_TEST_LOCK_SELECTOR", selector)
        .env("TREER_TEST_LOCK_MODE", mode)
        .stdin(crate::windows_update_pipe::creator_process_stdio().unwrap())
        .stdout(Stdio::null())
        .stderr(if mode == "null" {
            Stdio::null()
        } else {
            installation.worker_lock_stdio().unwrap()
        });
    let child = OwnedFixture(command.spawn().unwrap());
    // Drop Command's prepared Stdio: only the original lease and child should
    // retain the no-sharing file object during the continuity assertion.
    drop(command);
    let mut stream = pipe.connect(&child.0).await.unwrap();
    let (volume, mut id) = installation.lock_file_identity().unwrap();
    if mode == "wrong" {
        id ^= 1;
    }
    stream.write_all(&volume.to_le_bytes()).await.unwrap();
    stream.write_all(&id.to_le_bytes()).await.unwrap();
    (child, stream)
}

#[tokio::test]
async fn windows_update_worker_lock_child_keeps_same_lock_after_parent_drop() {
    let root = Fixture::new();
    let installation = root.installation();
    let (mut child, mut stream) = connect_fixture(&installation, "hold").await;
    tokio::time::timeout(Duration::from_secs(3), async {
        assert_eq!(stream.read_u8().await.unwrap(), 1);
        drop(installation);
        assert!(PinnedPair::open(&root.0)
            .unwrap()
            .lock_installation()
            .is_err());
        stream.write_all(&[2]).await.unwrap();
        // Even after dropping the retained duplicate the inherited standard
        // handle holds the SAME lock until child exit. No premature retirement.
        assert_eq!(stream.read_u8().await.unwrap(), 3);
        assert!(PinnedPair::open(&root.0)
            .unwrap()
            .lock_installation()
            .is_err());
        stream.write_all(&[4]).await.unwrap();
    })
    .await
    .unwrap();
    child.wait_success().await;
    // Fixture has NO claim/intent: this verifies kernel lifetime, not permission
    // to replay an updater after a real operation's creator or worker exits.
    drop(root.installation());
}

#[tokio::test]
async fn windows_update_worker_lock_wrong_identity_and_nonfile_refuse() {
    let root = Fixture::new();
    let installation = root.installation();
    for mode in ["wrong", "null"] {
        let (mut child, mut stream) = connect_fixture(&installation, mode).await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), stream.read_u8())
                .await
                .unwrap()
                .unwrap(),
            0
        );
        child.wait_success().await;
        assert!(PinnedPair::open(&root.0)
            .unwrap()
            .lock_installation()
            .is_err());
    }
}

#[test]
#[ignore = "explicit task-owned child helper, not a selector entry"]
fn inherited_lock_fixture_child() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(3), async {
            let selector = std::env::var("TREER_TEST_LOCK_SELECTOR").unwrap();
            let mode = std::env::var("TREER_TEST_LOCK_MODE").unwrap();
            let mut creator =
                crate::windows_update_pipe::connect_creation_parent_pipe(&selector).unwrap();
            let mut bytes = [0; 16];
            creator.read_exact(&mut bytes).await.unwrap();
            let expected = (
                u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                u64::from_le_bytes(bytes[8..].try_into().unwrap()),
            );
            let retained = InheritedInstallationLock::retain_from_creator(&creator, expected);
            if mode != "hold" {
                assert!(retained.is_err());
                creator.write_all(&[0]).await.unwrap();
                return;
            }
            let retained = retained.unwrap();
            assert_eq!(lock_identity(&retained._file).unwrap(), expected);
            assert!((&retained._file).write_all(b"must refuse").is_err());
            creator.write_all(&[1]).await.unwrap();
            assert_eq!(creator.read_u8().await.unwrap(), 2);
            drop(retained);
            creator.write_all(&[3]).await.unwrap();
            assert_eq!(creator.read_u8().await.unwrap(), 4);
        })
        .await
        .unwrap();
    });
}
