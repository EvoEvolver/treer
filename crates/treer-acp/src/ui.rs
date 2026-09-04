use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::types::home_dir;

/// Default Host-wide thread UI git remote.
pub const DEFAULT_UI_GIT: &str = "https://github.com/dufangshi/remote-codex-thread-ui-rust.git";
pub const DEFAULT_UI_REF: &str = "main";
pub const UI_CHECKOUT_NAME: &str = "remote-codex-thread-ui-rust";
pub const INSTALL_RECORD_NAME: &str = "install.json";

/// Query string Treer iframes should append. AIS `ui_path` stays `/` so Proxy
/// asset tunneling keeps working; the control plane applies these later.
pub const TREER_EMBED_UI_QUERY: &str =
    "presentation=embedded-single-thread&explorer=1&shell=0&permissions=0&nav=0";

#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    pub git_url: Option<String>,
    pub git_ref: Option<String>,
    pub dir: Option<PathBuf>,
    pub ui_home: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiStatus {
    pub git: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub path: String,
    pub dist_path: Option<String>,
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_home: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_query: Option<String>,
}

pub fn resolve_ui_home(
    treer_ui_home: Option<PathBuf>,
    treer_host_root: Option<PathBuf>,
    cwd: Option<&Path>,
    user_home: &Path,
) -> PathBuf {
    if let Some(home) = treer_ui_home.filter(|path| !path.as_os_str().is_empty()) {
        return home;
    }
    if let Some(root) = treer_host_root.filter(|path| !path.as_os_str().is_empty()) {
        return root.join(".treer").join("ui");
    }
    if let Some(cwd) = cwd {
        for ancestor in cwd.ancestors() {
            let marker = ancestor.join(".treer").join("server-id");
            if marker.is_file() {
                return ancestor.join(".treer").join("ui");
            }
        }
    }
    user_home.join(".treer").join("ui")
}

pub fn ui_home() -> PathBuf {
    resolve_ui_home(
        env_path("TREER_UI_HOME"),
        env_path("TREER_HOST_ROOT"),
        std::env::current_dir().ok().as_deref(),
        &home_dir(),
    )
}

pub fn find_dist(checkout: &Path) -> Option<PathBuf> {
    let candidates = [
        checkout.join("apps/agent-ui-web/dist"),
        checkout.join("dist"),
        checkout.to_path_buf(),
    ];
    candidates
        .into_iter()
        .find(|path| path.join("index.html").is_file())
}

pub fn discover_installed_dist() -> Option<PathBuf> {
    discover_installed_dist_in(&ui_home())
}

pub fn discover_installed_dist_in(ui_home: &Path) -> Option<PathBuf> {
    if let Some(status) = read_install_record(ui_home) {
        if let Some(dist) = status
            .dist_path
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.join("index.html").is_file())
        {
            return Some(dist);
        }
        if let Some(dist) = find_dist(Path::new(&status.path)) {
            return Some(dist);
        }
    }
    find_dist(&ui_home.join(UI_CHECKOUT_NAME))
}

pub fn show(ui_home_override: Option<PathBuf>) -> Result<UiStatus> {
    let home = ui_home_override.unwrap_or_else(ui_home);
    if let Some(mut status) = read_install_record(&home) {
        status.installed = status
            .dist_path
            .as_deref()
            .map(Path::new)
            .is_some_and(|path| path.join("index.html").is_file());
        if !status.installed {
            if let Some(dist) = find_dist(Path::new(&status.path)) {
                status.dist_path = Some(dist.display().to_string());
                status.installed = true;
            }
        }
        status.ui_home = Some(home.display().to_string());
        status.embed_query = Some(TREER_EMBED_UI_QUERY.to_string());
        return Ok(status);
    }
    let path = home.join(UI_CHECKOUT_NAME);
    let dist = find_dist(&path);
    Ok(UiStatus {
        git: DEFAULT_UI_GIT.to_string(),
        git_ref: DEFAULT_UI_REF.to_string(),
        path: path.display().to_string(),
        dist_path: dist.as_ref().map(|path| path.display().to_string()),
        installed: dist.is_some(),
        ui_home: Some(home.display().to_string()),
        embed_query: Some(TREER_EMBED_UI_QUERY.to_string()),
    })
}

