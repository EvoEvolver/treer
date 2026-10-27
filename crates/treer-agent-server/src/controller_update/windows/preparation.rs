//! Trusted saved-installation selection and signed local staging into the
//! sustained coordinator. No exposed dispatch, network fallback or activation.
use super::coordinator::Coordinator;
use crate::{
    controller::ControllerRuntime,
    windows_update_admission::{Documents, ReservedInputs},
    windows_update_installation::native::Selection,
    windows_update_worker_entry::Trust,
};
use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};

fn refused() -> anyhow::Error {
    anyhow!("windows_update_preparation_unconfirmed")
}

fn configured_trust(settings: &crate::UpdateArgs) -> Result<Trust> {
    let config = crate::service::controller_update_config_directory()?;
    let local = config
        .parent()
        .ok_or_else(refused)?
        .join("release-signing-key.pub.pem");
    Trust::configured(
        &settings.release_base_url,
        settings.public_key.as_deref(),
        &local,
    )
}

pub(crate) struct Preparation {
    coordinator: Coordinator,
    selection: Option<Selection>,
    poisoned: bool,
    downloaded: Option<crate::release_update::SignedDocuments>,
}
/// Both original operation custody and saved installation/configuration guards.
/// Not a release permit, retirement token or new execution permission.
#[allow(dead_code)] // Returned custody is for the subsequent transaction stage.
pub(crate) struct SelectedCustody {
    pub(crate) reservation: ReservedInputs,
    _selection: Selection,
}

