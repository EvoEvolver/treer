//! Workspace-shared Overview pins (#140): human-session routes under the Tree
//! authorisation rules. Reading needs Tree read; pinning and unpinning need the
//! same Tree manage action that moving or renaming uses. Responses and the
//! live signal carry stable IDs only, never names or paths.
use treer_protocol::{
    workspace_pin_id_ok, WorkspacePinAction, WorkspacePinKind, WorkspacePinsChanged,
    WorkspacePinsResponse, WORKSPACE_PINS_CHANGED_EVENT, WORKSPACE_PINS_LIMIT,
};

use super::workload_tree::authorize_tree;
use super::*;
use crate::policy::{ACTION_AGENT_TREE_MANAGE, ACTION_AGENT_TREE_READ};

fn parse_target(kind: &str, id: &str) -> Result<WorkspacePinKind, ApiFailure> {
    let kind = WorkspacePinKind::parse(kind).ok_or_else(|| {
        ApiFailure::bad_request("pin_kind_invalid", "kind must be agent, app or folder")
    })?;
    if !workspace_pin_id_ok(id) {
        return Err(ApiFailure::bad_request(
            "pin_id_invalid",
            "item ID has an unexpected shape",
        ));
    }
    Ok(kind)
}

/// `GET /api/workspaces/{ws}/pins`: Tree read. `can_pin` is a UI hint that
/// fails closed: any Policy error or denial reads as false.
pub(crate) async fn list_workspace_pins(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    headers: HeaderMap,
    Path(workspace): Path<String>,
) -> Result<Json<WorkspacePinsResponse>, ApiFailure> {
    authorize_tree(
        &state,
        &auth,
        &policy,
        Some(&session),
        None,
        &headers,
        &workspace,
        ACTION_AGENT_TREE_READ,
    )
    .await?;
    let can_pin = authorize_tree(
        &state,
        &auth,
        &policy,
        Some(&session),
        None,
        &headers,
        &workspace,
        ACTION_AGENT_TREE_MANAGE,
    )
    .await
    .is_ok();
    let pins = auth.workspace_pins(&workspace).await?;
    Ok(Json(WorkspacePinsResponse {
        pins,
        limit: WORKSPACE_PINS_LIMIT,
        can_pin,
        revision: state.workspace_revision(&workspace).await.unwrap_or(0),
    }))
}

async fn signal(
    state: &AppState,
    workspace: &str,
    action: WorkspacePinAction,
    kind: WorkspacePinKind,
    id: &str,
) {
    // Published after the commit; a lost signal is reconciled by the next GET.
    let changed = WorkspacePinsChanged {
        action,
        kind,
        id: id.to_owned(),
        revision: 0,
    };
    state
        .publish_workspace_signal(workspace, WORKSPACE_PINS_CHANGED_EVENT, move |revision| {
            serde_json::to_value(WorkspacePinsChanged {
                revision,
                ..changed
            })
            .unwrap_or(Value::Null)
        })
        .await;
}

/// `PUT /api/workspaces/{ws}/pins/{kind}/{id}`: Tree manage; idempotent.
pub(crate) async fn pin_workspace_item(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    headers: HeaderMap,
    Path((workspace, kind, id)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let kind = parse_target(&kind, &id)?;
    authorize_tree(
        &state,
        &auth,
        &policy,
        Some(&session),
        None,
        &headers,
        &workspace,
        ACTION_AGENT_TREE_MANAGE,
    )
    .await?;
    let (pin, created) = auth
        .pin_workspace_item(&workspace, kind, &id, &session.user_id)
        .await?;
    if created {
        signal(&state, &workspace, WorkspacePinAction::Pinned, kind, &id).await;
    }
    Ok(Json(serde_json::to_value(pin).unwrap_or(Value::Null)))
}

/// `DELETE /api/workspaces/{ws}/pins/{kind}/{id}`: Tree manage; always 204.
pub(crate) async fn unpin_workspace_item(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    headers: HeaderMap,
    Path((workspace, kind, id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiFailure> {
    let kind = parse_target(&kind, &id)?;
    authorize_tree(
        &state,
        &auth,
        &policy,
        Some(&session),
        None,
        &headers,
        &workspace,
        ACTION_AGENT_TREE_MANAGE,
    )
    .await?;
    if auth.unpin_workspace_item(&workspace, kind, &id).await? {
        signal(&state, &workspace, WorkspacePinAction::Unpinned, kind, &id).await;
    }
    Ok(StatusCode::NO_CONTENT)
}
