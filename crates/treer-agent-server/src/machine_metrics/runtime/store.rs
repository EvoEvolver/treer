use super::*;
use sha2::{Digest, Sha256};
#[cfg(windows)]
use std::fs;
use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek, Write},
};

pub(super) struct Store {
    path: PathBuf,
    directory: File,
    #[cfg(windows)]
    _directory_guards: treer_platform::machine_file::Root,
    salt: [u8; 32],
}

impl Store {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        if path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        }) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        #[cfg(unix)]
        let directory = {
            use rustix::fs::{self as fsx, Mode, OFlags};
            let mut dir: File = fsx::open(
                "/",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )?
            .into();
            let parts: Vec<_> = path
                .components()
                .filter_map(|part| match part {
                    std::path::Component::Normal(name) => Some(name),
                    _ => None,
                })
                .collect();
            if !path.is_absolute() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            for (index, name) in parts.iter().enumerate() {
                if index + 1 == parts.len() {
                    match fsx::mkdirat(&dir, *name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                dir = fsx::openat(
                    &dir,
                    *name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?
                .into();
                let stat = fsx::fstat(&dir)?;
                let uid = rustix::process::geteuid().as_raw();
                if (stat.st_uid != 0 && stat.st_uid != uid)
                    || (stat.st_mode & 0o022 != 0 && stat.st_mode & 0o1000 == 0)
                    || (index + 1 == parts.len()
                        && (stat.st_uid != uid || stat.st_mode & 0o077 != 0))
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
            }
            dir
        };
        #[cfg(windows)]
        let directory_guards = {
            if !path.exists() {
                fs::create_dir(&path)?;
                treer_platform::secure_directory(&path)?;
            }
            // Root retains no-delete-sharing handles to every component,
            // rejecting junctions/reparse points and parent replacement.
            let root = treer_platform::machine_file::Root::open_local(&path)?;
            if !treer_platform::private_path_is_secure(&path)? {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            root
        };
        #[cfg(windows)]
        let directory = treer_platform::machine_file::open_protected(&path)?;
        let mut store = Self {
            path,
            directory,
            #[cfg(windows)]
            _directory_guards: directory_guards,
            salt: [0; 32],
        };
        let mut salt_file = store.open_file("salt", true, false)?;
        if salt_file.metadata()?.len() == 0 {
            let _lease = store.writer_lease()?;
            // Another cold opener may have initialized it while we waited.
            if salt_file.metadata()?.len() == 0 {
                let mut digest = Sha256::new();
                digest.update(uuid::Uuid::new_v4().as_bytes());
                digest.update(uuid::Uuid::new_v4().as_bytes());
                let bytes = digest.finalize();
                salt_file.write_all(&bytes)?;
                salt_file.sync_all()?;
            }
        }
        salt_file.rewind()?;
        salt_file.read_exact(&mut store.salt)?;
        if salt_file.metadata()?.len() != 32 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if store.generation().is_err() {
            let _lease = store.writer_lease()?;
            if store.generation().is_err() {
                store.advance_generation()?;
            }
        }
        if let Ok(_lease) = store.writer_lease() {
            store.prune(Utc::now())?;
        }
        Ok(store)
    }
    pub fn gpu_input(&self) -> io::Result<File> {
        self.open_file("salt", true, false)
    }
    pub fn disabled(&self) -> bool {
        self.path.join("disabled").symlink_metadata().is_ok()
    }
    pub fn set_enabled(&self, enabled: bool) -> io::Result<()> {
        if enabled {
            if self.disabled() {
                self.remove("disabled")?;
            }
        } else {
            self.open_file("disabled", true, false)?.sync_all()?;
            if let Ok(_lease) = self.writer_lease() {
                self.clear()?;
            }
        }
        self.sync_directory()
    }

    fn open_file(&self, name: &str, create: bool, append: bool) -> io::Result<File> {
        #[cfg(unix)]
        {
            use rustix::fs::{self as fsx, Mode, OFlags};
            let flags = if create {
                OFlags::RDWR | OFlags::CREATE
            } else {
                OFlags::RDONLY
            };
            let flags = flags
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | if append {
                    OFlags::APPEND
                } else {
                    OFlags::empty()
                };
            let file: File =
                fsx::openat(&self.directory, name, flags, Mode::RUSR | Mode::WUSR)?.into();
            let stat = fsx::fstat(&file)?;
            if fsx::FileType::from_raw_mode(stat.st_mode) != fsx::FileType::RegularFile
                || stat.st_nlink != 1
                || stat.st_uid != rustix::process::geteuid().as_raw()
                || stat.st_mode & 0o077 != 0
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(file)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let path = self.path.join(name);
            let open = |new: bool| {
                let mut options = fs::OpenOptions::new();
                options
                    .read(true)
                    .write(create)
                    .append(append)
                    .share_mode(1 | 2 | 4)
                    .custom_flags(0x0020_0000);
                if new {
                    options.create_new(true);
                }
                options.open(&path)
            };
            let file = match open(false) {
                Ok(file) => file,
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    let file = open(true)?;
                    treer_platform::secure_file(&path)?;
                    file
                }
                Err(error) => return Err(error),
            };
            let (_, _, links, attrs) = treer_platform::transfer_file_identity(&file)?;
            if links != 1
                || attrs & (0x400 | 0x10) != 0
                || !treer_platform::private_path_is_secure(&path)?
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(file)
        }
    }
    fn lease(&self, name: &str) -> io::Result<File> {
        let file = self.open_file(name, true, false)?;
        file.try_lock()
            .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
        Ok(file)
    }
    pub fn writer_lease(&self) -> io::Result<File> {
        self.lease("writer.lock")
    }
    pub fn query_lease(&self) -> io::Result<File> {
        self.lease("query.lock")
    }
    pub fn generation(&self) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.open_file("generation", false, false)?
            .take(37)
            .read_to_end(&mut bytes)?;
        if bytes.len() != 36
            || uuid::Uuid::try_parse(
                std::str::from_utf8(&bytes).map_err(|_| io::ErrorKind::InvalidData)?,
            )
            .is_err()
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(bytes)
    }
    fn advance_generation(&self) -> io::Result<()> {
        let name = format!("{}.epoch", uuid::Uuid::new_v4());
        let mut file = self.open_file(&name, true, false)?;
        file.write_all(uuid::Uuid::new_v4().to_string().as_bytes())?;
        file.sync_all()?;
        #[cfg(unix)]
        rustix::fs::renameat(
            &self.directory,
            name.as_str(),
            &self.directory,
            "generation",
        )?;
        #[cfg(windows)]
        treer_platform::replace_file(&self.path.join(name), &self.path.join("generation"))?;
        self.sync_directory()
    }
    fn sync_directory(&self) -> io::Result<()> {
        #[cfg(windows)]
        let _ = self.directory.metadata()?;
        #[cfg(unix)]
        self.directory.sync_all()?;
        Ok(())
    }
    fn entries(&self) -> io::Result<Vec<(i64, String, u64)>> {
        let mut entries = Vec::new();
        // Enumeration is bounded before pushing; every subsequent open/unlink
        // is relative to the pinned directory, never an enumerated path.
        #[cfg(unix)]
        let names = rustix::fs::Dir::read_from(&self.directory)?.map(|entry| {
            entry.map_err(io::Error::from).and_then(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .map(str::to_owned)
                    .map_err(|_| io::ErrorKind::InvalidData.into())
            })
        });
        #[cfg(windows)]
        let names = fs::read_dir(&self.path)?.map(|entry| {
            entry.and_then(|entry| {
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| io::ErrorKind::InvalidData.into())
            })
        });
        for (index, name) in names.enumerate() {
            if index >= 300 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let name = name?;
            if let Some(stamp) = name
                .strip_suffix(".samples")
                .and_then(|v| v.parse::<i64>().ok())
            {
                let file = self.open_file(&name, false, false)?;
                entries.push((stamp, name, file.metadata()?.len()));
            }
        }
        if entries.len() > 289 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        entries.sort_by_key(|entry| entry.0);
        Ok(entries)
    }
    fn prune(&self, now: DateTime<Utc>) -> io::Result<()> {
        for (at, name, _) in self.entries()? {
            let file = self.open_file(&name, false, false)?;
            if at > now.timestamp()
                || file
                    .metadata()?
                    .modified()?
                    .duration_since(std::time::SystemTime::now())
                    .is_ok_and(|ahead| ahead > Duration::from_secs(2))
            {
                return self.clear();
            }
            if at <= now.timestamp() - RETENTION_SECONDS {
                self.remove(&name)?;
            }
        }
        Ok(())
    }
    pub fn snapshot(&self) -> Result<Vec<(i64, String, u64)>, ProtocolError> {
        self.entries()
            .map_err(|_| failure("machine_metrics_invalid_response"))
    }
    fn remove(&self, name: &str) -> io::Result<()> {
        #[cfg(unix)]
        {
            rustix::fs::unlinkat(&self.directory, name, rustix::fs::AtFlags::empty())?;
            Ok(())
        }
        #[cfg(windows)]
        {
            // Validate the private, non-reparse, single-link file before
            // deletion under the retained directory ancestry handles.
            drop(self.open_file(name, false, false)?);
            fs::remove_file(self.path.join(name))
        }
    }
    pub fn clear(&self) -> io::Result<()> {
        self.advance_generation()?;
        for (_, name, _) in self.entries()? {
            self.remove(&name)?;
        }
        self.set_capacity_floor(0)?;
        self.sync_directory()
    }
    pub fn capacity_floor(&self) -> io::Result<i64> {
        let mut text = String::new();
        match self.open_file("capacity-floor", false, false) {
            Ok(file) => {
                file.take(32).read_to_string(&mut text)?;
                text.parse().map_err(|_| io::ErrorKind::InvalidData.into())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }
    fn set_capacity_floor(&self, floor: i64) -> io::Result<()> {
        let mut file = self.open_file("capacity-floor", true, false)?;
        file.set_len(0)?;
        write!(file, "{floor}")?;
        file.sync_data()
    }
    pub fn append(&self, sample: &Sample) -> io::Result<()> {
        if self.disabled() {
            return self.clear();
        }
        let mut bytes = serde_json::to_vec(sample)?;
        // Detect valid-JSON corruption as well as torn writes. The digest is
        // an integrity check, not authentication against the owning account.
        let digest = format!("{:x}", Sha256::digest(&bytes));
        bytes.push(b'\t');
        bytes.extend_from_slice(digest.as_bytes());
        bytes.push(b'\n');
        if bytes.len() > RECORD_BYTES {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let now = sample.at.timestamp();
        let segment = now.div_euclid(SEGMENT_SECONDS) * SEGMENT_SECONDS;
        let entries = self.entries()?;
        let mut total: u64 = entries.iter().map(|entry| entry.2).sum();
        for (at, name, size) in entries {
            // Drop an entire boundary segment early: never keep expired data
            // physically on disk pending a viewer or cleanup query.
            let capacity = total + bytes.len() as u64 + 65536 > DISK_BYTES;
            if at <= now - RETENTION_SECONDS || at > now || capacity {
                if capacity && at >= now - RETENTION_SECONDS && at <= now {
                    self.advance_generation()?;
                    self.set_capacity_floor(self.capacity_floor()?.max(at + SEGMENT_SECONDS))?;
                }
                self.remove(&name)?;
                total = total.saturating_sub(size);
            }
        }
        let mut file = self.open_file(&format!("{segment}.samples"), true, true)?;
        // At most 60 frames in each retained five-minute segment. Keeping at
        // most 288 segments bounds the physical record count to 17,280 even
        // after sub-threshold clock corrections or a collector handover.
        if file.metadata()?.len() > 1024 * 1024 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut previous = file.try_clone()?;
        let mut buffer = [0_u8; 8192];
        let mut records = 0;
        loop {
            let count = previous.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            records += buffer[..count]
                .iter()
                .filter(|byte| **byte == b'\n')
                .count();
        }
        if records >= 60 {
            return Ok(());
        }
        file.write_all(&bytes)?;
        file.sync_data()
    }
    /// One bounded line at a time; a torn or invalid record refuses the entire
    /// response. Captured file lengths prevent reading a concurrent append tail.
    #[cfg(test)]
    pub fn scan(
        &self,
        now: DateTime<Utc>,
        visit: impl FnMut(Sample) -> Result<(), ProtocolError>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), ProtocolError> {
        self.scan_snapshot(&self.snapshot()?, now, visit, cancelled)
    }
    pub fn scan_snapshot(
        &self,
        entries: &[(i64, String, u64)],
        now: DateTime<Utc>,
        mut visit: impl FnMut(Sample) -> Result<(), ProtocolError>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), ProtocolError> {
        if self.disabled() {
            return Err(failure("machine_metrics_disabled"));
        }
        let mut previous = None;
        let mut count = 0;
        for (at, name, length) in entries {
            if cancelled() {
                return Err(failure("machine_metrics_timeout"));
            }
            if *at <= now.timestamp() - RETENTION_SECONDS || *at > now.timestamp() {
                continue;
            }
            if *length > DISK_BYTES {
                return Err(failure("machine_metrics_invalid_response"));
            }
            let file = self
                .open_file(name, false, false)
                .map_err(|_| failure("machine_metrics_invalid_response"))?;
            let mut reader = BufReader::new(file.take(*length));
            let mut line = Vec::with_capacity(RECORD_BYTES);
            loop {
                if cancelled() {
                    return Err(failure("machine_metrics_timeout"));
                }
                line.clear();
                let n = reader
                    .by_ref()
                    .take(RECORD_BYTES as u64 + 1)
                    .read_until(b'\n', &mut line)
                    .map_err(|_| failure("machine_metrics_invalid_response"))?;
                if n == 0 {
                    break;
                }
                if n > RECORD_BYTES || line.last() != Some(&b'\n') || count >= 17280 {
                    return Err(failure("machine_metrics_invalid_response"));
                }
                let separator = line
                    .iter()
                    .rposition(|byte| *byte == b'\t')
                    .ok_or_else(|| failure("machine_metrics_invalid_response"))?;
                let expected = format!("{:x}", Sha256::digest(&line[..separator]));
                if line[separator + 1..line.len() - 1] != *expected.as_bytes() {
                    return Err(failure("machine_metrics_invalid_response"));
                }
                let sample: Sample = serde_json::from_slice(&line[..separator])
                    .map_err(|_| failure("machine_metrics_invalid_response"))?;
                if sample.at.timestamp().div_euclid(SEGMENT_SECONDS) * SEGMENT_SECONDS != *at
                    || previous.is_some_and(|p| sample.at <= p)
                    || sample.gpus.len() > MACHINE_METRICS_MAX_GPUS
                {
                    return Err(failure("machine_metrics_invalid_response"));
                }
                previous = Some(sample.at);
                count += 1;
                if sample.at <= now {
                    visit(sample)?;
                }
            }
        }
        Ok(())
    }
}
