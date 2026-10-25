//! Evidence about inherited endpoints, not a supervisor bootstrap or authority.
use super::*;
use std::{
    io::{Read, Write},
    os::fd::{AsFd, AsRawFd, OwnedFd},
    process::{Child, Command, Stdio},
    time::Duration,
};

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "isolated inherited socketpair endpoint, invoked by the outer test"]
fn inherited_endpoint_child() {
    assert_eq!(std::env::var("TREER_TEST_INHERITED_ENDPOINT").unwrap(), "1");
    let stdin = std::io::stdin();
    // The standard Command stdio adapter inherited exactly this endpoint as
    // fd 0. Restore CLOEXEC there, then clone atomically with CLOEXEC. Neither
    // the fd number nor the test environment is an authentication capability.
    nix::fcntl::fcntl(
        stdin.as_raw_fd(),
        nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
    )
    .unwrap();
    let mut endpoint = UnixStream::from(stdin.as_fd().try_clone_to_owned().unwrap());
    require_cloexec(&endpoint).unwrap();
    endpoint
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    treer_platform::process_image::enter_private_session().unwrap();
    let mut start = [0];
    endpoint.read_exact(&mut start).unwrap();
    assert_eq!(start, [b'R']);
    let observed = peer(&endpoint).ok();
    let (pid, uid) = observed.unwrap_or((0, 0));
    // Only an observation transcript for this test. The parent independently
    // obtains the actual child PID from its owned Child, never from this packet.
    let mut transcript = Vec::new();
    transcript.extend_from_slice(&std::process::id().to_be_bytes());
    transcript.extend_from_slice(&pid.to_be_bytes());
    transcript.extend_from_slice(&uid.to_be_bytes());
    endpoint.write_all(&transcript).unwrap();
    endpoint.read_exact(&mut start).unwrap();
    assert_eq!(start, [b'X']);
}

#[test]
fn inherited_socketpair_credentials_are_checked_against_the_owned_exec_child() {
    let (mut owner, child_end) = UnixStream::pair().unwrap();
    require_cloexec(&owner).unwrap();
    require_cloexec(&child_end).unwrap();
    let before = peer(&owner).ok();
    let child_before = peer(&child_end).ok();
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "self_update::handoff::socketpair_tests::inherited_endpoint_child",
                "--ignored",
                "--nocapture",
            ])
            .env_clear()
            .env("TREER_TEST_INHERITED_ENDPOINT", "1")
            .stdin(Stdio::from(OwnedFd::from(child_end)))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    owner
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    owner.write_all(b"R").unwrap();
    let mut transcript = [0; 12];
    owner.read_exact(&mut transcript).unwrap();
    let field = |index: usize| {
        let offset = index * 4;
        u32::from_be_bytes(transcript[offset..offset + 4].try_into().unwrap())
    };
    assert_eq!(field(0), child.0.id());
    assert_ne!(child.0.id(), std::process::id());
    assert!(child.0.try_wait().unwrap().is_none());
    let child_image = ProcessImage::capture(child.0.id()).unwrap();
    let parent_image = ProcessImage::capture(std::process::id()).unwrap();
    child_image.verify_parent(&parent_image).unwrap();
    child_image.verify_private_session().unwrap();
    let after = peer(&owner).ok();
    let child_after = (field(1) != 0).then(|| (field(1), field(2)));
    eprintln!(
        "inherited socketpair: creator={}, owned_child={}, owner_before={before:?}, owner_after={after:?}, child_before={child_before:?}, child_after={child_after:?}",
        std::process::id(), child.0.id(),
    );
    // Pin the observed kernel behavior separately on each platform. These
    // assertions concern this inherited socketpair, not named-socket connects.
    #[cfg(target_os = "linux")]
    {
        let creator = Some((std::process::id(), nix::unistd::geteuid().as_raw()));
        assert_eq!(before, creator);
        assert_eq!(after, creator);
        assert_eq!(child_before, creator);
        assert_eq!(child_after, creator);
    }
    #[cfg(target_os = "macos")]
    {
        let creator = Some((std::process::id(), nix::unistd::geteuid().as_raw()));
        assert_eq!(before, creator);
        assert_eq!(child_before, creator);
        assert_eq!(after, Some((child.0.id(), nix::unistd::geteuid().as_raw())));
        assert_eq!(child_after, creator);
    }
    let digest = digest_file(child_image.executable()).unwrap();
    let result = AuthenticatedPeer::authenticate(
        owner.try_clone().unwrap(),
        &child_image,
        child_image.executable(),
        &digest,
    );
    #[cfg(target_os = "macos")]
    result.unwrap().verify().unwrap();
    #[cfg(target_os = "linux")]
    assert_eq!(
        result.err().unwrap().to_string(),
        "update_handoff_peer_mismatch"
    );
    // A UID match, correct executable or child-reported PID cannot repair
    // unavailable/wrong OS peer identity in the existing authenticated wrapper.
    owner.write_all(b"X").unwrap();
    assert!(child.0.wait().unwrap().success());
}
