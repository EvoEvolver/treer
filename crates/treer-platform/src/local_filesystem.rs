//! Conservative storage preflight for durable local update jobs.
//! Filesystem type is a prerequisite, not proof of mount durability, ownership,
//! quiescence, or authority to recover. Inspect retained handles, never locators.
use std::{fs::File, io};

/// Require a recognized local filesystem on this exact open handle.
/// Unknown, network, userspace, overlay and memory filesystems fail closed.
/// This does not create, reopen, remount, or modify anything.
pub fn verify(file: &File) -> io::Result<()> {
    let stat = rustix::fs::fstatfs(file).map_err(|_| denied())?;
    #[cfg(target_os = "linux")]
    let supported = linux_supported(stat.f_type as u64);
    #[cfg(target_os = "macos")]
    let supported = darwin_supported(
        &stat.f_fstypename.map(|value| value as u8),
        stat.f_flags & libc::MNT_LOCAL as u32 != 0,
    );
    if supported {
        Ok(())
    } else {
        Err(denied())
    }
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "update storage requires a supported local filesystem",
    )
}

#[cfg(any(target_os = "linux", test))]
fn linux_supported(kind: u64) -> bool {
    // Linux UAPI magic.h: EXT2/3/4 share 0xef53; XFS; Btrfs.
    matches!(kind, 0xef53 | 0x58465342 | 0x9123683e)
}

#[cfg(any(target_os = "macos", test))]
fn darwin_supported(name: &[u8], local: bool) -> bool {
    let Some(end) = name.iter().position(|value| *value == 0) else {
        return false;
    };
    local && matches!(&name[..end], b"apfs" | b"hfs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_network_virtual_memory_unknown_and_nonlocal_storage() {
        for kind in [
            0x6969,
            0xff534d42,
            0x65735546,
            0x794c7630,
            0x01021994,
            0x9fa0,
            0,
            u64::MAX,
        ] {
            assert!(!linux_supported(kind), "unsafe filesystem {kind:x}");
        }
        for kind in [0xef53, 0x58465342, 0x9123683e] {
            assert!(linux_supported(kind));
        }
        for name in [
            b"nfs\0".as_slice(),
            b"smbfs\0",
            b"osxfuse\0",
            b"tmpfs\0",
            b"unknown\0",
            b"apfs",
            b"apfs-extra\0",
        ] {
            assert!(!darwin_supported(name, true));
        }
        for name in [b"apfs\0".as_slice(), b"hfs\0"] {
            assert!(darwin_supported(name, true));
            assert!(!darwin_supported(name, false));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn queries_real_retained_local_temp_directory() {
        let file = File::open(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        verify(&file).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rejects_real_device_filesystem() {
        let file = File::open("/dev/null").unwrap();
        assert_eq!(
            verify(&file).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_real_procfs_handle_without_path_or_filesystem_details() {
        let file = File::open("/proc/self").unwrap();
        let error = verify(&file).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(
            error.to_string(),
            "update storage requires a supported local filesystem"
        );
    }
}
