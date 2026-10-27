//! File-only two-step publication under the retained installation lease. The
//! Controller authenticates copies/hashes and supplies all higher-level gates.
//! No execution, cleanup, claim retirement, resume or health-success API.
use crate::windows_update_inputs::{LockedInstallation, PinnedPair};
use std::{
    fs::{File, OpenOptions},
    io,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

const NAMES: [&str; 2] = ["treer-agent-server.exe", "treer.exe"];
const CANDIDATES: [&str; 4] = [
    ".treer-update-controller.new",
    ".treer-update-cli.new",
    ".treer-update-controller.restore",
    ".treer-update-cli.restore",
];
type Identity = (u64, u64);

fn identity(file: &File) -> io::Result<Identity> {
    let (volume, id, links, _) = crate::transfer_file_identity(file)?;
    if id == 0 || links != 1 {
        return Err(io::Error::other("unsafe pair identity"));
    }
    Ok((volume, id))
}

fn named_identity(path: &Path) -> io::Result<Identity> {
    // Query-only access can inspect the directory entry even while our own
    // readonly/RW candidate denies all sharing. No new read/write authority.
    let file = OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    crate::windows_update_store::checked_file(path, &file)?;
    identity(&file)
}

pub struct StagingPair {
    installation: LockedInstallation,
    files: [File; 4],
}

fn create_candidates(directory: &Path) -> io::Result<[File; 4]> {
    let mut files = Vec::new();
    for name in CANDIDATES {
        let path = directory.join(name);
        let file = crate::windows::create_private_update_candidate(&path)?;
        crate::windows_update_store::checked_file(&path, &file)?;
        files.push(file);
    }
    files
        .try_into()
        .map_err(|_| io::Error::other("invalid pair candidates"))
}

fn seal_candidates(directory: &Path, files: &[File; 4]) -> io::Result<()> {
    for (name, file) in CANDIDATES.into_iter().zip(files) {
        file.sync_all()?;
        crate::windows_update_store::checked_file(&directory.join(name), file)?;
        if file.metadata()?.len() == 0 {
            return Err(io::Error::other("empty pair candidate"));
        }
    }
    Ok(())
}

/// Candidate preparation only. Independent physical parents and a reduced
/// reference to the original installation lease stay held. This type has no
/// lifecycle authority. Core supplies the explicit operation
/// grant, authenticated whole inputs and backup verification.
pub struct WorkerReadyPair {
    guard: Guard,
    files: [Option<File>; 4],
    old: [Identity; 2],
    bridges: Vec<File>,
    attempted: bool,
    state: Option<PairState>,
}

pub struct WorkerStagingPair(WorkerReadyPair);

impl PinnedPair {
    pub fn stage_worker_pair(
        &self,
        lock: &crate::windows_update_inputs::InheritedInstallationLock,
    ) -> io::Result<WorkerStagingPair> {
        use std::os::windows::io::AsRawHandle;
        let root = crate::machine_file::Root::open(&self.path, &[])?;
        let expected = crate::windows_update_inputs::lock_identity(&lock._file)?;
        if root.identity()? != self.identity()?
            || expected.0 != self.identity()?.0
            || named_identity(&self.path.join(".treer-controller-update.lock"))? != expected
        {
            return Err(io::Error::other("worker candidate installation differs"));
        }
        let retained = File::from(crate::windows_update_inputs::duplicate_lock(
            lock._file.as_raw_handle(),
        )?);
        // Save the actual installed IDs while the original receiver pins are
        // still held. A later pathname observation is not this baseline.
        let old = [identity(&self.controller)?, identity(&self.cli)?];
        let mut files = Vec::new();
        for name in CANDIDATES {
            files.push(Some(crate::windows::create_private_worker_candidate(
                &self.path.join(name),
            )?));
        }
        Ok(WorkerStagingPair(WorkerReadyPair {
            guard: Guard {
                _root: root,
                _lock: retained,
                path: self.path.clone(),
            },
            files: files
                .try_into()
                .map_err(|_| io::Error::other("invalid candidates"))?,
            old,
            bridges: Vec::new(),
            attempted: false,
            state: None,
        }))
    }
}

impl WorkerStagingPair {
    pub fn files(&mut self) -> [&mut File; 4] {
        let [a, b, c, d] = &mut self.0.files;
        [
            a.as_mut().unwrap(),
            b.as_mut().unwrap(),
            c.as_mut().unwrap(),
            d.as_mut().unwrap(),
        ]
    }
    pub fn seal(self) -> io::Result<WorkerReadyPair> {
        for (name, file) in CANDIDATES.into_iter().zip(self.0.files()?) {
            file.sync_all()?;
            crate::windows_update_store::checked_file(&self.0.guard.path.join(name), file)?;
            if file.metadata()?.len() == 0 {
                return Err(io::Error::other("empty candidate"));
            }
        }
        Ok(self.0)
    }
}

impl WorkerReadyPair {
    pub fn files(&self) -> io::Result<[&File; 4]> {
        let missing = || io::Error::other("unknown worker pair custody");
        Ok([
            self.files[0].as_ref().ok_or_else(missing)?,
            self.files[1].as_ref().ok_or_else(missing)?,
            self.files[2].as_ref().ok_or_else(missing)?,
            self.files[3].as_ref().ok_or_else(missing)?,
        ])
    }
    pub fn old_identities(&self) -> [Identity; 2] {
        self.old
    }
    pub fn state(&self) -> Option<PairState> {
        self.state
    }
    pub fn verify_published(&self) -> io::Result<()> {
        if self.state != Some(PairState::Published) {
            return Err(io::Error::other("pair not published"));
        }
        self.guard.require(self.identities()?)
    }
    pub fn identities(&self) -> io::Result<[Identity; 2]> {
        let files = self.files()?;
        Ok([identity(files[0])?, identity(files[1])?])
    }
    /// Replace write-capable file objects with fresh restrictive read objects.
    /// DuplicateHandle alone leaves FileObject write access/sharing in force
    /// and can prevent PE mapping. Keep an identity bridge across the reopen;
    /// Core MUST rehash all bytes under the final restrictive handles.
    fn executable_pin(&mut self, index: usize) -> io::Result<()> {
        let expected = identity(
            self.files[index]
                .as_ref()
                .ok_or_else(|| io::Error::other("missing candidate"))?,
        )?;
        let path = self.guard.path.join(NAMES[index]);
        let bridge = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&path)?;
        crate::windows_update_store::checked_file(&path, &bridge)?;
        if identity(&bridge)? != expected {
            return Err(io::Error::other("candidate changed"));
        }
        self.bridges.push(bridge);
        self.files[index].take();
        let file = crate::windows_update_store::open_record(&path, false)?;
        if identity(&file)? != expected {
            return Err(io::Error::other("candidate changed"));
        }
        self.files[index] = Some(file);
        Ok(())
    }
    /// In-place single attempt: even Err retains parents/lease and every
    /// remaining candidate/bridge. No consuming-error path drops custody.
    /// Caller records a durable started phase BEFORE entering this method.
    pub fn publish(
        &mut self,
        mut before_effect: impl FnMut() -> io::Result<()>,
    ) -> io::Result<PairState> {
        if self.attempted {
            return Err(io::Error::other("pair already attempted"));
        }
        self.attempted = true;
        self.guard.require(self.old)?;
        let new = self.identities()?;
        before_effect()?;
        if self.guard.replace(self.files()?[0], 0).is_err() {
            self.guard.require(self.old)?;
            self.state = Some(PairState::Refused);
            return Ok(PairState::Refused);
        }
        self.guard.require([new[0], self.old[1]])?;
        // A failure after the first rename is explicitly unknown. Retain all
        // guards; no second attempt or guessed compensation is permitted.
        before_effect()?;
        self.guard.replace(self.files()?[1], 1)?;
        self.guard.require(new)?;
        before_effect()?;
        self.executable_pin(0)?;
        self.executable_pin(1)?;
        self.guard.require(new)?;
        self.state = Some(PairState::Published);
        Ok(PairState::Published)
    }
}

