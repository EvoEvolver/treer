//! Local input loading and internal receiver preparation; no public capability.
use anyhow::Result;

use super::catalog::Catalog;

pub(crate) mod coordinator;
pub(crate) mod preparation;

impl Catalog {
    #[allow(dead_code)] // Admission/coordinator wiring is a separate boundary.
    pub(crate) fn local() -> Result<Self> {
        let load = || -> Result<Self> {
            let root = crate::service::controller_update_config_directory()?;
            let bytes = treer_platform::windows_update_catalog::read_catalog(&root)?;
            Self::parse(&bytes)
        };
        load().map_err(|_| anyhow::anyhow!("controller_update_catalog_unavailable"))
    }
}
