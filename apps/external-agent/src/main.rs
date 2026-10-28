use treer_external_agent::{
    runtime::{App, CliGateway, Config, DataAdvertisement},
    Error,
};
fn run() -> Result<(), Error> {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().ok_or(Error::Invalid)?;
    let mut operator_path = None;
    let mut advertisement_path = None;
    let mut gateway_path = None;
    while let Some(flag) = args.next() {
        let target = if flag == "--data-advertisement" {
            &mut advertisement_path
        } else if flag == "--operator" {
            &mut operator_path
        } else if flag == "--cli-gateway" {
            &mut gateway_path
        } else {
            return Err(Error::Invalid);
        };
        if target.is_some() {
            return Err(Error::Invalid);
        }
        *target = Some(args.next().ok_or(Error::Invalid)?);
    }
    if operator_path.is_some() && (advertisement_path.is_some() || gateway_path.is_some()) {
        return Err(Error::Invalid);
    }
    let (c, digest) = Config::load_with_digest(std::path::Path::new(&path))?;
    let advertisement = advertisement_path
        .map(|path| DataAdvertisement::load(std::path::Path::new(&path), &c, &digest))
        .transpose()?;
    let gateway = gateway_path
        .map(|path| CliGateway::load(std::path::Path::new(&path), &c, &digest))
        .transpose()?;
    // Managed identity must match the configured fixed workload; never read operator configuration.
    for (key, want) in [
        ("TREER_WORKSPACE_ID", &c.workspace),
        ("TREER_AGENT_ID", &c.agent),
        ("TREER_SERVER_ID", &c.machine),
        ("TREER_AGENT_SERVER_URL", &c.controller.to_string()),
    ] {
        let actual = std::env::var(key).map_err(|_| Error::Denied)?;
        if key == "TREER_AGENT_SERVER_URL" {
            if url::Url::parse(&actual).ok().as_ref() != Some(&c.controller) {
                return Err(Error::Denied);
            }
        } else if actual != *want {
            return Err(Error::Denied);
        }
    }
    let credential = std::env::var("TREER_WORKLOAD_CREDENTIAL").map_err(|_| Error::Denied)?;
    if let Some(path) = operator_path {
        let receipt = treer_external_agent::runtime::operator::run(
            c,
            &digest,
            credential,
            std::path::Path::new(&path),
        )?;
        println!("{receipt}");
        return Ok(());
    }
    let address = c.listen;
    let owner_address = c.owner_listen;
    let app = App::open_gateway(c, credential, advertisement, gateway)?;
    let listener = std::net::TcpListener::bind(address).map_err(|_| Error::Invalid)?;
    let owner_listener = std::net::TcpListener::bind(owner_address).map_err(|_| Error::Invalid)?;
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
        .map_err(|_| Error::Invalid)?;
    // No request, header, token, prompt or downstream error logging.
    eprintln!(
        "external-agent: listening on {}",
        listener.local_addr().map_err(|_| Error::Invalid)?
    );
    app.serve(listener, false).map_err(|_| Error::Invalid)
}
fn main() {
    if run().is_err() {
        eprintln!("external-agent: startup or service unavailable");
        std::process::exit(1);
    }
}
