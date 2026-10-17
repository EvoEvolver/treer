//! Fixed launcher invocation data, NOT an execution capability. There is no
//! spawn/Command API here. The executor still needs retained image proofs,
//! durable child-start/identity, strict completion and final live health.
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Component, Path, PathBuf},
};

use anyhow::{bail, Result};
use treer_platform::installation_directory::InstallationDirectory;

use super::{
    artifacts::Binary, handoff::OwnedActivation, launcher_preflight, selection::Selected, Journal,
    Receipt,
};

#[derive(Clone, Copy)]
pub enum Operation {
    Install,
    Status,
}

/// Borrows the original owner so installation/enrollment locks cannot be
/// released while this recipe exists. Intentionally not Debug/Serialize.
pub struct Invocation<'a> {
    selected: &'a Selected,
    owned: &'a OwnedActivation,
    journal: &'a Journal,
    receipt: Receipt,
    program: PathBuf,
    cwd: PathBuf,
    arguments: Vec<OsString>,
    environment: BTreeMap<OsString, OsString>,
}

impl<'a> Invocation<'a> {
    /// Resolve trusted Node from the launcher's fixed OS locations, never PATH
    /// or an Agent override. HOME comes from the effective OS account database;
    /// the selected enrollment supplies XDG_CONFIG_HOME even for custom roots.
    /// This does not run Node or authenticate an installed Controller image.
    pub fn prepare(
        selected: &'a Selected,
        owned: &'a OwnedActivation,
        journal: &'a Journal,
        operation: Operation,
    ) -> Result<Self> {
        launcher_preflight::checked(selected, owned, journal)?;
        let home = nix::unistd::User::from_uid(nix::unistd::geteuid())
            .ok()
            .flatten()
            .and_then(|user| user.dir.canonicalize().ok())
            .ok_or_else(|| anyhow::anyhow!("update_launcher_account_home_unavailable"))?;
        InstallationDirectory::open(&home)
            .map_err(|_| anyhow::anyhow!("update_launcher_account_home_unavailable"))?;
        let node = crate::launcher::node_executable(None)
            .map_err(|_| anyhow::anyhow!("update_launcher_node_unavailable"))?;
        Self::with_context(selected, owned, journal, operation, &home, &node)
    }

    // Private deterministic data-builder seam: production callers cannot
    // substitute an ambient home, Node path, environment, or arbitrary argv.
    fn with_context(
        selected: &'a Selected,
        owned: &'a OwnedActivation,
        journal: &'a Journal,
        operation: Operation,
        home: &Path,
        node: &Path,
    ) -> Result<Self> {
        let receipt = launcher_preflight::checked(selected, owned, journal)?;
        owned.locks().directory(selected, Binary::Controller)?;
        owned.locks().directory(selected, Binary::Cli)?;
        for path in [home, node] {
            if !path.is_absolute()
                || !path
                    .components()
                    .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
            {
                bail!("update_launcher_context_invalid");
            }
        }
        let url = selected.launcher_url()?;
        let arguments = vec![
            "launcher".into(),
            "--node".into(),
            node.as_os_str().to_owned(),
            "--treer".into(),
            selected.cli_path().into_os_string(),
            "--workspace".into(),
            selected.workspace().into(),
            "--url".into(),
            url.clone().into(),
            match operation {
                Operation::Install => "install",
                Operation::Status => "status",
            }
            .into(),
        ];
        // Construct from empty, never copy std::env::vars(). The future child
        // must call env_clear before applying this allowlist.
        let environment = [
            (OsString::from("HOME"), home.as_os_str().to_owned()),
            (
                "XDG_CONFIG_HOME".into(),
                selected.launcher_config_home()?.as_os_str().to_owned(),
            ),
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("LANG".into(), "C".into()),
            ("LC_ALL".into(), "C".into()),
            ("TREER_WORKSPACE_ID".into(), selected.workspace().into()),
            ("TREER_AGENT_SERVER_URL".into(), url.into()),
            ("TREER_SERVER_ID".into(), selected.server_id().into()),
        ]
        .into_iter()
        .collect();
        let program = selected.controller_path().to_owned();
        let cwd = program
            .parent()
            .ok_or_else(|| anyhow::anyhow!("update_launcher_context_invalid"))?
            .to_owned();
        Ok(Self {
            selected,
            owned,
            journal,
            receipt,
            program,
            cwd,
            arguments,
            environment,
        })
    }

    /// Only a journal/ownership binding check; never substitutes for retained
    /// image authentication or the durable child lifecycle at actual spawn.
    pub fn verify_binding(&self) -> Result<()> {
        if launcher_preflight::checked(self.selected, self.owned, self.journal)? != self.receipt {
            bail!("update_launcher_binding_mismatch");
        }
        self.owned
            .locks()
            .directory(self.selected, Binary::Controller)?;
        self.owned.locks().directory(self.selected, Binary::Cli)?;
        Ok(())
    }

