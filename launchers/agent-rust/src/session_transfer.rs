//! Allowlisted Claude snapshots and explicitly prepared startup. No credentials
//! are read; persistent startup reads only its own leased runtime checkpoint.
use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use rustix::fs::{self, AtFlags, Mode, OFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

const BYTES: usize = 64 * 1024 * 1024;
const INPUT: usize = 96 * 1024 * 1024;
const FILES: usize = 1024;
const INSTRUCTIONS: &[&str] = &["AGENTS.md", "CLAUDE.md", "MEMORY.md", ".claude/CLAUDE.md"];
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bundle {
    schema: String,
    harness: String,
    session_id: String,
    cwd: String,
    exported_at: String,
    files: Vec<Entry>,
}
#[derive(Serialize, Deserialize)]
struct Entry {
    scope: String,
    path: String,
    encoding: String,
    content: String,
    sha256: String,
}
fn hash(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}
fn uuid(s: &str) -> Result<()> {
    ensure!(
        s.len() == 36
            && s.bytes()
                .enumerate()
                .all(|(i, c)| if [8, 13, 18, 23].contains(&i) {
                    c == b'-'
                } else {
                    c.is_ascii_hexdigit()
                }),
        "session_id_must_be_uuid"
    );
    Ok(())
}
fn safe(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty()
            && !s.contains(['\\', '\0', ':'])
            && s.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
            && !s.starts_with('/'),
        "unsafe_session_path"
    );
    ensure!(s.split('/').count() <= 14, "session_path_too_deep");
    Ok(())
}
fn project_key(s: &str) -> String {
    s.encode_utf16()
        .map(|c| {
            if c < 128 && (c as u8).is_ascii_alphanumeric() {
                char::from(c as u8)
            } else {
                '-'
            }
        })
        .collect()
}
fn open_at(root: &File, path: &Path, directory: bool) -> Result<File> {
    let mut fd = root.try_clone()?;
    let mut parts = path.components().peekable();
    while let Some(p) = parts.next() {
        let Component::Normal(name) = p else {
            bail!("unsafe_session_path")
        };
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        if directory || parts.peek().is_some() {
            flags |= OFlags::DIRECTORY;
        }
        // Only the final component is a file when directory=false.
        if !directory && parts.peek().is_none() {
            flags.remove(OFlags::DIRECTORY);
        }
        fd = fs::openat(&fd, name, flags, Mode::empty())?.into();
        let metadata = fd.metadata()?;
        if metadata.is_dir() {
            ensure!(
                [0, rustix::process::getuid().as_raw()].contains(&metadata.uid())
                    && (metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0),
                "session_directory_not_trusted"
            );
        }
    }
    Ok(fd)
}
fn root(path: &Path) -> Result<(PathBuf, File)> {
    ensure!(path.is_absolute(), "absolute_path_required");
    let path = path.canonicalize()?;
    let fd = open_at(&File::open("/")?, path.strip_prefix("/")?, true)?;
    Ok((path, fd))
}
fn read(root: &File, path: &str, limit: usize) -> Result<Vec<u8>> {
    safe(path)?;
    let mut fd = open_at(root, Path::new(path), false)?;
    let before = fd.metadata()?;
    ensure!(
        before.is_file()
            && before.uid() == rustix::process::getuid().as_raw()
            && before.nlink() == 1
            && before.mode() & 0o022 == 0,
        "session_file_not_owned_regular"
    );
    ensure!(before.len() <= limit as u64, "session_too_large");
    let mut bytes = Vec::new();
    (&mut fd).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    let after = fd.metadata()?;
    ensure!(
        bytes.len() <= limit
            && bytes.len() as u64 == before.len()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "session_changed_during_export"
    );
    Ok(bytes)
}
fn exists(root: &File, path: &str) -> Result<bool> {
    // Walk ancestors with nofollow, including for optional missing leaves.
    match open_at(root, Path::new(path), false) {
        Ok(_) => Ok(true),
        Err(e) if e.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT) => {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}
fn rows(bytes: &[u8]) -> Result<Vec<Value>> {
    ensure!(
        !bytes.is_empty() && bytes.last() == Some(&b'\n'),
        "incomplete_native_jsonl"
    );
    let mut out = Vec::new();
    for line in bytes.split(|c| *c == b'\n').filter(|l| !l.is_empty()) {
        ensure!(out.len() < 100_000, "native_record_limit");
        let row = crate::bounded_json::decode(line)?;
        ensure!(row.is_object(), "invalid_native_record");
        out.push(row);
    }
    Ok(out)
}
fn main_session(bytes: &[u8], id: &str) -> Result<()> {
    ensure!(
        rows(bytes)?
            .iter()
            .any(|r| r.get("sessionId").and_then(Value::as_str) == Some(id)),
        "native_session_id_mismatch"
    );
    Ok(())
}
fn allowed(scope: &str, path: &str) -> bool {
    match scope {
        "session" => {
            path == "session.jsonl" || path.starts_with("subagents/") && path.ends_with(".jsonl")
        }
        "memory" => path.ends_with(".md"),
        "project" => {
            INSTRUCTIONS.contains(&path)
                || [".codex/memory/", ".claude/memory/"]
                    .iter()
                    .any(|p| path.starts_with(p) && path.ends_with(".md"))
        }
        _ => false,
    }
}
fn walk(
    fd: &File,
    prefix: &str,
    suffix: &str,
    depth: usize,
    visited: &mut usize,
    out: &mut Vec<String>,
) -> Result<()> {
    ensure!(depth <= 10, "session_directory_too_deep");
    let mut names = Vec::new();
    for entry in fs::Dir::read_from(fd)? {
        let entry = entry?;
        let name = entry.file_name().to_str()?.to_owned();
        if name == "." || name == ".." {
            continue;
        }
        *visited += 1;
        ensure!(*visited <= 10_000, "session_directory_entry_limit");
        names.push(name);
    }
    names.sort();
    for name in names {
        let child = open_at(fd, Path::new(&name), false)?;
        let metadata = child.metadata()?;
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if metadata.is_dir() {
            walk(&child, &path, suffix, depth + 1, visited, out)?;
        } else {
            ensure!(metadata.is_file(), "session_special_file_refused");
            if path.ends_with(suffix) {
                out.push(path);
                ensure!(out.len() <= FILES, "session_file_limit");
            }
        }
    }
    Ok(())
}
fn export(home: &Path, cwd: &Path, id: &str) -> Result<Bundle> {
    uuid(id)?;
    let (cwd, project) = root(cwd)?;
    let (_, home) = root(home)?;
    let cwd = cwd.to_str().context("non_utf8_cwd")?.to_owned();
    let native = format!("projects/{}", project_key(&cwd));
    let mut sources = vec![(
        "session".to_owned(),
        "session.jsonl".to_owned(),
        format!("{native}/{id}.jsonl"),
        false,
    )];
    let mut visited = 0;
    for (scope, prefix, base, suffix, is_project) in [
        (
            "session",
            "subagents/",
            format!("{native}/{id}/subagents"),
            ".jsonl",
            false,
        ),
        ("memory", "", format!("{native}/memory"), ".md", false),
        (
            "project",
            ".codex/memory/",
            ".codex/memory".into(),
            ".md",
            true,
        ),
        (
            "project",
            ".claude/memory/",
            ".claude/memory".into(),
            ".md",
            true,
        ),
    ] {
        let anchor = if is_project { &project } else { &home };
        if exists(anchor, &base)? {
            let dir = open_at(anchor, Path::new(&base), true)?;
            let mut paths = Vec::new();
            walk(&dir, "", suffix, 0, &mut visited, &mut paths)?;
            sources.extend(paths.into_iter().map(|p| {
                (
                    scope.into(),
                    format!("{prefix}{p}"),
                    format!("{base}/{p}"),
                    is_project,
                )
            }));
        }
    }
    for path in INSTRUCTIONS {
        if exists(&project, path)? {
            sources.push(("project".into(), (*path).into(), (*path).into(), true));
        }
    }
    ensure!(sources.len() <= FILES, "session_file_limit");
    let mut total = 0;
    let mut files = Vec::new();
    for (scope, path, source, is_project) in sources {
        let bytes = read(
            if is_project { &project } else { &home },
            &source,
            BYTES - total,
        )?;
        total += bytes.len();
        if scope == "session" {
            rows(&bytes)?;
            if path == "session.jsonl" {
                main_session(&bytes, id)?;
            }
        }
        files.push(Entry {
            scope,
            path,
            encoding: "base64".into(),
            content: STANDARD.encode(&bytes),
            sha256: hash(&bytes),
        });
    }
    Ok(Bundle {
        schema: "treer.agent-session/v1".into(),
        harness: "claude".into(),
        session_id: id.into(),
        cwd,
        exported_at: chrono::Utc::now().to_rfc3339(),
        files,
    })
}
// The actor supplies all selectors; HTTP clients cannot choose source paths.
pub(crate) fn managed_snapshot(home: &Path, cwd: &Path, id: &str) -> Result<Value> {
    #[cfg(feature = "fixture-harness")]
    if let Some(gate) = std::env::var_os("RUST_SESSION_SNAPSHOT_GATE") {
        let gate = PathBuf::from(gate);
        std::fs::write(gate.with_extension("entered"), b"entered")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !gate.exists() {
            ensure!(
                std::time::Instant::now() < deadline,
                "snapshot_fixture_gate_timeout"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    serde_json::to_value(export(home, cwd, id)?).map_err(Into::into)
}
// This is the same native user/assistant JSONL source read by Node loadHistory,
// not a provider result or a claim that an old operation completed. Historical
// envelopes are displayed in file order, with no transferred receipt authority.
fn native_history(bytes: &[u8], id: &str) -> Result<Value> {
    main_session(bytes, id)?;
    let mut history = crate::transcript::Transcript::default();
    let mut count = 0usize;
    for (index, row) in rows(bytes)?.into_iter().enumerate() {
        let kind = match row["type"].as_str() {
            Some("user") => "userMessage",
            Some("assistant") => "agentMessage",
            _ => continue,
        };
        let content = &row["message"]["content"];
        let mut text = if let Some(text) = content.as_str() {
            text.to_owned()
        } else {
            serde_json::to_string(content)?
        };
        let truncated = text.len() > 32768;
        if truncated {
            let mut end = 32768;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str("\n[Native history display truncated]");
        }
        let turn = format!("native-history-{index}");
        history.insert(json!({"id":turn,"status":"completed","projectionOnly":true,"historySource":"native-jsonl","items":[{"id":format!("native-item-{index}"),"kind":kind,"text":text,"sourceTurnId":turn,"historySource":"native-jsonl","displayTruncated":truncated}]}))?;
        count += 1;
    }
    ensure!(count > 0, "native_history_has_no_messages");
    Ok(
        json!({"threadId":id,"turns":&*history,"start":history.dropped,"total":count,"source":"native-jsonl","nativeSha256":hash(bytes),"providerConfirmed":false}),
    )
}
pub struct Launch {
    pub cwd: PathBuf,
    pub config: PathBuf,
    pub id: String,
    pub history: Value,
    persistent: bool,
    native_hash: String,
    root: File,
}
pub fn launch(args: &[String], persistent: bool) -> Result<Launch> {
    ensure!(
        args.len() == 6,
        "launch_requires_directory_session_and_auth"
    );
    let mut options = BTreeMap::new();
    for pair in args.chunks_exact(2) {
        ensure!(
            ["--directory", "--session-id", "--auth"].contains(&pair[0].as_str())
                && options.insert(pair[0].as_str(), pair[1].as_str()).is_none(),
            "invalid_launch_option"
        );
    }
    ensure!(
        options.get("--auth") == Some(&"environment"),
        "launch_auth_must_be_environment"
    );
    // Do not load .credentials.json, keychains, old HOME/config or bundle env.
    // This selects a provider-supported environment credential, not proof that
    // remote authentication succeeded; that remains the provider's decision.
    ensure!(
        ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"]
            .iter()
            .any(|key| std::env::var_os(key).is_some_and(|v| !v.is_empty())),
        "launch_explicit_environment_auth_required"
    );
    let id = options
        .get("--session-id")
        .context("launch_session_required")?
        .to_string();
    uuid(&id)?;
    let (cwd, dir) = root(Path::new(
        options
            .get("--directory")
            .context("launch_directory_required")?,
    ))?;
    if persistent && std::env::var_os("TREER_AGENT_ID").is_some_and(|v| !v.is_empty()) {
        ensure!(
            std::fs::canonicalize(std::env::current_dir()?)? == cwd,
            "prepared_startup_requires_matching_process_cwd"
        );
    }
    let plan = crate::bounded_json::decode(&read(&dir, ".harness/claude-launch.json", 4096)?)?;
    ensure!(
        plan["schema"] == "treer.claude-launch/v1"
            && plan["cwd"] == cwd.to_string_lossy().as_ref()
            && plan["sessionId"] == id,
        "launch_binding_mismatch"
    );
    let native_hash = plan["nativeSha256"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .context("launch_hash_required")?
        .to_owned();
    let history = if persistent {
        Value::Null
    } else {
        read_launch_history(&dir, &cwd, &id, &native_hash)?
    };
    Ok(Launch {
        config: cwd.join(".harness/claude"),
        cwd,
        id,
        history,
        persistent,
        native_hash,
        root: dir,
    })
}
fn read_launch_history(dir: &File, cwd: &Path, id: &str, digest: &str) -> Result<Value> {
    let path = format!(
        ".harness/claude/projects/{}/{id}.jsonl",
        project_key(cwd.to_str().context("non_utf8_destination")?)
    );
    let bytes = read(dir, &path, BYTES)?;
    ensure!(digest == hash(&bytes), "launch_native_hash_mismatch");
    native_history(&bytes, id)
}
pub fn claim_launch(launch: &mut Launch, private: &Path, agent: &str) -> Result<()> {
    // Caller holds the ordinary private single-writer lease. A persisted
    // startup may resume only its own completed checkpoint, never recreate it
    // from the imported file (which legitimately grows after first launch).
    let private = std::fs::canonicalize(private)?;
    let (_, current) = root(&launch.cwd)?;
    let a = current.metadata()?;
    let b = launch.root.metadata()?;
    ensure!(
        a.dev() == b.dev() && a.ino() == b.ino(),
        "launch_directory_changed"
    );
    if launch.persistent {
        if let Some(saved) = crate::store::Store::open(&private, &launch.cwd)?.load()? {
            let intent = crate::bounded_json::decode(&read(
                &launch.root,
                ".harness/launch-intent.json",
                4096,
            )?)?;
            let metadata = std::fs::metadata(&private)?;
            ensure!(
                intent["schema"] == "treer.claude-launch-intent/v1"
                    && intent["sessionId"] == launch.id
                    && intent["nativeSha256"] == launch.native_hash
                    && intent["privateRoot"] == private.to_string_lossy().as_ref()
                    && intent["privateDevice"] == metadata.dev()
                    && intent["privateInode"] == metadata.ino()
                    && intent["agentId"] == agent
                    && saved["harness"] == "claude"
                    && saved["threadId"] == launch.id,
                "prepared_startup_checkpoint_binding_mismatch"
            );
            return Ok(());
        }
        // Presence of an intent without a checkpoint is an earlier failed or
        // ambiguous attempt. Exclusive claim below refuses; no automatic retry.
        launch.history =
            read_launch_history(&launch.root, &launch.cwd, &launch.id, &launch.native_hash)?;
    }
    // The ordinary lease is already held. Never bootstrap an existing runtime,
    // including one whose journal is absent but other durable state remains.
    for entry in std::fs::read_dir(&private)? {
        ensure!(
            entry?.file_name() == ".rust-launcher.lock",
            "launch_requires_fresh_private_root"
        );
    }
    let native = format!(
        ".harness/claude/projects/{}/{}.jsonl",
        project_key(launch.cwd.to_str().context("non_utf8_destination")?),
        launch.id
    );
    ensure!(
        hash(&read(&launch.root, &native, BYTES)?) == launch.history["nativeSha256"],
        "launch_native_changed"
    );
    let metadata = std::fs::metadata(&private)?;
    write_private(
        &launch.root,
        ".harness/launch-intent.json",
        &serde_json::to_vec(
            &json!({"schema":"treer.claude-launch-intent/v1","sessionId":launch.id,"nativeSha256":launch.history["nativeSha256"],"automaticRetry":false,"privateRoot":private,"privateDevice":metadata.dev(),"privateInode":metadata.ino(),"agentId":agent}),
        )?,
    )?;
    launch.root.sync_all()?;
    Ok(())
}
fn decoded(bundle: Bundle, id: &str, cwd: &str) -> Result<Vec<(String, Vec<u8>)>> {
    ensure!(
        bundle.schema == "treer.agent-session/v1" && bundle.harness == "claude",
        "unsupported_session_bundle"
    );
    uuid(&bundle.session_id)?;
    uuid(id)?;
    ensure!(
        !id.eq_ignore_ascii_case(&bundle.session_id),
        "import_requires_fresh_uuid"
    );
    ensure!(Path::new(&bundle.cwd).is_absolute(), "invalid_bundle_cwd");
    ensure!(
        !bundle.files.is_empty() && bundle.files.len() <= FILES,
        "session_file_limit"
    );
    let mut seen = HashSet::new();
    let mut total = 0;
    let mut rewritten = 0;
    let mut main = false;
    let mut out = Vec::new();
    for file in bundle.files {
        safe(&file.path)?;
        ensure!(
            allowed(&file.scope, &file.path)
                && file.encoding == "base64"
                && file.sha256.len() == 64
                && file
                    .sha256
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "invalid_session_entry"
        );
        ensure!(
            seen.insert(format!("{}/{}", file.scope, file.path)),
            "duplicate_session_path"
        );
        ensure!(
            file.content.len() <= BYTES.div_ceil(3) * 4,
            "session_too_large"
        );
        let mut bytes = STANDARD.decode(&file.content)?;
        ensure!(
            STANDARD.encode(&bytes) == file.content && hash(&bytes) == file.sha256,
            "session_hash_or_encoding_mismatch"
        );
        total += bytes.len();
        ensure!(total <= BYTES, "session_too_large");
        let destination = match file.scope.as_str() {
            "session" => {
                if file.path == "session.jsonl" {
                    main_session(&bytes, &bundle.session_id)?;
                    main = true;
                }
                let mut buffer = Vec::new();
                for mut row in rows(&bytes)? {
                    for field in ["sessionId", "session_id"] {
                        if row.get(field).is_some() {
                            row[field] = json!(id);
                        }
                    }
                    if row.get("cwd").is_some() {
                        row["cwd"] = json!(cwd);
                    }
                    if let Some(payload) = row.get_mut("payload").and_then(Value::as_object_mut) {
                        if payload.contains_key("cwd") {
                            payload.insert("cwd".into(), json!(cwd));
                        }
                    }
                    serde_json::to_writer(&mut buffer, &row)?;
                    buffer.push(b'\n');
                    ensure!(buffer.len() <= BYTES, "rewritten_session_too_large");
                }
                bytes = buffer;
                let leaf = if file.path == "session.jsonl" {
                    format!("{id}.jsonl")
                } else {
                    format!("{id}/{}", file.path)
                };
                format!(".harness/claude/projects/{}/{leaf}", project_key(cwd))
            }
            "memory" => format!(
                ".harness/claude/projects/{}/memory/{}",
                project_key(cwd),
                file.path
            ),
            _ => file.path,
        };
        rewritten += bytes.len();
        ensure!(rewritten <= BYTES, "rewritten_session_too_large");
        out.push((destination, bytes));
    }
    ensure!(main, "missing_main_session");
    Ok(out)
}
fn write_private(root: &File, path: &str, bytes: &[u8]) -> Result<()> {
    let mut dir = root.try_clone()?;
    let mut parts = path.split('/').peekable();
    while let Some(name) = parts.next() {
        if parts.peek().is_some() {
            match fs::mkdirat(&dir, name, Mode::from_raw_mode(0o700)) {
                Ok(()) => {}
                Err(rustix::io::Errno::EXIST) => {}
                Err(e) => return Err(e.into()),
            }
            dir.sync_all()?;
            dir = open_at(&dir, Path::new(name), true)?;
        } else {
            let mut file: File = fs::openat(
                &dir,
                name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )?
            .into();
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        dir.sync_all()?;
    }
    Ok(())
}
fn remove_contents(dir: &File) -> Result<()> {
    for entry in fs::Dir::read_from(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let stat = fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if fs::FileType::from_raw_mode(stat.st_mode) == fs::FileType::Directory {
            let child = open_at(dir, Path::new(name.to_str()?), true)?;
            remove_contents(&child)?;
            fs::unlinkat(dir, name, AtFlags::REMOVEDIR)?;
        } else {
            fs::unlinkat(dir, name, AtFlags::empty())?;
        }
    }
    Ok(())
}
fn import(input: &Path, destination: &Path, id: &str, prepare: bool) -> Result<Value> {
    ensure!(
        input.is_absolute() && destination.is_absolute(),
        "absolute_path_required"
    );
    let (_, input_parent) = root(input.parent().context("input_parent_required")?)?;
    let bytes = read(
        &input_parent,
        input
            .file_name()
            .and_then(|s| s.to_str())
            .context("input_filename_required")?,
        INPUT,
    )?;
    let bundle: Bundle = serde_json::from_value(crate::bounded_json::decode(&bytes)?)?;
    let (parent_path, parent) = root(
        destination
            .parent()
            .context("destination_parent_required")?,
    )?;
    let name = destination
        .file_name()
        .and_then(|s| s.to_str())
        .context("destination_name_required")?;
    safe(name)?;
    let destination = parent_path.join(name);
    let cwd = destination.to_str().context("non_utf8_destination")?;
    // Validate every decoded and rewritten byte before creating anything.
    let mut files = decoded(bundle, id, cwd)?;
    if prepare {
        let native = format!(".harness/claude/projects/{}/{id}.jsonl", project_key(cwd));
        let bytes = &files
            .iter()
            .find(|(path, _)| *path == native)
            .context("native_session_missing")?
            .1;
        native_history(bytes, id)?;
        let plan = json!({"schema":"treer.claude-launch/v1","cwd":cwd,"sessionId":id,"nativeSha256":hash(bytes)});
        files.push((
            ".harness/claude-launch.json".into(),
            serde_json::to_vec(&plan)?,
        ));
    }
    ensure!(!exists(&parent, name)?, "destination_must_be_new");
    let stage_name = format!(".session-import-{}", uuid::Uuid::new_v4());
    fs::mkdirat(&parent, stage_name.as_str(), Mode::from_raw_mode(0o700))?;
    let stage = match open_at(&parent, Path::new(&stage_name), true) {
        Ok(stage) => stage,
        Err(error) => {
            fs::unlinkat(&parent, stage_name.as_str(), AtFlags::REMOVEDIR)
                .context("import_stage_initialization_cleanup_failed")?;
            return Err(error);
        }
    };
    let result = (|| -> Result<()> {
        for (path, bytes) in files {
            write_private(&stage, &path, &bytes)?;
        }
        stage.sync_all()?;
        let current = open_at(&parent, Path::new(&stage_name), true)?.metadata()?;
        let held = stage.metadata()?;
        ensure!(
            current.dev() == held.dev() && current.ino() == held.ino(),
            "import_stage_changed"
        );
        let (current_parent_path, current_parent) = root(&parent_path)?;
        let current_parent = current_parent.metadata()?;
        let held_parent = parent.metadata()?;
        ensure!(
            current_parent_path == parent_path
                && current_parent.dev() == held_parent.dev()
                && current_parent.ino() == held_parent.ino(),
            "import_parent_changed"
        );
        fs::renameat_with(
            &parent,
            stage_name.as_str(),
            &parent,
            name,
            fs::RenameFlags::NOREPLACE,
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        let current = open_at(&parent, Path::new(&stage_name), true)?.metadata()?;
        let held = stage.metadata()?;
        ensure!(
            current.dev() == held.dev() && current.ino() == held.ino(),
            "import_stage_cleanup_identity_changed"
        );
        remove_contents(&stage).context("import_stage_cleanup_failed")?;
        fs::unlinkat(&parent, stage_name.as_str(), AtFlags::REMOVEDIR)
            .context("import_stage_cleanup_failed")?;
        return Err(error);
    }
    // Publication has happened. Never delete the published destination on a
    // directory-sync error or encourage a retry with the same identity.
    parent
        .sync_all()
        .context("import_published_durability_unknown_do_not_retry")?;
    Ok(
        json!({"sessionId":id,"cwd":cwd,"environment":{"CLAUDE_CONFIG_DIR":destination.join(".harness/claude")}}),
    )
}
pub fn cli(args: Vec<String>) -> Result<()> {
    if args.first().map(String::as_str) == Some("--help") {
        println!("session export --harness claude --config-dir ABS --cwd ABS --session-id UUID\nsession import --input ABS --directory NEW_ABS [--session-id NEW_UUID]\nsession prepare --input ABS --directory NEW_ABS [--session-id NEW_UUID]\nsession launch --directory PREPARED_ABS --session-id NEW_UUID --auth environment\nsession run --directory PREPARED_ABS --session-id NEW_UUID --auth environment\nImport/prepare publish a new isolated directory. Launch needs a fresh private runtime root and explicit environment auth, never copies credentials or modifies the source session. Launch is once-only. Run is the persistent startup form: first bootstrap once, then only the same Agent/private checkpoint resumes. Failed intent without checkpoint is never retried. Normal Create saves this argv; process creation is not provider readiness.");
        return Ok(());
    }
    let operation = args.first().context("session_operation_required")?;
    ensure!(args.len() % 2 == 1, "session_options_require_values");
    let mut options = BTreeMap::new();
    let allowed = match operation.as_str() {
        "export" => &["--harness", "--config-dir", "--cwd", "--session-id"][..],
        "import" | "prepare" => &["--input", "--directory", "--session-id"][..],
        _ => bail!("unknown_session_operation"),
    };
    for pair in args[1..].chunks_exact(2) {
        ensure!(
            allowed.contains(&pair[0].as_str())
                && options.insert(pair[0].clone(), pair[1].clone()).is_none(),
            "unknown_or_duplicate_session_option"
        );
    }
    let required = |key: &str| {
        options
            .get(key)
            .map(String::as_str)
            .context("missing_session_option")
    };
    let value = if operation == "export" {
        ensure!(
            required("--harness")? == "claude",
            "only_claude_transfer_supported"
        );
        serde_json::to_value(export(
            Path::new(required("--config-dir")?),
            Path::new(required("--cwd")?),
            required("--session-id")?,
        )?)?
    } else {
        let id = options
            .get("--session-id")
            .cloned()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        import(
            Path::new(required("--input")?),
            Path::new(required("--directory")?),
            &id,
            operation == "prepare",
        )?
    };
    let output = (|| -> Result<()> {
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer(&mut stdout, &value)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
        Ok(())
    })();
    if matches!(operation.as_str(), "import" | "prepare") {
        output.context("import_published_output_failed_do_not_retry")?;
    } else {
        output?;
    }
    Ok(())
}
