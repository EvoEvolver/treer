//! Bounded raw permit framing, NOT authenticated EOF or execution authority.
//! The future authenticated transport must exclusively own this receiver and
//! attest its peer/attempt/writers. Reading EOF alone does not prove owner death
//! or that every writer copy is gone. No connection, launch or receipt mutation.
use std::{
    io::{self, Read},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

use anyhow::{bail, Result};
use uuid::Uuid;

use super::Binding;

const HEADER: &[u8; 8] = b"TRLP\x01\0\0\0";
const FRAME: usize = 8 + 16 + 64 + 64;
const BUDGET: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Permit,
    EmptyEof,
    PartialEof,
}

/// A raw observation bound by an exclusive borrow to the still-open receiver.
/// Private fields prevent constructing this wrapper outside the actual reader;
/// its Kind and Binding remain ordinary data, never an authenticated proof.
/// Dropping it releases the borrow but does not close the receiver or authorize
/// noexec persistence. A half-close can yield EOF while the owner is alive.
pub struct Observation<'a> {
    _receiver: &'a mut UnixStream,
    kind: Kind,
    binding: Option<Binding>,
}

impl Observation<'_> {
    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn binding(&self) -> Option<&Binding> {
        self.binding.as_ref()
    }
}

/// Read at most one fixed frame plus an overflow-detection byte, requiring EOF
/// before returning even a complete permit. Timeout bounds the whole read, not
/// each chunk. Owner-side future transport must half-close writes before ACK.
/// Errors leave the receiver open and never synthesize an empty-EOF observation.
/// Leaves the private receiver nonblocking; callers must not retry ambiguous
/// input on this receiver.
/// Production integration must authenticate this exact accepted connection
/// before calling read: frame contents are public metadata, not a MAC. It must
/// invalidate the pending arbitration on every read error (including timeout or
/// invalid framing), never retry it as Pending. These integration guards are
/// not implemented here; raw unit fixtures confer no authority.
pub fn read(receiver: &mut UnixStream) -> Result<Observation<'_>> {
    read_until(receiver, Instant::now() + BUDGET)
}

fn read_until(receiver: &mut UnixStream, deadline: Instant) -> Result<Observation<'_>> {
    // Use nonblocking reads rather than SO_RCVTIMEO: setting a socket timeout
    // can fail on macOS after the peer has exited, before buffered input drains.
    receiver
        .set_nonblocking(true)
        .map_err(|_| anyhow::anyhow!("update_launcher_permit_read_failed"))?;
    let mut bytes = [0; FRAME + 1];
    let mut count = 0;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("update_launcher_permit_read_timeout");
        }
        match receiver.read(&mut bytes[count..]) {
            Ok(0) => break,
            Ok(read) => {
                count += read;
                if count > FRAME {
                    bail!("update_launcher_permit_frame_invalid");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(remaining.min(Duration::from_millis(1)));
            }
            Err(_) => bail!("update_launcher_permit_read_failed"),
        }
    }
    if Instant::now() >= deadline {
        bail!("update_launcher_permit_read_timeout");
    }
    let (kind, binding) = match count {
        0 => (Kind::EmptyEof, None),
        FRAME => (Kind::Permit, Some(parse(&bytes[..FRAME])?)),
        _ => (Kind::PartialEof, None),
    };
    Ok(Observation {
        _receiver: receiver,
        kind,
        binding,
    })
}