pub fn install(options: InstallOptions) -> Result<UiStatus> {
    let home = options.ui_home.clone().unwrap_or_else(ui_home);
    fs::create_dir_all(&home).with_context(|| format!("create UI home {}", home.display()))?;

    let git_ref = options
        .git_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_UI_REF)
        .to_string();

    let (git, path) = if let Some(dir) = options.dir.as_ref() {
        let path = dir
            .canonicalize()
            .with_context(|| format!("UI --dir {} does not exist", dir.display()))?;
        if !path.is_dir() {
            bail!("UI --dir {} is not a directory", path.display());
        }
        let git = options
            .git_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("")
            .to_string();
        (git, path)
    } else {
        let git = options
            .git_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_UI_GIT)
            .to_string();
        let path = home.join(UI_CHECKOUT_NAME);
        sync_git_checkout(&git, &git_ref, &path)?;
        (git, path)
    };

    let dist = ensure_dist(&path)?;
    let git_ref = if options.dir.is_some() && options.git_url.is_none() && options.git_ref.is_none()
    {
        "local".to_string()
    } else {
        git_ref
    };
    let status = UiStatus {
        git,
        git_ref,
        path: path.display().to_string(),
        dist_path: Some(dist.display().to_string()),
        installed: true,
        ui_home: Some(home.display().to_string()),
        embed_query: Some(TREER_EMBED_UI_QUERY.to_string()),
    };
    write_install_record(&home, &status)?;
    Ok(status)
}

fn ensure_dist(checkout: &Path) -> Result<PathBuf> {
    if let Some(dist) = find_dist(checkout) {
        return Ok(dist);
    }
    build_ui(checkout)?;
    find_dist(checkout).with_context(|| {
        format!(
            "UI dist is missing after build under {} (expected apps/agent-ui-web/dist, dist, or index.html)",
            checkout.display()
        )
    })
}

fn build_ui(checkout: &Path) -> Result<()> {
    if !checkout.join("package.json").is_file() {
        bail!(
            "UI checkout {} has no dist/index.html and no package.json to build",
            checkout.display()
        );
    }
    let pnpm = which::which("pnpm").context("pnpm is required to build the Host thread UI")?;
    run_cmd(&pnpm, &["install"], checkout)?;
    let package = fs::read_to_string(checkout.join("package.json")).unwrap_or_default();
    if package.contains("\"agent-ui:build\"") {
        run_cmd(&pnpm, &["run", "agent-ui:build"], checkout)?;
    } else {
        run_cmd(&pnpm, &["run", "build"], checkout)?;
    }
    Ok(())
}

fn sync_git_checkout(git_url: &str, git_ref: &str, dest: &Path) -> Result<()> {
    let git = which::which("git").context("git is required for `treer ui install`")?;
    if dest.join(".git").exists() {
        run_cmd(&git, &["fetch", "--tags", "--force", "origin"], dest)?;
        run_cmd(&git, &["checkout", "--force", git_ref], dest)?;
        if Command::new(&git)
            .args(["rev-parse", "--verify", &format!("origin/{git_ref}")])
            .current_dir(dest)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
        {
            run_cmd(
                &git,
                &["reset", "--hard", &format!("origin/{git_ref}")],
                dest,
            )?;
        }
        return Ok(());
    }
    if dest.exists() {
        let empty = dest.read_dir()?.next().is_none();
        if !empty {
            bail!(
                "UI checkout {} exists and is not a git repository; remove it or pass --dir",
                dest.display()
            );
        }
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let dest_name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .context("UI checkout path is missing a file name")?;
    let parent = dest.parent().unwrap_or(dest);
    run_cmd(&git, &["clone", "--", git_url, dest_name], parent)?;
    run_cmd(&git, &["checkout", "--force", git_ref], dest)?;
    Ok(())
}

fn run_cmd(program: &Path, args: &[&str], cwd: &Path) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .with_context(|| format!("run {} {}", program.display(), args.join(" ")))?;
    if !status.success() {
        bail!(
            "{} {} failed with {status} in {}",
            program.display(),
            args.join(" "),
            cwd.display()
        );
    }
    Ok(())
}

