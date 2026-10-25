//! Fixed internal observation diagnostics shared by producers and retry policy.
//! New diagnostics are permanent by default; adding one here does not make it
//! retryable until the activation policy explicitly includes it.
pub(super) const RUNNING_CONTROLLER_UNPROVEN: &str = "update_running_controller_unproven";
pub(super) const CONTROLLER_IDENTITY_CHANGED: &str = "update_controller_identity_changed";
pub(super) const CONTROLLER_IDENTITY_UNAVAILABLE: &str = "update_controller_identity_unavailable";
pub(super) const HEALTH_UNAVAILABLE: &str = "update_health_unavailable";
pub(super) const CONTROLLER_HEALTH_MISMATCH: &str = "update_controller_health_mismatch";
