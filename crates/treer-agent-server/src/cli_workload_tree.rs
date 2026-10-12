use super::*;
use treer_protocol::{AppDeployment, WorkloadFolder, WorkloadTree};

const TREE: &str = "api/workload-tree";

fn fail(code: &str, message: impl Into<String>) -> anyhow::Error {
    CliFailure {
        code: code.into(),
        message: message.into(),
    }
    .into()
}

// The caller's placement is the starting folder for relative tree paths.
fn current_folder<'a>(tree: &'a WorkloadTree, client: &ApiClient) -> Option<&'a str> {
    let caller = client.source_agent_id.as_deref()?;
    let folder = tree
        .placements
        .iter()
        .find(|p| p.agent_id == caller)?
        .folder_id
        .as_deref()?;
    tree.folders
        .iter()
        .any(|entry| entry.folder_id == folder)
        .then_some(folder)
}

fn components(tree: &WorkloadTree, path: &str, cwd: Option<&str>) -> anyhow::Result<Vec<String>> {
    if path.trim().is_empty() {
        return Err(fail("invalid_tree_path", "path cannot be empty"));
    }
    let mut parts = if path.starts_with('/') {
        Vec::new()
    } else {
        folder_path(tree, cwd)?
            .split('/')
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect()
    };
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            name if name.trim() != name || name.chars().any(char::is_control) => {
                return Err(fail(
                    "invalid_tree_path",
                    "path components must have visible names without surrounding whitespace",
                ));
            }
            name => parts.push(name.to_owned()),
        }
    }
    Ok(parts)
}

fn child<'a>(
    tree: &'a WorkloadTree,
    parent: Option<&str>,
    name: &str,
) -> anyhow::Result<Option<&'a WorkloadFolder>> {
    let matches: Vec<_> = tree
        .folders
        .iter()
        .filter(|f| f.parent_id.as_deref() == parent && f.name == name)
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [folder] => Ok(Some(folder)),
        _ => Err(fail(
            "folder_ambiguous",
            "folder name is duplicated; use its folder ID",
        )),
    }
}

fn resolve_folder(
    tree: &WorkloadTree,
    path: &str,
    cwd: Option<&str>,
) -> anyhow::Result<Option<String>> {
    if let Some(folder) = tree.folders.iter().find(|f| f.folder_id == path) {
        return Ok(Some(folder.folder_id.clone()));
    }
    let mut parent = None;
    for name in components(tree, path, cwd)? {
        parent = Some(
            child(tree, parent.as_deref(), &name)?
                .ok_or_else(|| fail("folder_not_found", format!("folder not found: {path}")))?
                .folder_id
                .clone(),
        );
    }
    Ok(parent)
}

fn folder_path(tree: &WorkloadTree, id: Option<&str>) -> anyhow::Result<String> {
    let mut parts = Vec::new();
    let mut next = id;
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = next {
        if !seen.insert(id) {
            return Err(fail("invalid_workload_tree", "folder cycle detected"));
        }
        let folder = tree
            .folders
            .iter()
            .find(|f| f.folder_id == id)
            .ok_or_else(|| {
                fail(
                    "folder_not_found",
                    "folder no longer exists; retry the command",
                )
            })?;
        parts.push(folder.name.as_str());
        next = folder.parent_id.as_deref();
    }
    parts.reverse();
    Ok(format!("/{}", parts.join("/")))
}

fn entry_path(parent: &str, name: &str) -> String {
    format!("{}/{}", parent.trim_end_matches('/'), name)
}