fn read_install_record(ui_home: &Path) -> Option<UiStatus> {
    let path = ui_home.join(INSTALL_RECORD_NAME);
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_install_record(ui_home: &Path, status: &UiStatus) -> Result<()> {
    let path = ui_home.join(INSTALL_RECORD_NAME);
    let body = serde_json::to_vec_pretty(status)?;
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ui_home_prefers_explicit_env_then_host_root_then_marker_then_user_home() {
        let user = PathBuf::from("/users/me");
        assert_eq!(
            resolve_ui_home(Some(PathBuf::from("/tmp/ui")), None, None, &user),
            PathBuf::from("/tmp/ui")
        );
        assert_eq!(
            resolve_ui_home(None, Some(PathBuf::from("/host")), None, &user),
            PathBuf::from("/host/.treer/ui")
        );

        let host = TempDir::new().unwrap();
        fs::create_dir_all(host.path().join(".treer")).unwrap();
        fs::write(host.path().join(".treer/server-id"), "srv_test\n").unwrap();
        let nested = host.path().join("proj/cwd");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            resolve_ui_home(None, None, Some(&nested), &user),
            host.path().join(".treer/ui")
        );
        assert_eq!(
            resolve_ui_home(None, None, None, &user),
            user.join(".treer/ui")
        );
    }

    #[test]
    fn install_from_local_dir_records_dist_and_show() {
        let root = TempDir::new().unwrap();
        let fixture = root.path().join("fixture");
        fs::create_dir_all(&fixture).unwrap();
        fs::write(fixture.join("index.html"), "<html>hello</html>").unwrap();
        let fixture = fixture.canonicalize().unwrap();
        let home = root.path().join("ui-home");

        let status = install(InstallOptions {
            dir: Some(fixture.clone()),
            ui_home: Some(home.clone()),
            ..InstallOptions::default()
        })
        .unwrap();
        assert!(status.installed);
        assert_eq!(status.git_ref, "local");
        assert_eq!(status.dist_path.as_deref(), Some(fixture.to_str().unwrap()));
        assert_eq!(
            discover_installed_dist_in(&home).as_deref(),
            Some(fixture.as_path())
        );

        let shown = show(Some(home)).unwrap();
        assert!(shown.installed);
        assert_eq!(shown.path, fixture.display().to_string());
    }

    fn git(cwd: &Path, args: &[&str]) {
        let git = which::which("git").expect("git");
        let status = Command::new(git)
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "Treer")
            .env("GIT_AUTHOR_EMAIL", "treer@example")
            .env("GIT_COMMITTER_NAME", "Treer")
            .env("GIT_COMMITTER_EMAIL", "treer@example")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn install_clones_a_local_git_checkout() {
        let root = TempDir::new().unwrap();
        let src = root.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("index.html"), "<html>from-git</html>").unwrap();
        git(&src, &["init"]);
        git(&src, &["add", "index.html"]);
        git(&src, &["commit", "-m", "ui"]);
        git(&src, &["branch", "-M", "main"]);

        let home = root.path().join("ui-home");
        let status = install(InstallOptions {
            git_url: Some(src.display().to_string()),
            git_ref: Some("main".into()),
            ui_home: Some(home.clone()),
            ..InstallOptions::default()
        })
        .unwrap();
        assert!(status.installed);
        assert_eq!(status.git_ref, "main");
        let dist = PathBuf::from(status.dist_path.unwrap());
        assert_eq!(
            fs::read_to_string(dist.join("index.html")).unwrap(),
            "<html>from-git</html>"
        );
        assert!(home.join(UI_CHECKOUT_NAME).join(".git").exists());
    }
}
