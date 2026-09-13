use super::*;

#[path = "handlers/admin.rs"]
mod admin;
#[path = "handlers/helpers.rs"]
mod helpers;
#[path = "handlers/middleware.rs"]
mod middleware;
#[path = "handlers/oauth.rs"]
mod oauth;
#[path = "handlers/organizations.rs"]
mod organizations;
#[path = "handlers/password_reset.rs"]
mod password_reset;
#[path = "handlers/session.rs"]
mod session;
#[path = "handlers/workspaces.rs"]
mod workspaces;

pub(crate) use admin::*;
pub(crate) use helpers::*;
pub(crate) use middleware::*;
pub(crate) use oauth::*;
pub(crate) use organizations::*;
pub(crate) use password_reset::*;
pub(crate) use session::*;
pub(crate) use workspaces::*;
