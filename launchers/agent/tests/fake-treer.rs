// Native executable fixture: no .cmd/shell shim and no real enrollment/login.
fn main() {
    let status = std::process::Command::new(std::env::var_os("FIXTURE_NODE").unwrap())
        .arg(std::env::var_os("FIXTURE_TREER_SCRIPT").unwrap())
        .args(std::env::args_os().skip(1))
        .status()
        .expect("sealed fake CLI failed");
    std::process::exit(status.code().unwrap_or(1));
}