// Resolve before creation so a misspelled destination never starts a workload.
// Placement uses the existing Policy-checked tree API, without changing the
// create/launch wire contract or interpreting paths in the thin client.
pub(super) async fn create_in_folder(
    client: &ApiClient,
    requested: Option<&str>,
    endpoint: &str,
    request: Value,
) -> anyhow::Result<Value> {
    let destination = if requested.is_some() || client.source_agent_id.is_some() {
        let tree: WorkloadTree = client.request(Method::GET, TREE, None).await?;
        resolve_folder(
            &tree,
            requested.unwrap_or("."),
            current_folder(&tree, client),
        )?
    } else {
        None
    };
    let created = client.value(Method::POST, endpoint, Some(request)).await?;
    if let Some(folder) = destination {
        let agent_id = created["agent_id"].as_str().ok_or_else(|| {
            fail("agent_created_placement_failed", "Agent creation returned no agent_id; inspect the workspace before retrying creation")
        })?;
        client
            .value(
                Method::PUT,
                &format!("{TREE}/placements/{}", path_segment(agent_id)),
                Some(json!({"folder_id":folder,"position":i64::MAX})),
            )
            .await
            .map_err(|error| fail("agent_created_placement_failed", format!(
                "Agent {agent_id} was created, but folder placement failed: {error}. Do not repeat creation; retry with treer agent mv {agent_id} {folder}"
            )))?;
    }
    Ok(created)
}

pub(super) async fn mkdir(client: &ApiClient, path: &str, parents: bool) -> anyhow::Result<Value> {
    let mut tree: WorkloadTree = client.request(Method::GET, TREE, None).await?;
    let parts = components(&tree, path, current_folder(&tree, client))?;
    if parts.is_empty() {
        if parents {
            return Ok(json!({"path":"/", "folder":null}));
        }
        return Err(fail("folder_exists", "workspace root already exists"));
    }
    let mut parent = None;
    let mut result = None;
    for (index, name) in parts.iter().enumerate() {
        if let Some(folder) = child(&tree, parent.as_deref(), name)? {
            if index == parts.len() - 1 && !parents {
                return Err(fail("folder_exists", "folder already exists"));
            }
            result = Some(folder.clone());
            parent = Some(folder.folder_id.clone());
            continue;
        }
        if !parents && index != parts.len() - 1 {
            return Err(fail(
                "folder_not_found",
                "parent folder does not exist; use mkdir -p",
            ));
        }
        let response = client
            .value(
                Method::POST,
                &format!("{TREE}/folders"),
                Some(json!({"name":name,"parent_id":parent})),
            )
            .await;
        // Another caller may create the same parent after our initial read.
        if parents
            && response.as_ref().err().is_some_and(|error| {
                error
                    .downcast_ref::<CliFailure>()
                    .is_some_and(|failure| failure.code == "folder_exists")
            })
        {
            tree = client.request(Method::GET, TREE, None).await?;
            let folder = child(&tree, parent.as_deref(), name)?
                .ok_or_else(|| fail("folder_not_found", "concurrent folder change; retry mkdir"))?
                .clone();
            parent = Some(folder.folder_id.clone());
            result = Some(folder);
            continue;
        }
        let response = response?;
        let folder: WorkloadFolder = serde_json::from_value(response["folder"].clone())?;
        parent = Some(folder.folder_id.clone());
        result = Some(folder.clone());
        tree.folders.push(folder);
    }
    Ok(json!({"path":folder_path(&tree,parent.as_deref())?,"folder":result}))
}

#[derive(serde::Deserialize)]
struct Agents {
    agents: Vec<AgentInfo>,
}
#[derive(serde::Deserialize)]
struct Apps {
    apps: Vec<AppDeployment>,
}

// Include dormant Apps without duplicating the same workload from inventory.
async fn workloads(client: &ApiClient) -> anyhow::Result<Vec<Value>> {
    let (agents, apps) = tokio::try_join!(
        client.request::<Agents>(Method::GET, "api/agents", None),
        client.request::<Apps>(Method::GET, "api/apps", None)
    )?;
    let mut rows = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for agent in agents.agents {
        ids.insert(agent.agent_id.clone());
        rows.push(serde_json::to_value(agent)?);
    }
    for app in apps.apps {
        let id = app.runtime_agent_id.as_deref().unwrap_or(&app.app_id);
        if ids.insert(id.to_owned()) {
            rows.push(json!({"agent_id":id,"name":app.name,"kind":"app","status":app.status,"server_id":app.server_id}));
        }
    }
    Ok(rows)
}

