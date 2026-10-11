use super::*;
use treer_protocol::{
    CreateWorkloadFolderRequest, SetWorkloadPlacementRequest, UpdateWorkloadFolderRequest,
};

pub(super) async fn get_workload_tree(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    auth.require_workspace_member(&workspace, &session.user_id)
        .await?;
    Ok(Json(json!(auth.workload_tree(&workspace).await?)))
}
pub(super) async fn create_workload_folder(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace): Path<String>,
    Json(request): Json<CreateWorkloadFolderRequest>,
) -> Result<Json<Value>, ApiFailure> {
    auth.require_workspace_member(&workspace, &session.user_id)
        .await?;
    Ok(Json(
        json!({"folder":auth.create_workload_folder(&workspace,request).await?}),
    ))
}
pub(super) async fn update_workload_folder(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace, id)): Path<(String, String)>,
    Json(request): Json<UpdateWorkloadFolderRequest>,
) -> Result<Json<Value>, ApiFailure> {
    auth.require_workspace_member(&workspace, &session.user_id)
        .await?;
    Ok(Json(
        json!({"folder":auth.update_workload_folder(&workspace,&id,request).await?}),
    ))
}
pub(super) async fn delete_workload_folder(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    auth.require_workspace_member(&workspace, &session.user_id)
        .await?;
    auth.delete_workload_folder(&workspace, &id).await?;
    Ok(Json(json!({"ok":true})))
}
pub(super) async fn set_workload_placement(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace, id)): Path<(String, String)>,
    Json(request): Json<SetWorkloadPlacementRequest>,
) -> Result<Json<Value>, ApiFailure> {
    auth.require_workspace_member(&workspace, &session.user_id)
        .await?;
    Ok(Json(
        json!({"placement":auth.set_workload_placement(&workspace,&id,request).await?}),
    ))
}
