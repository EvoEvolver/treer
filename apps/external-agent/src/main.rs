use treer_external_agent::{
    runtime::{App, Config},
    Error,
};
fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let (Some(path), None) = (args.next(), args.next()) else {
        return Err("usage: treer-external-agent CONFIG.json".into());
    };
    let c = Config::load(std::path::Path::new(&path))
        .map_err(|_| "config must be a valid private (0600, owner-only) JSON file".to_string())?;
    // Managed identity must match the configured fixed workload; never read operator configuration.
    for (key, want) in [
        ("TREER_WORKSPACE_ID", &c.workspace),
        ("TREER_AGENT_ID", &c.agent),
        ("TREER_SERVER_ID", &c.machine),
    ] {
        if std::env::var(key).ok().as_ref() != Some(want) {
            return Err(format!("{key} does not match the config"));
        }
    }
    let controller = std::env::var("TREER_AGENT_SERVER_URL")
        .ok()
        .and_then(|v| url::Url::parse(&v).ok());
    if controller.as_ref() != Some(&c.controller) {
        return Err("TREER_AGENT_SERVER_URL does not match config.controller".into());
    }
    let credential = std::env::var("TREER_WORKLOAD_CREDENTIAL")
        .map_err(|_| "TREER_WORKLOAD_CREDENTIAL is not set")?;
    let (address, owner_address) = (c.listen, c.owner_listen);
    let app = App::open(c, credential).map_err(|e| match e {
        Error::Denied => "Controller identity check failed (workspace/machine/agent mismatch or bad credential)".to_string(),
        Error::Storage(_) | Error::PrivateStorage | Error::Busy => {
            "state store unavailable (private 0700 directory, identity mismatch, or locked by another process)".to_string()
        }
        e => format!("startup failed: {e}"),
    })?;
    let listener =
        std::net::TcpListener::bind(address).map_err(|e| format!("bind {address}: {e}"))?;
    let owner_listener = std::net::TcpListener::bind(owner_address)
        .map_err(|e| format!("bind {owner_address}: {e}"))?;
    // Both sockets bind before either accepts. Losing management terminates this process.
    let owner_app = app.clone();
    std::thread::Builder::new()
        .name("owner-listener".into())
        .spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                owner_app.serve(owner_listener, true)
            }));
            std::process::exit(1);
        })
        .map_err(|e| e.to_string())?;
    // No request, header, token, prompt or downstream error logging.
    eprintln!("external-agent: listening on {address} (data) and {owner_address} (management)");
    app.serve(listener, false).map_err(|e| e.to_string())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("external-agent: {e}");
        std::process::exit(1);
    }
}