fn parse(bytes: &[u8]) -> Result<Binding> {
    if bytes.len() != FRAME || &bytes[..8] != HEADER {
        bail!("update_launcher_permit_frame_invalid");
    }
    let result = (|| -> Result<Binding> {
        Binding::new(
            Uuid::from_slice(&bytes[8..24])?,
            std::str::from_utf8(&bytes[24..88])?,
            std::str::from_utf8(&bytes[88..152])?,
        )
    })();
    result.map_err(|_| anyhow::anyhow!("update_launcher_permit_frame_invalid"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::Shutdown};

    fn frame() -> [u8; FRAME] {
        let mut bytes = [b'a'; FRAME];
        bytes[..8].copy_from_slice(HEADER);
        bytes[8..24].copy_from_slice(Uuid::from_u128(7).as_bytes());
        bytes[88..].fill(b'b');
        bytes
    }

    fn receive(bytes: &[u8]) -> Result<(Kind, Option<Binding>)> {
        let (mut receiver, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(bytes).unwrap();
        // Deliberately keep the writer alive: EOF alone is not owner death.
        writer.shutdown(Shutdown::Write).unwrap();
        let observation = read(&mut receiver)?;
        Ok((observation.kind(), observation.binding().cloned()))
    }

    #[test]
    fn empty_and_partial_eof_are_distinct_and_neither_contains_permit() {
        for length in [0, 1, 7, 8, 24, 88, FRAME - 1] {
            let (kind, binding) = receive(&frame()[..length]).unwrap();
            assert_eq!(
                kind,
                if length == 0 {
                    Kind::EmptyEof
                } else {
                    Kind::PartialEof
                }
            );
            assert!(binding.is_none());
        }
    }

    #[test]
    fn exact_frame_binds_attempt_intent_and_registered_identity() {
        let (kind, binding) = receive(&frame()).unwrap();
        assert_eq!(kind, Kind::Permit);
        let expected = Binding::new(Uuid::from_u128(7), &"a".repeat(64), &"b".repeat(64)).unwrap();
        assert!(binding.as_ref() == Some(&expected));
    }

    #[test]
    fn malformed_overlong_and_repeated_frames_are_never_permits_or_empty_eof() {
        let valid = frame();
        let mut wrong_header = valid;
        wrong_header[0] = b'X';
        let mut nil_attempt = valid;
        nil_attempt[8..24].fill(0);
        let mut invalid_digest = valid;
        invalid_digest[24] = 0xff;
        for bytes in [
            wrong_header.to_vec(),
            nil_attempt.to_vec(),
            invalid_digest.to_vec(),
            [valid.as_slice(), b"x"].concat(),
            [valid.as_slice(), valid.as_slice()].concat(),
        ] {
            assert_eq!(
                receive(&bytes).err().unwrap().to_string(),
                "update_launcher_permit_frame_invalid"
            );
        }
    }

    #[test]
    fn complete_frame_without_eof_and_expired_budget_fail_closed() {
        let (mut receiver, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(&frame()).unwrap();
        let deadline = Instant::now() + Duration::from_millis(100);
        assert_eq!(
            read_until(&mut receiver, deadline)
                .err()
                .unwrap()
                .to_string(),
            "update_launcher_permit_read_timeout"
        );
        assert_eq!(
            read_until(&mut receiver, Instant::now())
                .err()
                .unwrap()
                .to_string(),
            "update_launcher_permit_read_timeout"
        );
    }

    #[test]
    fn fragments_do_not_reset_absolute_read_budget() {
        let (mut receiver, mut writer) = UnixStream::pair().unwrap();
        let sender = std::thread::spawn(move || {
            for byte in frame() {
                if writer.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let result = read_until(&mut receiver, Instant::now() + Duration::from_millis(80));
        assert_eq!(
            result.err().unwrap().to_string(),
            "update_launcher_permit_read_timeout"
        );
        drop(receiver);
        sender.join().unwrap();
    }

    #[test]
    #[ignore = "isolated owned subprocess fixture, invoked by parent test"]
    fn writer_child() {
        let mut writer =
            UnixStream::connect(std::env::var_os("TREER_TEST_PERMIT_SOCKET").unwrap()).unwrap();
        let bytes = frame();
        let count = match std::env::var("TREER_TEST_PERMIT_MODE").unwrap().as_str() {
            "permit" => FRAME,
            "partial" => FRAME - 1,
            "empty" => 0,
            _ => panic!("invalid test mode"),
        };
        for byte in &bytes[..count] {
            writer.write_all(&[*byte]).unwrap();
        }
        writer.shutdown(Shutdown::Write).unwrap();
    }

    #[test]
    fn real_owned_process_frames_and_eof_use_private_socket_without_fixed_port() {
        use std::{
            os::unix::{fs::DirBuilderExt, net::UnixListener},
            path::PathBuf,
            process::{Child, Command, Stdio},
        };
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        struct OwnedChild(Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                if matches!(self.0.try_wait(), Ok(None)) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
        }
        for (mode, expected) in [
            ("permit", Kind::Permit),
            ("partial", Kind::PartialEof),
            ("empty", Kind::EmptyEof),
        ] {
            let path = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("tp-{}", &Uuid::new_v4().simple().to_string()[..16]));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            let fixture = Fixture(path);
            let socket = fixture.0.join("p.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let mut child = OwnedChild(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "self_update::launcher_arbitration::transport::tests::writer_child",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env_clear()
                    .env("TREER_TEST_PERMIT_SOCKET", &socket)
                    .env("TREER_TEST_PERMIT_MODE", mode)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut receiver = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "child connection timed out");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("test accept failed: {error}"),
                }
            };
            // Guarantee the peer has exited before draining buffered bytes.
            // This exercises macOS's post-exit socket behavior, not just a
            // convenient half-close while the sender still runs.
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                assert!(Instant::now() < deadline, "child exit timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
            let observation = read(&mut receiver).unwrap();
            assert_eq!(observation.kind(), expected);
        }
    }
}
