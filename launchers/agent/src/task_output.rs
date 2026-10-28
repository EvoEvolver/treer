//! Reads only a registered task's bounded tail from the workspace or the
//! provider's per-session task directory under the temp directory.
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Output {
    /// Canonical cwd, then canonical temp roots for provider task directories.
    roots: std::sync::Arc<Vec<PathBuf>>,
    /// The cwd as the provider sees it (no Windows verbatim prefix).
    cwd: PathBuf,
    slots: std::sync::Arc<tokio::sync::Semaphore>,
}
impl Output {
    pub fn new(cwd: &Path) -> Result<Self> {
        let mut roots = vec![std::fs::canonicalize(cwd)?];
        let mut temp = vec![std::env::temp_dir()];
        if cfg!(unix) {
            temp.push(PathBuf::from("/tmp"));
        }
        // An absent/inaccessible optional task temp root must not break
        // Codex or an otherwise healthy Claude launcher startup.
        for base in temp {
            if let Ok(canonical) = std::fs::canonicalize(&base) {
                if !roots[1..].contains(&canonical) {
                    roots.push(canonical);
                }
            }
        }
        Ok(Self {
            roots: std::sync::Arc::new(roots),
            cwd: plain(cwd),
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
        let (roots, cwd) = (self.roots.clone(), self.cwd.clone());
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            read(&roots, &cwd, &task, &session, count.min(65536))
        })
        .await?
    }
}

/// Windows: drop the `\\?\` verbatim prefix canonicalization adds, so the
/// path matches what the provider derives its project directory from.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if cfg!(windows) && !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// The provider's per-user task directory below a temp root. Unix temp roots
/// are shared between users, so it is the one carrying this user's ID; the
/// Windows temp directory is already per user.
fn user_task_root() -> Option<String> {
    #[cfg(unix)]
    return Some(format!("claude-{}", rustix::process::getuid().as_raw()));
    #[cfg(windows)]
    return None;
}
fn task_root_name_ok(name: &str) -> bool {
    match user_task_root() {
        Some(expected) => name == expected,
        None => name.starts_with("claude"),
    }
}

fn read(roots: &[PathBuf], cwd: &Path, task: &Value, session: &str, limit: usize) -> Result<Value> {
    if session.is_empty()
        || !session
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        bail!("Task session is unavailable");
    }
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
    let id = task["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Task not found"))?;
    let fallback = task["kind"] == "shell"
        && !id.is_empty()
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'));
    let path = if let Some(path) = task["outputFile"].as_str() {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    } else if let Some(user) = user_task_root().filter(|_| fallback) {
        // The provider's default shell-task output location (Unix).
        PathBuf::from("/tmp").join(user).join(&project).join(session).join("tasks").join(format!("{id}.output"))
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
    let unavailable = || anyhow::anyhow!("Task output is outside trusted roots or unavailable");
    let canonical = std::fs::canonicalize(&path).map_err(|_| unavailable())?;
    let allowed = roots.iter().enumerate().any(|(index, root)| {
        let Ok(relative) = canonical.strip_prefix(root) else { return false };
        if index == 0 {
            return true;
        }
        let mut parts = relative.components().map(|c| c.as_os_str().to_string_lossy());
        parts.next().is_some_and(|name| task_root_name_ok(&name))
            && parts.next().is_some_and(|name| name == project)
            && parts.next().is_some_and(|name| name == session)
            && parts.next().is_some_and(|name| name == "tasks")
    });
    if !allowed || !std::fs::metadata(&canonical).map_err(|_| unavailable())?.is_file() {
        bail!(unavailable());
    }
    let mut file = crate::fs_util::open_read(&canonical).map_err(|_| unavailable())?;
    let metadata = file.metadata()?;
    #[cfg(unix)]
    let owned = {
        use std::os::unix::fs::MetadataExt;
        metadata.uid() == rustix::process::getuid().as_raw() && metadata.nlink() == 1
    };
    #[cfg(windows)]
    let owned = true;
    if !metadata.is_file() || !owned {
        bail!("Task output must be an owned regular file with one link");
    }
    let length = limit.min(usize::try_from(metadata.len()).unwrap_or(usize::MAX));
    file.seek(SeekFrom::Start(metadata.len() - length as u64))?;
    let mut bytes = Vec::with_capacity(length);
    file.take(length as u64).read_to_end(&mut bytes)?;
    Ok(
        json!({"id":id,"content":String::from_utf8_lossy(&bytes),"size":metadata.len(),"truncated":metadata.len()>limit as u64}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn tails_are_confined_to_the_workspace_and_the_session_task_directory() {
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
            output.read(task.clone(), "session".into(), Some("5")).await.unwrap()["content"],
            "bytes"
        );
        std::fs::write(root.join("outside"), "secret").unwrap();
        for path in [root.join("outside"), cwd.join("../outside"), cwd.join("missing")] {
            assert!(output
                .read(json!({"id":"a","outputFile":path}), "session".into(), None)
                .await
                .is_err());
        }
        let project = plain(&cwd)
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>();
        #[cfg(unix)]
        let user = format!("claude-{}", rustix::process::getuid().as_raw());
        #[cfg(windows)]
        let user = "claude".to_string();
        let native = std::env::temp_dir().join(user).join(&project).join("session/tasks");
        std::fs::create_dir_all(&native).unwrap();
        std::fs::write(native.join("a.output"), "native tail").unwrap();
        let native_task = json!({"id":"a","outputFile":native.join("a.output")});
        assert_eq!(
            output.read(native_task.clone(), "session".into(), None).await.unwrap()["content"],
            "native tail"
        );
        assert!(output.read(native_task, "other".into(), None).await.is_err());
        std::fs::remove_dir_all(native.parent().unwrap().parent().unwrap()).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn outside_symlinks_and_fifos_are_refused() {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("rust-task-{}", uuid::Uuid::new_v4()));
        let cwd = root.join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(root.join("outside"), "secret").unwrap();
        std::os::unix::fs::symlink(root.join("outside"), cwd.join("link")).unwrap();
        let fifo = cwd.join("fifo");
        assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        let output = Output::new(&cwd).unwrap();
        for path in [cwd.join("link"), fifo] {
            let read = output.read(json!({"id":"a","outputFile":path}), "session".into(), None);
            assert!(tokio::time::timeout(std::time::Duration::from_secs(2), read).await.unwrap().is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
