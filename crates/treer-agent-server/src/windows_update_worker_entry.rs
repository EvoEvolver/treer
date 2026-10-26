//! Private input-receiver entry, not a transaction engine or lifecycle grant.
//! Trust is selected by normal local configuration before any offer is read.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use ed25519_dalek::{pkcs8::DecodePublicKey, VerifyingKey};
use uuid::Uuid;

pub(crate) const SWITCH: &str = "--controller-update-receive-inputs";
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const HOLD_SWITCH: &str = "--controller-update-hold-inputs";

fn refused() -> anyhow::Error {
    anyhow!("windows_update_worker_entry_refused")
}

enum Source {
    Official,
    Explicit(PathBuf),
}

/// Source selection is creator configuration, not wire data or a release
/// document. No Deserialize/Clone or caller-supplied public-key bytes.
pub(crate) struct Trust {
    source: Source,
    key: VerifyingKey,
}

#[allow(dead_code)] // Internal launch seam; product coordinator/cap remains unwired.
impl Trust {
    #[cfg(windows)]
    pub(crate) fn recheck(&self) -> Result<()> {
        if self.source.load()? != self.key {
            return Err(refused());
        }
        Ok(())
    }
    #[cfg(windows)]
    pub(crate) fn verification_key(&self) -> &VerifyingKey {
        &self.key
    }
    #[cfg(windows)]
    pub(crate) fn holding_arguments(
        &self,
        operation: Uuid,
        selector: Uuid,
        admitted_key: &[u8; 32],
    ) -> Result<Vec<OsString>> {
        let mut args = self.arguments(operation, selector, admitted_key)?;
        args[0] = HOLD_SWITCH.into();
        Ok(args)
    }
    /// Same resolver as the normal updater: explicit/local errors never fall
    /// back, only an absent local key on the exact official base uses the bundle.
    pub(crate) fn configured(
        base: &url::Url,
        explicit: Option<&Path>,
        local: &Path,
    ) -> Result<Self> {
        let key = crate::release_update::resolve_public_key(
            base,
            explicit,
            local,
            Some(crate::release_update::OFFICIAL_PUBLIC_KEY_PEM),
        )
        .map_err(|_| refused())?;
        let selected = if let Some(path) = explicit {
            Some(path)
        } else {
            match std::fs::symlink_metadata(local) {
                Ok(_) => Some(local),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err(refused()),
            }
        };
        let source = match selected {
            Some(path) => Source::Explicit(std::fs::canonicalize(path).map_err(|_| refused())?),
            None => Source::Official,
        };
        // A config/key race must refuse before process creation, not silently
        // substitute another trust source or rely on the child's offer.
        if source.load()? != key {
            return Err(refused());
        }
        Ok(Self { source, key })
    }

    #[cfg(any(windows, test))]
    pub(crate) fn arguments(
        &self,
        operation: Uuid,
        selector: Uuid,
        admitted_key: &[u8; 32],
    ) -> Result<Vec<OsString>> {
        crate::windows_update_handshake::bootstrap_arguments(
            [
                operation.to_string().into(),
                selector.simple().to_string().into(),
            ]
            .into_iter(),
        )
        .map_err(|_| refused())?;
        if self.key.to_bytes() != *admitted_key || self.source.load()? != self.key {
            return Err(refused());
        }
        let mut args = vec![
            SWITCH.into(),
            operation.to_string().into(),
            selector.simple().to_string().into(),
        ];
        match &self.source {
            Source::Official => args.push("--official".into()),
            Source::Explicit(path) => {
                args.push("--public-key".into());
                args.push(path.as_os_str().into());
            }
        }
        Ok(args)
    }
}

impl Source {
    fn load(&self) -> Result<VerifyingKey> {
        match self {
            Self::Official => {
                VerifyingKey::from_public_key_pem(crate::release_update::OFFICIAL_PUBLIC_KEY_PEM)
                    .map_err(|_| refused())
            }
            Self::Explicit(path) => {
                if !path.is_absolute() || path.as_os_str().to_string_lossy().len() > 32768 {
                    return Err(refused());
                }
                crate::release_update::load_public_key(path).map_err(|_| refused())
            }
        }
    }
}

/// Strict private argv: no implicit default directories/environment, offer key,
/// arbitrary PEM bytes, URL fetching or fallback after malformed input.
fn arguments(mut args: impl Iterator<Item = OsString>) -> Result<(Uuid, String, Source)> {
    let operation = args.next().ok_or_else(refused)?;
    let selector = args.next().ok_or_else(refused)?;
    let (operation, selector) =
        crate::windows_update_handshake::bootstrap_arguments([operation, selector].into_iter())
            .map_err(|_| refused())?;
    let source = match args.next().as_deref() {
        Some(value) if value == "--official" => Source::Official,
        Some(value) if value == "--public-key" => Source::Explicit(
            args.next()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .ok_or_else(refused)?,
        ),
        _ => return Err(refused()),
    };
    if args.next().is_some() {
        return Err(refused());
    }
    Ok((operation, selector, source))
}

#[cfg(windows)]
pub(crate) fn run(args: impl Iterator<Item = OsString>, hold: bool) -> Result<()> {
    let (operation, selector, source) = arguments(args)?;
    // Resolve the independently selected local/bundled key BEFORE pipe/offer.
    let key = source.load()?;
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|_| refused())?
        .block_on(async {
            let mut received =
                crate::windows_update_admission::worker_inputs::native::receive_creator_inputs(
                    &selector,
                    *operation.as_bytes(),
                    &key,
                )
                .await
                .map_err(|_| refused())?;
            // Terminal finite input check only. Custody remains owned through
            // this check and explicit drop. Receipt/exit never release sender
            // pins or authorize any publication, restart, recovery or readiness.
            received.inspect_snapshot().map_err(|_| refused())?;
            if hold {
                return received.serve_custody().await.map_err(|_| refused());
            }
            drop(received);
            Ok(())
        })
}

#[cfg(test)]
#[path = "windows_update_worker_entry_tests.rs"]
mod tests;