pub(super) async fn ls(client: &ApiClient, path: &str, recursive: bool) -> anyhow::Result<Value> {
    let tree: WorkloadTree = client.request(Method::GET, TREE, None).await?;
    let parent = resolve_folder(&tree, path, current_folder(&tree, client))?;
    let base = folder_path(&tree, parent.as_deref())?;
    let mut folders = Vec::new();
    if let Some(id) = parent.as_deref() {
        let folder = tree
            .folders
            .iter()
            .find(|entry| entry.folder_id == id)
            .expect("resolved folder");
        let up = folder.parent_id.as_deref();
        let grandparent = up
            .and_then(|id| tree.folders.iter().find(|entry| entry.folder_id == id))
            .and_then(|entry| entry.parent_id.as_deref());
        folders.push(json!({
            "name":"..",
            "folder_id":up,
            "parent_id":grandparent,
            "path":folder_path(&tree,up)?
        }));
    }
    for folder in &tree.folders {
        if Some(folder.folder_id.as_str()) == parent.as_deref() {
            continue;
        }
        let full = entry_path(
            &folder_path(&tree, folder.parent_id.as_deref())?,
            &folder.name,
        );
        if folder.parent_id == parent || (recursive && within(&base, &full)) {
            let mut value = serde_json::to_value(folder)?;
            value["path"] = json!(full);
            folders.push(value);
        }
    }
    let mut agents = Vec::new();
    for mut agent in workloads(client).await? {
        let placement = tree
            .placements
            .iter()
            .find(|p| Some(p.agent_id.as_str()) == agent["agent_id"].as_str());
        let folder = placement.and_then(|p| p.folder_id.as_deref());
        let location = folder_path(&tree, folder)?;
        if folder == parent.as_deref() || (recursive && within(&base, &location)) {
            agent["path"] = json!(entry_path(
                &location,
                agent["name"].as_str().unwrap_or_default()
            ));
            agent["folder_id"] = json!(folder);
            agent["position"] = json!(placement.map(|p| p.position));
            agents.push(agent);
        }
    }
    folders.sort_by_key(|f| {
        (
            f["name"] != "..",
            f["path"].as_str().unwrap_or_default().to_owned(),
        )
    });
    agents.sort_by_key(|a| {
        (
            a["folder_id"].as_str().unwrap_or_default().to_owned(),
            a["position"].as_i64().unwrap_or(i64::MAX),
            a["agent_id"].as_str().unwrap_or_default().to_owned(),
        )
    });
    Ok(json!({"path":base,"folders":folders,"agents":agents}))
}

fn within(base: &str, path: &str) -> bool {
    base == "/"
        || path == base
        || path
            .strip_prefix(base)
            .is_some_and(|tail| tail.starts_with('/'))
}

pub(super) async fn rmdir(client: &ApiClient, path: &str) -> anyhow::Result<Value> {
    let tree: WorkloadTree = client.request(Method::GET, TREE, None).await?;
    let id = resolve_folder(&tree, path, current_folder(&tree, client))?
        .ok_or_else(|| fail("invalid_tree_path", "cannot remove workspace root"))?;
    client
        .value(
            Method::DELETE,
            &format!("{TREE}/folders/{}?require_empty=true", path_segment(&id)),
            None,
        )
        .await
}

