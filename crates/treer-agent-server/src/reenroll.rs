//! `connect` on a machine that already has a local registration for the
//! enrolling workspace. A same-root, same-Proxy reconnect keeps the
//! registration and rotates the credential. A different root or Proxy, or an
//! old machine identity the Proxy no longer accepts, replaces the local
//! registration the way a reinstall would: the old Host and Controller stop and
//! the new configuration starts. Only when the old Host still runs workloads
//! does that need one confirmation (interactive y/N or `--replace`).
use std::future::Future;
use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Result};
use treer_protocol::MachineEnrollmentResponse;
use url::Url;

use crate::service::ServiceConfig;

/// Why the existing local registration cannot simply be reused.
pub(crate) fn replacement_reasons(
    installed: &ServiceConfig,
    requested_root: Option<&Path>,
    installed_proxy: &Url,
    proxy: &Url,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(root) = requested_root {
        let same = match (
            std::fs::canonicalize(root),
            std::fs::canonicalize(&installed.root),
        ) {
            (Ok(requested), Ok(current)) => requested == current,
            _ => false,
        };
        if !same {
            reasons.push(format!(
                "the root changes from {} to {}",
                installed.root.display(),
                root.display()
            ));
        }
    }
    if installed_proxy != proxy {
        reasons.push(format!(
            "the Proxy changes from {installed_proxy} to {proxy}"
        ));
    }
    reasons
}

/// How the caller agreed (or may be asked) to stop the old Host's workloads.
pub(crate) struct ReplaceConsent<'a, R, W> {
    /// `--replace`: stopping running workloads is already confirmed.
    pub replace: bool,
    /// `--non-interactive` or no terminal: never prompt.
    pub prompt: bool,
    pub input: &'a mut R,
    pub output: &'a mut W,
}

impl<R: BufRead, W: Write> ReplaceConsent<'_, R, W> {
    /// `workloads` is the old Host's running workload count, `None` when the
    /// Host is up but did not report it. Nothing running needs no question.
    pub fn confirm(&mut self, workloads: Option<usize>, reasons: &[String]) -> Result<()> {
        let why = reasons.join("; ");
        let stopping = match workloads {
            Some(0) => {
                writeln!(
                    self.output,
                    "treer: replacing the existing local registration ({why}); no workloads are running"
                )?;
                return Ok(());
            }
            Some(1) => "1 running workload".to_string(),
            Some(count) => format!("{count} running workloads"),
            None => {
                "the old Host is running but did not report its workloads; any running workloads"
                    .to_string()
            }
        };
        writeln!(
            self.output,
            "treer: connect replaces the existing local registration ({why}); {stopping} on the old Host will stop"
        )?;
        if self.replace {
            return Ok(());
        }
        if !self.prompt {
            bail!("{stopping} on the old Host would stop; rerun connect with --replace to confirm");
        }
        write!(
            self.output,
            "Stop them and replace the registration? [y/N] "
        )?;
        self.output.flush()?;
        let mut answer = String::new();
        self.input.read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            bail!("connect cancelled; the existing registration is unchanged");
        }
        Ok(())
    }
}

/// Outcome of claiming an enrollment key for an already registered workspace.
pub(crate) struct Claimed {
    pub response: MachineEnrollmentResponse,
    /// The old local registration must be stopped and replaced.
    pub replace: bool,
}

/// Claim the key for an existing registration. On the same Proxy the old
/// machine ID is offered first; if the Proxy no longer accepts that identity
/// (`machine_identity_conflict`), the key is claimed again as a fresh
/// enrollment (a failed claim does not use the key). Confirmation always
/// happens before the claim that can rotate a live machine's credential.
pub(crate) async fn claim_for_existing<C, F>(
    installed: &ServiceConfig,
    reasons: &[String],
    same_proxy: bool,
    workloads: Option<usize>,
    mut confirm: impl FnMut(Option<usize>, &[String]) -> Result<()>,
    mut claim: C,
) -> Result<Claimed>
where
    C: FnMut(Option<String>) -> F,
    F: Future<Output = Result<MachineEnrollmentResponse>>,
{
    let mut confirmed = false;
    if !reasons.is_empty() {
        confirm(workloads, reasons)?;
        confirmed = true;
    }
    let first = if same_proxy {
        claim(Some(installed.server_id.clone())).await
    } else {
        claim(None).await
    };
    let response = match first {
        Ok(response) => response,
        Err(error) if same_proxy && is_identity_conflict(&error) => {
            if !confirmed {
                let reason = [format!(
                    "the Proxy no longer accepts machine {} for this installation",
                    installed.server_id
                )];
                confirm(workloads, &reason)?;
                confirmed = true;
            }
            claim(None).await?
        }
        Err(error) => return Err(error),
    };
    let replace = confirmed || response.server_id != installed.server_id;
    Ok(Claimed { response, replace })
}

