//! Read-only fixed local-catalog input. No directory creation, ACL repair,
//! lock/receipt publication or authority derived from the returned bytes.
use std::io::{self, Read};

#[cfg(windows)]
const NAME: &str = "controller-update-catalog.json";
const LIMIT: u64 = 64 * 1024;

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "unsafe Windows update catalog",
    )
}

fn bounded(reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > LIMIT {
        return Err(refused());
    }
    Ok(bytes)
}

/// The Controller supplies its existing configuration directory, not a path
/// from a remote operation. Only this fixed filename may be read. Retains all
/// physical ancestors and the private, no-write/no-delete-sharing file for the
/// entire bounded read. This is a snapshot, not a future catalog-change CAS.
#[cfg(windows)]
pub fn read_catalog(directory: &std::path::Path) -> io::Result<Vec<u8>> {
    let parents = crate::machine_file::Root::open(directory, &[])?;
    if !crate::private_path_is_secure(directory)? {
        return Err(refused());
    }
    crate::windows::require_update_ntfs(directory)?;
    let path = directory.join(NAME);
    let file = crate::windows_update_store::open_record(&path, false)?;
    let (volume, id, _, _) = crate::transfer_file_identity(&file)?;
    let length = file.metadata()?.len();
    if volume != parents.identity()?.0 || id == 0 || length == 0 || length > LIMIT {
        return Err(refused());
    }
    let bytes = bounded(&file)?;
    if bytes.len() as u64 != length || file.metadata()?.len() != length {
        return Err(refused());
    }
    crate::windows_update_store::checked_file(&path, &file)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_update_catalog_file_bounded_read_and_io_refusal() {
        assert_eq!(bounded(b"catalog".as_slice()).unwrap(), b"catalog");
        assert_eq!(
            bounded(vec![1; LIMIT as usize].as_slice()).unwrap().len(),
            LIMIT as usize
        );
        assert!(bounded([].as_slice()).is_err());
        assert!(bounded(vec![1; LIMIT as usize + 1].as_slice()).is_err());
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("fixture read failure"))
            }
        }
        assert!(bounded(Broken).is_err());
    }

    #[cfg(windows)]
    mod native {
        use super::*;
        use std::{
            io::Write,
            path::PathBuf,
            sync::atomic::{AtomicU64, Ordering},
        };

        struct Fixture(PathBuf);
        impl Fixture {
            fn new() -> Self {
                static NEXT: AtomicU64 = AtomicU64::new(0);
                let path = std::env::temp_dir().join(format!(
                    "treer-catalog-file-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir(&path).unwrap();
                crate::secure_directory(&path).unwrap();
                Self(path)
            }
            fn write(&self, bytes: &[u8]) {
                let mut file =
                    crate::windows::create_private_update_file(&self.0.join(NAME)).unwrap();
                file.write_all(bytes).unwrap();
                file.sync_all().unwrap();
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }

        #[test]
        fn windows_update_catalog_file_native_private_fixed_snapshot_no_mutation() {
            let root = Fixture::new();
            root.write(b"opaque bytes; Controller validates the schema");
            assert_eq!(
                read_catalog(&root.0).unwrap(),
                b"opaque bytes; Controller validates the schema"
            );
            assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
            assert!(crate::private_path_is_secure(&root.0).unwrap());
            assert!(crate::private_path_is_secure(&root.0.join(NAME)).unwrap());
        }

        #[test]
        fn windows_update_catalog_file_native_unsafe_acl_and_hardlink_refused() {
            for case in 0..3 {
                let root = Fixture::new();
                root.write(b"catalog");
                match case {
                    0 => crate::apply_sddl_dacl(&root.0, "D:P(A;;GA;;;WD)").unwrap(),
                    1 => crate::apply_sddl_dacl(&root.0.join(NAME), "D:P(A;;GA;;;WD)").unwrap(),
                    _ => std::fs::hard_link(root.0.join(NAME), root.0.join("alias")).unwrap(),
                }
                assert!(read_catalog(&root.0).is_err());
                assert_eq!(std::fs::read(root.0.join(NAME)).unwrap(), b"catalog");
                assert!(!root.0.join("receipt.lock").exists());
                if case < 2 {
                    let path = if case == 0 {
                        root.0.clone()
                    } else {
                        root.0.join(NAME)
                    };
                    assert!(!crate::private_path_is_secure(&path).unwrap());
                }
            }
        }

        #[test]
        fn windows_update_catalog_file_native_missing_bounds_directory_writer_refused() {
            for case in 0..5 {
                let root = Fixture::new();
                let writer = match case {
                    0 => None,
                    1 => {
                        root.write(b"");
                        None
                    }
                    2 => {
                        root.write(&vec![1; LIMIT as usize + 1]);
                        None
                    }
                    3 => {
                        std::fs::create_dir(root.0.join(NAME)).unwrap();
                        None
                    }
                    _ => {
                        root.write(b"catalog");
                        Some(
                            std::fs::OpenOptions::new()
                                .write(true)
                                .open(root.0.join(NAME))
                                .unwrap(),
                        )
                    }
                };
                assert!(read_catalog(&root.0).is_err(), "case {case}");
                drop(writer);
                if case == 4 {
                    assert_eq!(read_catalog(&root.0).unwrap(), b"catalog");
                }
                assert!(!root.0.join("receipt.lock").exists());
                assert!(!root.0.join("receipt.next").exists());
            }
        }
    }
}
