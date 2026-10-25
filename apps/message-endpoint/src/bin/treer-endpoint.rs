#[cfg(unix)]
fn main() -> std::process::ExitCode {
    use std::{
        env,
        path::Path,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    use treer_message_endpoint::relay::{Config, Relay, StatusRenewal};
    let run = || -> Result<(), treer_message_endpoint::relay::Error> {
        let arguments: Vec<_> = env::args_os().skip(1).collect();
        if arguments.len() != 2 || arguments[0] != "--config" {
            return Err(treer_message_endpoint::relay::Error::Configuration);
        }
        let own_id = env::var("TREER_AGENT_ID")
            .map_err(|_| treer_message_endpoint::relay::Error::Configuration)?;
        let config = Config::load(Path::new(&arguments[1]), &own_id)?;
        let (mut relay, actual) = Relay::open(config, &own_id)?;
        let boot = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| treer_message_endpoint::relay::Error::Configuration)?
            .as_nanos();
        let boot_nonce = format!("{}-{boot}", std::process::id());
        let mut renewal = StatusRenewal::default();
        let mut budget_reported = false;
        loop {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|d| i64::try_from(d.as_secs()).ok());
            match now {
                Some(now) => {
                    if let Err(error) = relay.renew_binary(&actual, &boot_nonce, now, &mut renewal)
                    {
                        eprintln!("{error}");
                    }
                    if let Err(error) = relay.tick(now) {
                        eprintln!("{error}");
                        if matches!(error, treer_message_endpoint::relay::Error::FailureBudget)
                            && !budget_reported
                        {
                            budget_reported = relay.report_binary(&actual, &boot_nonce).is_ok();
                        }
                        std::thread::sleep(Duration::from_secs(2));
                    }
                }
                None => {
                    eprintln!("relay_clock_unavailable");
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
    };
    // No voluntary successful exit: command Agent exit 0 means completed.
    if let Err(error) = run() {
        eprintln!("{error}");
        std::thread::sleep(Duration::from_secs(
            treer_message_endpoint::relay::STARTUP_ERROR_BACKOFF_SECONDS,
        ));
    }
    std::process::ExitCode::FAILURE
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("relay_platform_unsupported");
    std::process::ExitCode::FAILURE
}