#[allow(dead_code)] // Public capability/Policy route stays OFF.
impl Preparation {
    /// After normal identity/Policy and confirmation, use only normal local
    /// release settings. No locks/staging/claim exist while awaiting downloads.
    pub(crate) async fn download(
        runtime: &ControllerRuntime,
        dispatch: treer_protocol::controller_update_runtime::DispatchBinding,
        settings: &crate::UpdateArgs,
        expected_shared: &[String],
    ) -> Result<Self> {
        let download = async {
            let trust = configured_trust(settings)?;
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(120))
                .build()?;
            // Bound the complete six-object transaction, not just each request.
            let (documents, controller, cli) = tokio::time::timeout(
                std::time::Duration::from_secs(120),
                crate::release_update::download_inputs(
                    &client,
                    settings.release_base_url.clone(),
                    &settings.channel,
                    trust.verification_key(),
                    "windows-x86_64",
                ),
            )
            .await??;
            // Reload normal trust and validate the same original documents and
            // bytes again; a changed key cannot be silently adopted mid-flight.
            let mut prepared = Self::local(
                runtime,
                dispatch,
                settings,
                &documents.borrow(),
                (controller.bytes(), cli.bytes()),
                expected_shared,
            )?;
            prepared.downloaded = Some(documents);
            Ok(prepared)
        };
        download.await.map_err(|_: anyhow::Error| refused())
    }

    /// settings are normal local UpdateArgs, never Proxy/offer data. Signed
    /// documents/bytes may originate from the existing release downloader;
    /// independently configured trust authenticates them before staging.
    pub(crate) fn local(
        runtime: &ControllerRuntime,
        dispatch: treer_protocol::controller_update_runtime::DispatchBinding,
        settings: &crate::UpdateArgs,
        documents: &Documents<'_>,
        binaries: (&[u8], &[u8]),
        expected_shared: &[String],
    ) -> Result<Self> {
        let prepare = || -> Result<Self> {
            if documents.channel_name != settings.channel {
                return Err(refused());
            }
            let trust = configured_trust(settings)?;
            let release = crate::release_update::authenticate_documents(
                documents.channel,
                documents.channel_signature,
                documents.manifest,
                documents.manifest_signature,
                documents.channel_name,
                trust.verification_key(),
                "windows-x86_64",
            )?;
            for (bytes, artifact) in [
                (binaries.0, &release.controller),
                (binaries.1, &release.cli),
            ] {
                if bytes.len() != artifact.size
                    || format!("{:x}", Sha256::digest(bytes)) != artifact.sha256
                {
                    return Err(refused());
                }
            }
            let selection = Selection::local(&dispatch.workspace_id, &dispatch.server_id)?;
            selection.confirm(&dispatch, expected_shared)?;
            selection.check(runtime)?;
            let operation = uuid::Uuid::parse_str(&dispatch.operation_id)?;
            let state = runtime.windows_update_state_for_operation(
                operation,
                &dispatch.workspace_id,
                &dispatch.server_id,
            )?;
            let installed = selection.lock()?;
            selection.check(runtime)?;
            let (target, store) = treer_platform::windows_update_preparation::stage_pair(
                state.directory(),
                &operation.simple().to_string(),
                binaries.0,
                binaries.1,
            )?;
            selection.check(runtime)?;
            let coordinator = Coordinator::prepare(
                runtime, dispatch, installed, target, documents, store, trust,
            )?;
            Ok(Self {
                coordinator,
                selection: Some(selection),
                poisoned: false,
                downloaded: None,
            })
        };
        prepare().map_err(|_| refused())
    }
    fn check(&mut self, runtime: &ControllerRuntime) -> Result<()> {
        if self.poisoned {
            return Err(refused());
        }
        let result = self.selection.as_ref().ok_or_else(refused)?.check(runtime);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    pub(crate) fn shared(&self) -> Result<&[String]> {
        Ok(self.selection.as_ref().ok_or_else(refused)?.shared())
    }
    pub(crate) async fn compare_write_startup(
        &mut self,
        runtime: &ControllerRuntime,
        replacement: Option<&[u8]>,
    ) -> Result<()> {
        self.check(runtime)?;
        self.poisoned = true;
        self.coordinator
            .compare_write_startup(runtime, replacement)
            .await?;
        self.selection
            .as_ref()
            .ok_or_else(refused)?
            .check(runtime)?;
        self.poisoned = false;
        Ok(())
    }
    pub(crate) async fn compare_write_startup_fenced(
        &mut self,
        runtime: &ControllerRuntime,
        replacement: Option<&[u8]>,
    ) -> Result<()> {
        self.check(runtime)?;
        self.poisoned = true;
        self.coordinator
            .compare_write_startup_fenced(runtime, replacement)
            .await?;
        self.selection
            .as_ref()
            .ok_or_else(refused)?
            .check(runtime)?;
        self.poisoned = false;
        Ok(())
    }
    /// Explicit backup-only mutation while the SAME receiver remains Holding.
    pub(crate) async fn prepare_worker_backup(
        &mut self,
        runtime: &ControllerRuntime,
    ) -> Result<()> {
        self.check(runtime)?;
        self.poisoned = true;
        self.coordinator.prepare_worker_backup(runtime).await?;
        self.selection
            .as_ref()
            .ok_or_else(refused)?
            .check(runtime)?;
        self.poisoned = false;
        Ok(())
    }
    pub(crate) async fn prepare_worker_candidates(
        &mut self,
        runtime: &ControllerRuntime,
    ) -> Result<()> {
        self.check(runtime)?;
        self.poisoned = true;
        self.coordinator.prepare_worker_candidates(runtime).await?;
        self.selection
            .as_ref()
            .ok_or_else(refused)?
            .check(runtime)?;
        self.poisoned = false;
        Ok(())
    }
    /// Uses the retained, actually downloaded envelopes. ACK does not release
    /// any original sender pins/lease/claim or authorize installation mutation.
    pub(crate) async fn start_downloaded(&mut self, runtime: &ControllerRuntime) -> Result<()> {
        self.check(runtime)?;
        self.poisoned = true;
        let documents = self.downloaded.as_ref().ok_or_else(refused)?.borrow();
        self.coordinator.start(runtime, &documents).await?;
        self.selection
            .as_ref()
            .ok_or_else(refused)?
            .check(runtime)?;
        self.poisoned = false;
        Ok(())
    }
    pub(crate) async fn start(
        &mut self,
        runtime: &ControllerRuntime,
        documents: &Documents<'_>,
    ) -> Result<()> {
        self.check(runtime)?;
        self.poisoned = true;
        self.coordinator.start(runtime, documents).await?;
        self.selection
            .as_ref()
            .ok_or_else(refused)?
            .check(runtime)?;
        self.poisoned = false;
        Ok(())
    }
    pub(crate) async fn inspect(&mut self, runtime: &ControllerRuntime) -> Result<()> {
        self.check(runtime)?;
        self.poisoned = true;
        self.coordinator.inspect(runtime).await?;
        self.selection
            .as_ref()
            .ok_or_else(refused)?
            .check(runtime)?;
        self.poisoned = false;
        Ok(())
    }
    pub(crate) async fn close(&mut self, runtime: &ControllerRuntime) -> Result<()> {
        self.check(runtime)?;
        self.poisoned = true;
        self.coordinator.close(runtime).await?;
        self.selection
            .as_ref()
            .ok_or_else(refused)?
            .check(runtime)?;
        self.poisoned = false;
        Ok(())
    }
    pub(crate) fn retain_closed(&mut self, runtime: &ControllerRuntime) -> Result<SelectedCustody> {
        self.check(runtime)?;
        let reservation = self.coordinator.retain_closed(runtime)?;
        Ok(SelectedCustody {
            reservation,
            _selection: self.selection.take().ok_or_else(refused)?,
        })
    }
}