    pub fn program(&self) -> &Path {
        &self.program
    }
    pub fn current_directory(&self) -> &Path {
        &self.cwd
    }
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
    pub fn environment(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.environment
            .iter()
            .map(|(key, value)| (key.as_os_str(), value.as_os_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::{
        publication::Publications, selection::tests::Fixture, tests::receipt, Phase,
    };
    use treer_platform::private_directory::PrivateDirectory;

    fn bind(f: &Fixture, workspace: &str) -> (Selected, OwnedActivation, Journal) {
        let selected = f.inventory().select(Some(workspace), true).unwrap();
        let locks = selected.lock().unwrap();
        let mut r = receipt();
        r.workspace_id = workspace.into();
        r.server_id = selected.server_id().into();
        r.publications = Publications::new(
            r.controller_sha256.clone(),
            r.cli_sha256.clone(),
            &r.controller_sha256,
            &r.cli_sha256,
        );
        let jobs = PrivateDirectory::open(&f.path)
            .unwrap()
            .create_child("jobs")
            .unwrap();
        let journal = Journal::create(&jobs, &r).unwrap();
        let prepared = journal.prepare_fixture().unwrap();
        let activating = journal
            .advance(prepared.revision, Phase::Activating)
            .unwrap();
        journal
            .advance(activating.revision, Phase::CoreHealthy)
            .unwrap();
        (selected, OwnedActivation::fixture(locks, prepared), journal)
    }

    #[test]
    fn fixed_operations_select_only_the_bound_enrollment_with_clean_environment() {
        use clap::Parser;
        let f = Fixture::standard_config();
        for (server, workspace) in [("srv_a", "ws_a"), ("srv_b", "ws_b")] {
            f.enrollment(server, workspace, &f.bin.join(Binary::Controller.name()));
        }
        let (selected, owned, journal) = bind(&f, "ws_b");
        let before = journal.read().unwrap();
        let home = f.path.join("account-home");
        let node = f.bin.join("trusted-node");
        for (operation, name) in [
            (Operation::Install, "install"),
            (Operation::Status, "status"),
        ] {
            let plan =
                Invocation::with_context(&selected, &owned, &journal, operation, &home, &node)
                    .unwrap();
            plan.verify_binding().unwrap();
            assert_eq!(plan.program(), selected.controller_path());
            assert_eq!(plan.current_directory(), f.bin);
            assert_eq!(
                plan.arguments(),
                [
                    OsString::from("launcher"),
                    "--node".into(),
                    node.as_os_str().to_owned(),
                    "--treer".into(),
                    f.bin.join("treer").into_os_string(),
                    "--workspace".into(),
                    "ws_b".into(),
                    "--url".into(),
                    "http://127.0.0.1:1234".into(),
                    name.into(),
                ]
            );
            let parsed = crate::Args::try_parse_from(
                std::iter::once(plan.program().as_os_str())
                    .chain(plan.arguments().iter().map(OsString::as_os_str)),
            )
            .unwrap();
            assert!(matches!(parsed.command, Some(crate::Command::Launcher(_))));
            let env: BTreeMap<_, _> = plan.environment().collect();
            assert_eq!(env.len(), 8);
            assert_eq!(env[OsStr::new("HOME")], home);
            assert_eq!(env[OsStr::new("XDG_CONFIG_HOME")], f.path.join("xdg"));
            assert_eq!(env[OsStr::new("TREER_WORKSPACE_ID")], "ws_b");
            assert_eq!(env[OsStr::new("TREER_SERVER_ID")], "srv_b");
            assert_eq!(
                env[OsStr::new("TREER_AGENT_SERVER_URL")],
                "http://127.0.0.1:1234"
            );
            assert_eq!(env[OsStr::new("PATH")], "/usr/bin:/bin:/usr/sbin:/sbin");
            assert_eq!(env[OsStr::new("LANG")], "C");
            assert_eq!(env[OsStr::new("LC_ALL")], "C");
        }
        assert_eq!(journal.read().unwrap(), before);
        assert!(!f.bin.join("trusted-node").exists()); // No child execution.
    }

    #[test]
    fn changed_journal_invalidates_an_existing_recipe() {
        let f = Fixture::standard_config();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        let plan = Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Install,
            &f.path,
            &f.bin.join("node"),
        )
        .unwrap();
        let r = journal.read().unwrap();
        journal.advance(r.revision, Phase::Partial).unwrap();
        assert_eq!(
            plan.verify_binding().unwrap_err().to_string(),
            "update_launcher_binding_mismatch"
        );
        assert!(Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Status,
            &f.path,
            &f.bin.join("node"),
        )
        .is_err());
    }

    #[test]
    fn same_phase_revision_change_also_invalidates_the_original_recipe() {
        let f = Fixture::standard_config();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        let plan = Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Status,
            &f.path,
            &f.bin.join("node"),
        )
        .unwrap();
        let mut changed = journal.read().unwrap();
        changed.revision += 1;
        // Test-only rewrite keeps the phase and immutable job fields valid.
        // An ownership/phase-only recheck would erroneously accept it.
        journal.write(&changed).unwrap();
        launcher_preflight::checked(&selected, &owned, &journal).unwrap();
        assert_eq!(
            plan.verify_binding().unwrap_err().to_string(),
            "update_launcher_binding_mismatch"
        );
    }

    #[test]
    fn nonstandard_config_layout_cannot_fall_back_to_ambient_config() {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        let result = Invocation::with_context(
            &selected,
            &owned,
            &journal,
            Operation::Install,
            &f.path,
            &f.bin.join("node"),
        );
        assert_eq!(
            result.err().unwrap().to_string(),
            "update_launcher_config_binding_mismatch"
        );
    }

    #[test]
    fn context_paths_must_be_absolute_without_parent_components() {
        let f = Fixture::standard_config();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let (selected, owned, journal) = bind(&f, "ws_a");
        for (home, node) in [
            (Path::new("relative-home"), f.bin.as_path()),
            (f.path.as_path(), Path::new("node")),
            (Path::new("/tmp/../home"), f.bin.as_path()),
        ] {
            let error = Invocation::with_context(
                &selected,
                &owned,
                &journal,
                Operation::Install,
                home,
                node,
            )
            .err()
            .unwrap();
            assert_eq!(error.to_string(), "update_launcher_context_invalid");
        }
    }
}