fn is_identity_conflict(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<crate::EnrollmentFailure>()
        .is_some_and(|failure| failure.code == "machine_identity_conflict")
}

/// Running workloads on the Host at `socket`: `Some(0)` when no Host listens,
/// `None` when a Host listens but does not answer a read-only sync in time.
pub(crate) async fn running_workloads(socket: &Path) -> Option<usize> {
    if !treer_platform::local_endpoint_active(socket) {
        return Some(0);
    }
    let count = async {
        let (host, _events) = crate::host_client::HostClient::connect(socket).await.ok()?;
        match host.sync(Default::default()).await.ok()? {
            treer_host_protocol::HostResponse::Synced { processes, .. } => {
                Some(processes.iter().filter(|process| process.running).count())
            }
            _ => None,
        }
    };
    tokio::time::timeout(Duration::from_secs(5), count)
        .await
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Cursor;
    use std::path::PathBuf;

    fn installed(root: PathBuf) -> ServiceConfig {
        ServiceConfig {
            proxy: "https://treer.example/".into(),
            workspace: "ws_fixture".into(),
            server_id: "srv_old".into(),
            machine_token: "srv_old.secret".into(),
            operator_credential: "opc_fixture".into(),
            root,
            listen: "127.0.0.1:9".into(),
            host_socket: PathBuf::from("/nonexistent/host.sock"),
            install_hostname: "fixture".into(),
            network_mode: None,
            service_manager: crate::service::ServiceManager::Nohup,
            service_fallback_reason: None,
        }
    }

    fn response(server_id: &str) -> MachineEnrollmentResponse {
        MachineEnrollmentResponse {
            workspace_id: "ws_fixture".into(),
            server_id: server_id.into(),
            machine_token: format!("{server_id}.new-secret"),
        }
    }

    fn conflict() -> anyhow::Error {
        anyhow::Error::new(crate::EnrollmentFailure {
            code: "machine_identity_conflict".into(),
            message: "the installed machine is bound to another installation identity".into(),
        })
    }

    fn directories() -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("reenroll-{}", uuid::Uuid::new_v4().simple()));
        let (old, new) = (base.join("old"), base.join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        (base, old, new)
    }

    fn proxy() -> Url {
        Url::parse("https://treer.example/").unwrap()
    }

    /// Run the claim with a non-interactive consent and a scripted Proxy.
    async fn run(
        config: &ServiceConfig,
        requested_root: Option<&Path>,
        workloads: Option<usize>,
        replace_flag: bool,
        proxy_answers: Vec<Result<MachineEnrollmentResponse>>,
    ) -> (Result<Claimed>, Vec<Option<String>>, String) {
        let reasons = replacement_reasons(config, requested_root, &proxy(), &proxy());
        let answers = RefCell::new(proxy_answers.into_iter());
        let offered = RefCell::new(Vec::new());
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let mut consent = ReplaceConsent {
            replace: replace_flag,
            prompt: false,
            input: &mut input,
            output: &mut output,
        };
        let result = claim_for_existing(
            config,
            &reasons,
            true,
            workloads,
            |count, reasons| consent.confirm(count, reasons),
            |existing| {
                offered.borrow_mut().push(existing);
                let answer = answers.borrow_mut().next().expect("unexpected claim");
                async move { answer }
            },
        )
        .await;
        (
            result,
            offered.into_inner(),
            String::from_utf8(output).unwrap(),
        )
    }

    #[tokio::test]
    async fn unchanged_reconnect_keeps_the_registration() {
        let (base, old, _) = directories();
        let config = installed(old.clone());
        let (result, offered, output) = run(
            &config,
            Some(&old),
            Some(3),
            false,
            vec![Ok(response("srv_old"))],
        )
        .await;
        let claimed = result.unwrap();
        assert!(
            !claimed.replace,
            "same root, Proxy and machine is a plain reconnect"
        );
        assert_eq!(offered, vec![Some("srv_old".to_string())]);
        assert!(
            output.is_empty(),
            "running workloads are not touched: {output}"
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn deleted_machine_reenroll_replaces_the_stale_registration() {
        // The machine was deleted in the Console and its identity now
        // conflicts; the user reruns the Console command with a new --root.
        let (base, old, new) = directories();
        let config = installed(old);
        let (result, offered, output) = run(
            &config,
            Some(&new),
            Some(0),
            false,
            vec![Err(conflict()), Ok(response("srv_new"))],
        )
        .await;
        let claimed = result.unwrap();
        assert!(claimed.replace);
        assert_eq!(claimed.response.server_id, "srv_new");
        assert_eq!(offered, vec![Some("srv_old".to_string()), None]);
        assert!(output.contains("no workloads are running"), "{output}");

        // Same root: the stale identity alone is enough to replace.
        let (result, offered, _) = run(
            &config,
            None,
            Some(0),
            false,
            vec![Err(conflict()), Ok(response("srv_new"))],
        )
        .await;
        assert!(result.unwrap().replace);
        assert_eq!(offered, vec![Some("srv_old".to_string()), None]);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn different_root_without_workloads_replaces_without_asking() {
        let (base, old, new) = directories();
        let config = installed(old);
        let (result, offered, output) = run(
            &config,
            Some(&new),
            Some(0),
            false,
            vec![Ok(response("srv_old"))],
        )
        .await;
        let claimed = result.unwrap();
        assert!(
            claimed.replace,
            "a new root replaces the registration in place"
        );
        assert_eq!(claimed.response.server_id, "srv_old");
        assert_eq!(offered, vec![Some("srv_old".to_string())]);
        assert!(
            !output.contains("[y/N]") && !output.contains("--replace"),
            "{output}"
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn running_workloads_require_replace_before_any_claim() {
        let (base, old, new) = directories();
        let config = installed(old);
        let (result, offered, _) = run(&config, Some(&new), Some(2), false, vec![]).await;
        let error = result.err().expect("refused without --replace").to_string();
        assert!(
            error.contains("2 running workloads") && error.contains("--replace"),
            "{error}"
        );
        assert!(
            offered.is_empty(),
            "the key must not be claimed before confirmation"
        );

        let (result, _, output) = run(
            &config,
            Some(&new),
            Some(2),
            true,
            vec![Ok(response("srv_old"))],
        )
        .await;
        assert!(result.unwrap().replace);
        assert!(
            output.contains("2 running workloads on the old Host will stop"),
            "{output}"
        );

        // A stale identity found only by the Proxy asks before the second claim.
        let (result, offered, _) = run(&config, None, Some(1), false, vec![Err(conflict())]).await;
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("1 running workload"));
        assert_eq!(offered, vec![Some("srv_old".to_string())]);

        // An unresponsive Host counts as running workloads.
        let (result, _, _) = run(&config, Some(&new), None, false, vec![]).await;
        assert!(result.err().unwrap().to_string().contains("--replace"));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn no_listening_host_has_no_running_workloads() {
        let socket =
            std::env::temp_dir().join(format!("reenroll-{}.sock", uuid::Uuid::new_v4().simple()));
        assert_eq!(running_workloads(&socket).await, Some(0));
    }

    #[test]
    fn interactive_confirmation_accepts_only_yes() {
        let reasons = ["the root changes".to_string()];
        for (answer, accepted) in [
            ("y\n", true),
            ("yes\n", true),
            ("\n", false),
            ("n\n", false),
        ] {
            let mut input = Cursor::new(answer.as_bytes().to_vec());
            let mut output = Vec::new();
            let result = ReplaceConsent {
                replace: false,
                prompt: true,
                input: &mut input,
                output: &mut output,
            }
            .confirm(Some(4), &reasons);
            assert_eq!(result.is_ok(), accepted, "answer {answer:?}");
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("4 running workloads") && output.contains("[y/N]"));
        }
    }

    #[test]
    fn proxy_change_is_a_replacement_reason() {
        let (base, old, _) = directories();
        let config = installed(old.clone());
        let other = Url::parse("https://other.example/").unwrap();
        let reasons = replacement_reasons(&config, Some(&old), &proxy(), &other);
        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].contains("other.example"));
        assert!(replacement_reasons(&config, Some(&old), &proxy(), &proxy()).is_empty());
        std::fs::remove_dir_all(base).unwrap();
    }
}