pub(super) async fn mv(
    client: &ApiClient,
    source: &str,
    destination: &str,
) -> anyhow::Result<Value> {
    let tree: WorkloadTree = client.request(Method::GET, TREE, None).await?;
    let cwd = current_folder(&tree, client);
    // `self` selects the caller workload; `.` is the current tree folder.
    let source_folder = if source == "self" {
        None
    } else {
        match resolve_folder(&tree, source, cwd) {
            Ok(Some(id)) => Some(id),
            Ok(None) => return Err(fail("invalid_tree_path", "cannot move workspace root")),
            Err(error)
                if error
                    .downcast_ref::<CliFailure>()
                    .is_some_and(|e| e.code == "folder_not_found") =>
            {
                None
            }
            Err(error) => return Err(error),
        }
    };
    if let Some(id) = source_folder {
        let folder = tree
            .folders
            .iter()
            .find(|f| f.folder_id == id)
            .expect("resolved folder");
        let (parent, name) = match resolve_folder(&tree, destination, cwd) {
            Ok(existing) if existing.as_deref() == Some(&id) => {
                return Ok(json!({"folder":folder}))
            }
            Ok(existing) => (existing, folder.name.clone()),
            Err(error)
                if !destination.ends_with('/')
                    && error
                        .downcast_ref::<CliFailure>()
                        .is_some_and(|e| e.code == "folder_not_found") =>
            {
                let parts = components(&tree, destination, cwd)?;
                let name = parts
                    .last()
                    .ok_or_else(|| fail("invalid_tree_path", "cannot replace workspace root"))?
                    .to_string();
                let parent_path = format!("/{}", parts[..parts.len() - 1].join("/"));
                (resolve_folder(&tree, &parent_path, cwd)?, name)
            }
            Err(error) => return Err(error),
        };
        if child(&tree, parent.as_deref(), &name)?.is_some_and(|f| f.folder_id != id) {
            return Err(fail(
                "folder_exists",
                "destination already contains this folder name",
            ));
        }
        return client
            .value(
                Method::PATCH,
                &format!("{TREE}/folders/{}", path_segment(&id)),
                Some(json!({"name":name,"parent_id":parent})),
            )
            .await;
    }
    let destination = resolve_folder(&tree, destination, cwd)?;
    let agent = if source == "self" {
        client
            .source_agent_id
            .clone()
            .ok_or_else(|| fail("agent_not_found", "self requires an Agent identity"))?
    } else {
        let rows = workloads(client).await?;
        if let Some(id) = rows
            .iter()
            .find(|a| !source.contains('/') && a["agent_id"] == source)
            .and_then(|a| a["agent_id"].as_str())
        {
            id.to_owned()
        } else {
            let parts = components(&tree, source, cwd)?;
            let name = parts
                .last()
                .ok_or_else(|| fail("agent_not_found", "Agent path is empty"))?;
            let parent = resolve_folder(
                &tree,
                &format!("/{}", parts[..parts.len() - 1].join("/")),
                cwd,
            )?;
            let matches: Vec<_> = rows
                .iter()
                .filter(|a| {
                    let folder = tree
                        .placements
                        .iter()
                        .find(|p| Some(p.agent_id.as_str()) == a["agent_id"].as_str())
                        .and_then(|p| p.folder_id.as_deref());
                    a["name"].as_str() == Some(name.as_str()) && folder == parent.as_deref()
                })
                .collect();
            match matches.as_slice() {
                [agent] => agent["agent_id"]
                    .as_str()
                    .context("missing agent ID")?
                    .to_owned(),
                [] => return Err(fail("agent_not_found", "Agent not found at this path")),
                _ => {
                    return Err(fail(
                        "agent_ambiguous",
                        "multiple Agents at this path; use the Agent ID",
                    ))
                }
            }
        }
    };
    client
        .value(
            Method::PUT,
            &format!("{TREE}/placements/{}", path_segment(&agent)),
            Some(json!({"folder_id":destination,"position":i64::MAX})),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, extract::State, http::Request, routing::any, Json, Router};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    type Expected = (Method, String, Option<Value>, Value);

    async fn mock_request(
        State(expected): State<Arc<Mutex<std::collections::VecDeque<Expected>>>>,
        request: Request<Body>,
    ) -> Json<Value> {
        if request.headers().contains_key(AGENT_ID_HEADER) {
            assert_eq!(request.headers()[AGENT_ID_HEADER], "agent-caller");
            assert_eq!(
                request.headers()[WORKLOAD_CREDENTIAL_HEADER],
                "test-workload-credential"
            );
            assert!(!request.headers().contains_key(OPERATOR_CREDENTIAL_HEADER));
        } else {
            assert_eq!(
                request.headers()[OPERATOR_CREDENTIAL_HEADER],
                "test-operator-credential"
            );
            assert!(!request.headers().contains_key(WORKLOAD_CREDENTIAL_HEADER));
        }
        let mut pending = expected.lock().await;
        let index = pending
            .iter()
            .position(|(method, path, _, _)| {
                request.method() == method && request.uri().to_string() == *path
            })
            .expect("unexpected CLI request");
        let (method, path, body, response) = pending.remove(index).unwrap();
        drop(pending);
        assert_eq!(request.method(), method);
        assert_eq!(request.uri().to_string(), path);
        let bytes = axum::body::to_bytes(request.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            if bytes.is_empty() {
                None
            } else {
                Some(serde_json::from_slice::<Value>(&bytes).unwrap())
            },
            body
        );
        Json(response)
    }

    #[tokio::test]
    async fn directory_commands_use_authenticated_api_and_preserve_workload_identity() {
        let empty = json!({"folders":[],"placements":[]});
        let research = json!({"folder_id":"folder_root","name":"research","parent_id":null});
        let experiments =
            json!({"folder_id":"folder_child","name":"experiments","parent_id":"folder_root"});
        let tree = json!({"folders":[research,experiments],"placements":[]});
        let agent = json!({
            "agent_id":"agent-reviewer","workspace_id":"ws","server_id":"machine","kind":"command","name":"reviewer","cwd":".",
            "status":"idle","pid":null,"started_at":"2026-10-11T00:00:00Z","updated_at":"2026-10-11T00:00:00Z","exited_at":null,"exit_code":null,"output_revision":0
        });
        let app = json!({
            "app_id":"app-dormant","workspace_id":"ws","server_id":"machine","name":"docs","command":"serve","cwd":".","port":8080,"hostname":"","service_id":"svc-docs","desired_state":"stopped","status":"stopped",
            "created_at":"2026-10-11T00:00:00Z","updated_at":"2026-10-11T00:00:00Z","created_by":"owner","updated_by":"owner"
        });
        let expected: Arc<Mutex<std::collections::VecDeque<Expected>>> = Arc::new(Mutex::new(vec![
            (Method::GET,"/api/workload-tree".into(),None,empty),
            (Method::POST,"/api/workload-tree/folders".into(),Some(json!({"name":"research","parent_id":null})),json!({"folder":research})),
            (Method::POST,"/api/workload-tree/folders".into(),Some(json!({"name":"experiments","parent_id":"folder_root"})),json!({"folder":experiments})),
            // Repeating mkdir -p makes no writes.
            (Method::GET,"/api/workload-tree".into(),None,tree.clone()),
            (Method::GET,"/api/workload-tree".into(),None,tree.clone()),
            (Method::PATCH,"/api/workload-tree/folders/folder%5Fchild".into(),Some(json!({"name":"runs","parent_id":"folder_root"})),json!({"ok":true})),
            (Method::GET,"/api/workload-tree".into(),None,tree.clone()),
            (Method::GET,"/api/agents".into(),None,json!({"agents":[agent.clone()]})),
            (Method::GET,"/api/apps".into(),None,json!({"apps":[app.clone()]})),
            (Method::PUT,"/api/workload-tree/placements/agent%2Dreviewer".into(),Some(json!({"folder_id":"folder_child","position":i64::MAX})),json!({"placement":{"agent_id":"agent-reviewer","folder_id":"folder_child","position":0}})),
            (Method::GET,"/api/workload-tree".into(),None,tree.clone()),
            (Method::DELETE,"/api/workload-tree/folders/folder%5Fchild?require_empty=true".into(),None,json!({"ok":true})),
            (Method::GET,"/api/workload-tree".into(),None,json!({"folders":[research,experiments],"placements":[{"agent_id":"agent-reviewer","folder_id":"folder_child","position":0}]})),
            (Method::GET,"/api/agents".into(),None,json!({"agents":[agent]})),
            (Method::GET,"/api/apps".into(),None,json!({"apps":[app]})),
        ].into()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .fallback(any(mock_request))
            .with_state(expected.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = ApiClient {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            base: Url::parse(&format!("http://{address}/")).unwrap(),
            source_agent_id: Some("agent-caller".into()),
            workload_credential: Some("test-workload-credential".into()),
            operator_credential: None,
        };
        for argv in [
            vec!["treer", "agent", "mkdir", "-p", "/research/experiments"],
            vec!["treer", "agent", "mkdir", "-p", "research/experiments"],
            vec![
                "treer",
                "agent",
                "mv",
                "/research/experiments",
                "/research/runs",
            ],
            vec!["treer", "agent", "mv", "reviewer", "/research/experiments/"],
            vec!["treer", "agent", "rmdir", "/research/experiments"],
        ] {
            let args = Args::try_parse_from(argv).unwrap();
            let Some(Command::Agent { command }) = args.command else {
                panic!("Agent command")
            };
            run_agent_command(&client, command).await.unwrap();
        }
        let args = Args::try_parse_from(["treer", "agent", "ls", "-R", "/research"]).unwrap();
        let Some(Command::Agent { command }) = args.command else {
            panic!("Agent ls")
        };
        let listed = run_agent_command(&client, command).await.unwrap();
        assert_eq!(listed["folders"].as_array().unwrap().len(), 2);
        assert_eq!(listed["folders"][0]["name"], "..");
        assert_eq!(listed["folders"][0]["path"], "/");
        assert_eq!(listed["folders"][1]["path"], "/research/experiments");
        assert_eq!(listed["agents"].as_array().unwrap().len(), 1);
        assert_eq!(
            listed["agents"][0]["path"],
            "/research/experiments/reviewer"
        );
        assert!(expected.lock().await.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn agent_placement_sets_relative_directory_for_ls_mv_and_rmdir() {
        let folders = json!([
            {"folder_id":"research","name":"research","parent_id":null},
            {"folder_id":"experiments","name":"experiments","parent_id":"research"},
            {"folder_id":"runs","name":"runs","parent_id":"research"},
            {"folder_id":"empty","name":"empty","parent_id":"research"}
        ]);
        let tree = json!({"folders":folders,"placements":[
            {"agent_id":"agent-caller","folder_id":"experiments","position":0},
            {"agent_id":"agent-reviewer","folder_id":"experiments","position":1}
        ]});
        let agent = |id: &str, name: &str| {
            json!({
                "agent_id":id,"workspace_id":"ws","server_id":"machine","kind":"command","name":name,"cwd":".",
                "status":"idle","pid":null,"started_at":"2026-10-11T00:00:00Z","updated_at":"2026-10-11T00:00:00Z","exited_at":null,"exit_code":null,"output_revision":0
            })
        };
        let roster =
            json!({"agents":[agent("agent-caller","caller"),agent("agent-reviewer","reviewer")]});
        let mut requests = Vec::new();
        for _ in 0..6 {
            requests.push((Method::GET, "/api/workload-tree".into(), None, tree.clone()));
        }
        for _ in 0..4 {
            requests.push((Method::GET, "/api/agents".into(), None, roster.clone()));
            requests.push((Method::GET, "/api/apps".into(), None, json!({"apps":[]})));
        }
        requests.extend([
            (
                Method::PUT,
                "/api/workload-tree/placements/agent%2Dreviewer".into(),
                Some(json!({"folder_id":"runs","position":i64::MAX})),
                json!({"ok":true}),
            ),
            (
                Method::DELETE,
                "/api/workload-tree/folders/empty?require_empty=true".into(),
                None,
                json!({"ok":true}),
            ),
            (
                Method::POST,
                "/api/workload-tree/folders".into(),
                Some(json!({"name":"notes","parent_id":"experiments"})),
                json!({"folder":{"folder_id":"notes","name":"notes","parent_id":"experiments"}}),
            ),
        ]);
        let expected: Arc<Mutex<std::collections::VecDeque<Expected>>> =
            Arc::new(Mutex::new(requests.into()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn({
            let expected = expected.clone();
            async move {
                axum::serve(
                    listener,
                    Router::new()
                        .fallback(any(mock_request))
                        .with_state(expected),
                )
                .await
                .unwrap()
            }
        });
        let client = ApiClient {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            base: Url::parse(&format!("http://{address}/")).unwrap(),
            source_agent_id: Some("agent-caller".into()),
            workload_credential: Some("test-workload-credential".into()),
            operator_credential: None,
        };
        let here = ls(&client, ".", false).await.unwrap();
        assert_eq!(here["path"], "/research/experiments");
        assert_eq!(here["folders"][0]["name"], "..");
        assert_eq!(here["folders"][0]["path"], "/research");
        assert_eq!(here["agents"].as_array().unwrap().len(), 2);
        let parent = ls(&client, "..", false).await.unwrap();
        assert_eq!(parent["path"], "/research");
        assert_eq!(parent["folders"][0]["path"], "/");
        let root = ls(&client, "/", false).await.unwrap();
        assert_eq!(root["path"], "/");
        assert!(root["folders"]
            .as_array()
            .unwrap()
            .iter()
            .all(|folder| folder["name"] != ".."));
        mv(&client, "reviewer", "../runs/").await.unwrap();
        rmdir(&client, "../empty").await.unwrap();
        assert_eq!(
            mkdir(&client, "notes", false).await.unwrap()["path"],
            "/research/experiments/notes"
        );
        assert!(expected.lock().await.is_empty());
        server.abort();
    }

    #[test]
    fn tree_paths_resolve_hierarchy_ids_and_ambiguity() {
        let mut tree = WorkloadTree {
            folders: vec![
                WorkloadFolder {
                    folder_id: "one".into(),
                    name: "research".into(),
                    parent_id: None,
                },
                WorkloadFolder {
                    folder_id: "two".into(),
                    name: "experiments".into(),
                    parent_id: Some("one".into()),
                },
            ],
            placements: vec![],
        };
        assert_eq!(
            resolve_folder(&tree, "/research/./experiments/../experiments/", None).unwrap(),
            Some("two".into())
        );
        assert_eq!(
            resolve_folder(&tree, "two", None).unwrap(),
            Some("two".into())
        );
        assert_eq!(resolve_folder(&tree, "../../", None).unwrap(), None);
        assert_eq!(
            resolve_folder(&tree, "..", Some("two")).unwrap(),
            Some("one".into())
        );
        assert_eq!(
            resolve_folder(&tree, ".", Some("two")).unwrap(),
            Some("two".into())
        );
        assert_eq!(resolve_folder(&tree, "/", Some("two")).unwrap(), None);
        assert_eq!(
            resolve_folder(&tree, "../../../", Some("two")).unwrap(),
            None
        );
        assert_eq!(
            folder_path(&tree, Some("two")).unwrap(),
            "/research/experiments"
        );
        assert!(resolve_folder(&tree, "/missing", None).is_err());
        assert!(components(&tree, "/invalid /x", None).is_err());
        assert!(!within("/research", "/research2"));
        tree.folders.push(WorkloadFolder {
            folder_id: "duplicate".into(),
            name: "research".into(),
            parent_id: None,
        });
        assert!(resolve_folder(&tree, "/research", None).is_err());
    }

    #[tokio::test]
    async fn creation_inherits_tree_folder_or_resolves_explicit_destination_before_creation() {
        let tree = json!({"folders":[
            {"folder_id":"research","name":"research","parent_id":null},
            {"folder_id":"experiments","name":"experiments","parent_id":"research"},
            {"folder_id":"reviews","name":"reviews","parent_id":"research"}
        ],"placements":[{"agent_id":"agent-caller","folder_id":"experiments","position":0}]});
        for (managed, requested, destination, caller_folder) in [
            (true, None, Some("experiments"), Some("experiments")),
            (
                true,
                Some("../reviews"),
                Some("reviews"),
                Some("experiments"),
            ),
            (
                true,
                Some("/research"),
                Some("research"),
                Some("experiments"),
            ),
            (true, Some("reviews"), Some("reviews"), Some("experiments")), // folder ID
            (true, Some("/"), None, Some("experiments")),
            (false, None, None, None),
            (false, Some("research/reviews"), Some("reviews"), None),
            (true, Some("missing"), None, Some("experiments")),
            (true, None, None, None), // Caller at root stays at root.
        ] {
            let mut tree = tree.clone();
            tree["placements"][0]["folder_id"] = json!(caller_folder);
            let invalid = requested == Some("missing");
            let mut requests = Vec::new();
            if managed || requested.is_some() {
                requests.push((Method::GET, "/api/workload-tree".into(), None, tree.clone()));
            }
            if !invalid {
                requests.push((
                    Method::POST,
                    "/api/agents".into(),
                    Some(json!({"name":"child"})),
                    json!({"agent_id":"new-agent","cwd":"repo"}),
                ));
                if let Some(folder) = destination {
                    requests.push((
                        Method::PUT,
                        "/api/workload-tree/placements/new%2Dagent".into(),
                        Some(json!({"folder_id":folder,"position":i64::MAX})),
                        json!({"ok":true}),
                    ));
                }
            }
            let expected = Arc::new(Mutex::new(std::collections::VecDeque::from(requests)));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn({
                let expected = expected.clone();
                async move {
                    axum::serve(
                        listener,
                        Router::new()
                            .fallback(any(mock_request))
                            .with_state(expected),
                    )
                    .await
                    .unwrap()
                }
            });
            let client = ApiClient {
                http: reqwest::Client::builder().no_proxy().build().unwrap(),
                base: Url::parse(&format!("http://{address}/")).unwrap(),
                source_agent_id: managed.then(|| "agent-caller".into()),
                workload_credential: managed.then(|| "test-workload-credential".into()),
                operator_credential: (!managed).then(|| "test-operator-credential".into()),
            };
            let result =
                create_in_folder(&client, requested, "api/agents", json!({"name":"child"})).await;
            if invalid {
                assert_eq!(
                    result
                        .unwrap_err()
                        .downcast_ref::<CliFailure>()
                        .unwrap()
                        .code,
                    "folder_not_found"
                );
            } else {
                assert_eq!(
                    result.unwrap(),
                    json!({"agent_id":"new-agent","cwd":"repo"})
                );
            }
            assert!(expected.lock().await.is_empty());
            server.abort();
        }
    }

    #[tokio::test]
    async fn profile_launch_inherits_folder_and_reports_partial_creation_without_retrying() {
        let expected = Arc::new(Mutex::new(std::collections::VecDeque::from([
            (
                Method::GET,
                "/api/workload-tree".into(),
                None,
                json!({"folders":[{"folder_id":"research","name":"research","parent_id":null}],"placements":[{"agent_id":"agent-caller","folder_id":"research","position":0}]}),
            ),
            (
                Method::POST,
                "/api/launch-profiles/reviewer/launch".into(),
                Some(
                    serde_json::to_value(LaunchAgentProfileRequest {
                        server_id: None,
                        agent_name: None,
                        cwd: None,
                        cols: 120,
                        rows: 36,
                    })
                    .unwrap(),
                ),
                json!({"agent_id":"new-agent"}),
            ),
        ])));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn({
            let expected = expected.clone();
            async move {
                axum::serve(listener, Router::new()
                    .route("/api/workload-tree/placements/new%2Dagent", axum::routing::put(|| async {
                        (axum::http::StatusCode::FORBIDDEN, Json(json!({"error":{"code":"policy_denied","message":"placement denied"}})))
                    }))
                    .fallback(any(mock_request)).with_state(expected)).await.unwrap()
            }
        });
        let client = ApiClient {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            base: Url::parse(&format!("http://{address}/")).unwrap(),
            source_agent_id: Some("agent-caller".into()),
            workload_credential: Some("test-workload-credential".into()),
            operator_credential: None,
        };
        let error = run_profile_command(
            &client,
            ProfileCommand::Launch {
                target: "reviewer".into(),
                machine: None,
                name: None,
                cwd: None,
                folder: None,
            },
        )
        .await
        .unwrap_err();
        let failure = error.downcast_ref::<CliFailure>().unwrap();
        assert_eq!(failure.code, "agent_created_placement_failed");
        assert!(failure.message.contains("new-agent was created"));
        assert!(failure
            .message
            .contains("treer agent mv new-agent research"));
        assert!(failure.message.contains("Do not repeat creation"));
        assert!(expected.lock().await.is_empty());
        server.abort();
    }
}
