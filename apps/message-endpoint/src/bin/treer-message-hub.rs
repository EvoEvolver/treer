#[cfg(unix)]
fn main() -> std::process::ExitCode {
    use treer_message_endpoint::hub_runtime::{Config, Error};
    let run = || -> Result<(), Error> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.len() == 2 && args[0] == "--telegram-import-snapshot" {
            let result = treer_message_endpoint::telegram_snapshot_import::run_file(
                std::path::Path::new(&args[1]),
            )
            .map_err(|_| Error::Storage)?;
            println!(
                "{}",
                serde_json::to_string(&result).map_err(|_| Error::Storage)?
            );
            return Ok(());
        }
        if args.len() == 2 && args[0] == "--telegram-cutoff-journal" {
            let result =
                treer_message_endpoint::telegram_cutoff::run_file(std::path::Path::new(&args[1]))
                    .map_err(|_| Error::Storage)?;
            println!(
                "{}",
                serde_json::to_string(&result).map_err(|_| Error::Storage)?
            );
            return Ok(());
        }
        if args.len() != 2 || args[0] != "--config" {
            return Err(Error::Configuration);
        }
        let own = std::env::var("TREER_AGENT_ID").map_err(|_| Error::Configuration)?;
        let workspace = std::env::var("TREER_WORKSPACE_ID").ok();
        let config = Config::load(std::path::Path::new(&args[1]), &own, workspace.as_deref())?;
        treer_message_endpoint::hub_runtime::run(config)
    };
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("hub_platform_unsupported");
    std::process::ExitCode::FAILURE
}