impl LockedInstallation {
    /// Four fixed CREATE_NEW candidates. A partial failure retains every file
    /// and the existing claim; there is no overwrite or automatic retry.
    pub fn stage_pair(self) -> io::Result<StagingPair> {
        let files = create_candidates(&self.pair.path)?;
        Ok(StagingPair {
            installation: self,
            files,
        })
    }
}

impl StagingPair {
    pub fn files(&mut self) -> [&mut File; 4] {
        let [a, b, c, d] = &mut self.files;
        [a, b, c, d]
    }

    pub fn seal(self) -> io::Result<ReadyPair> {
        seal_candidates(&self.installation.pair.path, &self.files)?;
        Ok(ReadyPair {
            installation: self.installation,
            files: self.files,
        })
    }
}

/// The caller must verify all four complete held candidates before publication.
pub struct ReadyPair {
    installation: LockedInstallation,
    files: [File; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairState {
    Published,
    Restored,
    Refused,
}

struct Guard {
    _root: crate::machine_file::Root,
    _lock: File,
    path: PathBuf,
}

impl Guard {
    fn observe(&self) -> io::Result<[Identity; 2]> {
        Ok([
            named_identity(&self.path.join(NAMES[0]))?,
            named_identity(&self.path.join(NAMES[1]))?,
        ])
    }
    fn require(&self, expected: [Identity; 2]) -> io::Result<()> {
        if self.observe()? != expected {
            return Err(io::Error::other("unknown pair publication identity"));
        }
        Ok(())
    }
    fn replace(&self, file: &File, index: usize) -> io::Result<()> {
        crate::windows::rename_update_candidate(file, &self.path.join(NAMES[index]))
    }
    fn old_files(&self) -> io::Result<[File; 2]> {
        Ok([
            crate::windows_update_store::open_record(&self.path.join(NAMES[0]), false)?,
            crate::windows_update_store::open_record(&self.path.join(NAMES[1]), false)?,
        ])
    }
}

pub struct PairFiles {
    guard: Guard,
    files: [File; 2],
    restore: Option<[File; 2]>,
    state: PairState,
}

impl ReadyPair {
    pub fn files(&self) -> [&File; 4] {
        [
            &self.files[0],
            &self.files[1],
            &self.files[2],
            &self.files[3],
        ]
    }

    /// There is a transient mixed pair between renames; this is NOT pair-atomic.
    /// Only a known unchanged/mixed state permits refusal/compensation. Any
    /// unknown observation/restore error stops, leaving the claim and files.
    pub fn publish(self) -> io::Result<PairFiles> {
        let LockedInstallation { pair, _lock } = self.installation;
        let PinnedPair {
            root,
            controller,
            cli,
            path,
        } = pair;
        let old = [identity(&controller)?, identity(&cli)?];
        let [next_c, next_l, restore_c, restore_l] = self.files;
        let new = [identity(&next_c)?, identity(&next_l)?];
        let restored_c = identity(&restore_c)?;
        let guard = Guard {
            _root: root,
            _lock,
            path,
        };
        // Keep physical parents/lease/claim throughout; drop ONLY our old image
        // pins. Other no-delete-sharing readers must make replacement refuse.
        drop((controller, cli));
        guard.require(old)?;
        if guard.replace(&next_c, 0).is_err() {
            guard.require(old)?;
            let files = guard.old_files()?;
            return Ok(PairFiles {
                guard,
                files,
                restore: None,
                state: PairState::Refused,
            });
        }
        guard.require([new[0], old[1]])?;
        if guard.replace(&next_l, 1).is_err() {
            guard.require([new[0], old[1]])?;
            // Our published controller pin must close before compensation.
            drop(next_c);
            guard.replace(&restore_c, 0)?;
            guard.require([restored_c, old[1]])?;
            let cli = crate::windows_update_store::open_record(&guard.path.join(NAMES[1]), false)?;
            return Ok(PairFiles {
                guard,
                files: [crate::windows::readonly_update_file(restore_c)?, cli],
                restore: None,
                state: PairState::Restored,
            });
        }
        guard.require(new)?;
        Ok(PairFiles {
            guard,
            files: [
                crate::windows::readonly_update_file(next_c)?,
                crate::windows::readonly_update_file(next_l)?,
            ],
            restore: Some([restore_c, restore_l]),
            state: PairState::Published,
        })
    }
}

impl PairFiles {
    pub fn state(&self) -> PairState {
        self.state
    }
    pub fn files(&self) -> (&File, &File) {
        (&self.files[0], &self.files[1])
    }
    pub fn identities(&self) -> io::Result<[Identity; 2]> {
        let ids = [identity(&self.files[0])?, identity(&self.files[1])?];
        self.guard.require(ids)?;
        Ok(ids)
    }

    /// Explicit single-use file restoration after a verified publication. Not
    /// workload recovery/health success. A partial restore becomes unknown.
    pub fn restore_files(self) -> io::Result<Self> {
        self.identities()?;
        let Self {
            guard,
            files,
            restore,
            ..
        } = self;
        let [old_c, old_l] =
            restore.ok_or_else(|| io::Error::other("pair restoration unavailable"))?;
        let expected = [identity(&old_c)?, identity(&old_l)?];
        drop(files);
        guard.replace(&old_c, 0)?;
        guard.replace(&old_l, 1)?;
        guard.require(expected)?;
        Ok(Self {
            guard,
            files: [
                crate::windows::readonly_update_file(old_c)?,
                crate::windows::readonly_update_file(old_l)?,
            ],
            restore: None,
            state: PairState::Restored,
        })
    }
}
