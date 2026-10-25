//! Reads only a registered task's bounded tail from pinned workspace/temp roots.
use anyhow::{bail, Result};
#[cfg(unix)]
use serde_json::json;
use serde_json::Value;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

#[derive(Clone)]
pub struct Output {
    #[cfg(unix)]
    roots: std::sync::Arc<Vec<(PathBuf, std::fs::File)>>,
    slots: std::sync::Arc<tokio::sync::Semaphore>,
}
impl Output {
    pub fn new(cwd: &Path) -> Result<Self> {
        #[cfg(unix)]
        let roots = {
            let mut roots = vec![(cwd.to_path_buf(), open_absolute(cwd)?)];
            for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
                // An absent/inaccessible optional task temp root must not break
                // Codex or an otherwise healthy Claude launcher startup.
                if let Ok(canonical) = std::fs::canonicalize(&base) {
                    if let Ok(anchor) = open_absolute(&canonical) {
                        if base != canonical {
                            roots.push((canonical.clone(), anchor.try_clone()?));
                        }
                        roots.push((base, anchor));
                    }
                }
            }
            std::sync::Arc::new(roots)
        };
        #[cfg(windows)]
        let _ = cwd;
        Ok(Self {
            #[cfg(unix)]
            roots,
            slots: std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
        })
    }
    pub async fn read(&self, task: Value, session: String, tail: Option<&str>) -> Result<Value> {
        let count = tail
            .map(|s| s.parse::<usize>())
            .transpose()?
            .unwrap_or(65536);
        if count == 0 {
            bail!("tail must be a positive byte count");
        }
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("Task output readers are busy"))?;
        #[cfg(unix)]
        {
            let roots = self.roots.clone();
            tokio::task::spawn_blocking(move || {
                let _slot = slot;
                read(&roots, &task, &session, count.min(65536))
            })
            .await?
        }
        #[cfg(windows)]
        {
            let _ = (task, session, count, slot);
            bail!("Task output reading is unavailable on Windows");
        }
    }
}
#[cfg(unix)]
fn open_beneath(anchor: &std::fs::File, relative: &Path) -> Result<std::fs::File> {
    use rustix::fs::{openat, Mode, OFlags};
    let mut file = anchor.try_clone()?;
    let mut parts = relative.components().peekable();
    while let Some(part) = parts.next() {
        let std::path::Component::Normal(name) = part else {
            bail!("Task output path is not confined");
        };
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        if parts.peek().is_some() {
            flags |= OFlags::DIRECTORY;
        }
        file = openat(&file, name, flags, Mode::empty())?.into();
    }
    Ok(file)
}
#[cfg(unix)]
fn open_absolute(path: &Path) -> Result<std::fs::File> {
    let root = std::fs::File::open("/")?;
    let file = open_beneath(&root, path.strip_prefix("/")?)?;
    if !file.metadata()?.is_dir() {
        bail!("Task output root is not a directory");
    }
    Ok(file)
}
#[cfg(unix)]
fn read(
    roots: &[(PathBuf, std::fs::File)],
    task: &Value,
    session: &str,
    limit: usize,
) -> Result<Value> {
    use std::os::unix::fs::{FileExt, MetadataExt};
    if session.is_empty()
        || !session
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        bail!("Task session is unavailable");
    }
    let cwd = &roots[0].0;
    // Node's /[^a-zA-Z0-9]/g works on UTF-16 code units (including astral paths).
    let project = cwd
        .to_string_lossy()
        .encode_utf16()
        .map(|c| {
            if c <= 127 && (c as u8).is_ascii_alphanumeric() {
                c as u8 as char
            } else {
                '-'
            }
        })
        .collect::<String>();
    let suffix = PathBuf::from(format!(
        "claude-{}/{}/{}/tasks",
        rustix::process::getuid().as_raw(),
        project,
        session
    ));
    let id = task["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Task not found"))?;
    let fallback = task["kind"] == "shell"
        && !id.is_empty()
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'));
    let supplied = task["outputFile"].as_str();
    let path = if let Some(path) = supplied {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    } else if fallback {
        PathBuf::from("/tmp")
            .join(&suffix)
            .join(format!("{id}.output"))
    } else {
        bail!("Task output is not available");
    };
    if path.as_os_str().as_encoded_bytes().contains(&0)
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("Task output path is not confined");
    }
    let mut opened = None;
    for (index, (root, anchor)) in roots.iter().enumerate() {
        let allowed = if index == 0 {
            root.clone()
        } else {
            root.join(&suffix)
        };
        if path.starts_with(&allowed) {
            // No canonicalization of provider-controlled paths: every component
            // walks the held capability without following a symlink.
            if let Ok(file) = open_beneath(anchor, path.strip_prefix(root)?) {
                opened = Some(file);
                break;
            }
        }
    }
    let file = opened.ok_or_else(|| {
        anyhow::anyhow!("Task output is outside trusted roots, unavailable, or a symlink")
    })?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.nlink() != 1
    {
        bail!("Task output must be an owned regular file with one link");
    }
    let mut bytes = vec![0; limit.min(usize::try_from(metadata.len()).unwrap_or(usize::MAX))];
    let offset = metadata.len().saturating_sub(bytes.len() as u64);
    let n = file.read_at(&mut bytes, offset)?;
    bytes.truncate(n);
    Ok(
        json!({"id":id,"content":String::from_utf8_lossy(&bytes),"size":metadata.len(),"truncated":metadata.len()>limit as u64}),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn tails_are_session_confined_and_roots_stay_pinned() {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("rust-task-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let cwd = root.join("cwd");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::write(cwd.join("output"), "old actual bytes").unwrap();
        let output = Output::new(&cwd).unwrap();
        let task = json!({"id":"a","outputFile":cwd.join("output")});
        assert_eq!(
            output
                .read(task.clone(), "session".into(), Some("5"))
                .await
                .unwrap()["content"],
            "bytes"
        );
        std::os::unix::fs::symlink(root.join("outside"), cwd.join("link")).unwrap();
        std::fs::write(root.join("outside"), "secret").unwrap();
        for path in [
            cwd.join("link"),
            root.join("outside"),
            cwd.join("../outside"),
        ] {
            assert!(output
                .read(json!({"id":"a","outputFile":path}), "session".into(), None)
                .await
                .is_err());
        }
        let fifo = cwd.join("fifo");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert!(output
            .read(json!({"id":"a","outputFile":fifo}), "session".into(), None)
            .await
            .is_err());
        let project = cwd
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>();
        let native = std::env::temp_dir().join(format!(
            "claude-{}/{}/session/tasks",
            rustix::process::getuid().as_raw(),
            project
        ));
        std::fs::create_dir_all(&native).unwrap();
        std::fs::write(native.join("a.output"), "native tail").unwrap();
        assert_eq!(
            output
                .read(
                    json!({"id":"a","outputFile":native.join("a.output")}),
                    "session".into(),
                    None
                )
                .await
                .unwrap()["content"],
            "native tail"
        );
        assert!(output
            .read(
                json!({"id":"a","outputFile":native.join("a.output")}),
                "other".into(),
                None
            )
            .await
            .is_err());
        std::fs::rename(&cwd, root.join("original")).unwrap();
        std::fs::create_dir(&cwd).unwrap();
        std::fs::write(cwd.join("output"), "replacement bytes").unwrap();
        assert_eq!(
            output.read(task, "session".into(), None).await.unwrap()["content"],
            "old actual bytes"
        );
        std::fs::remove_dir_all(native.parent().unwrap().parent().unwrap()).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
