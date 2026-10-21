//! Standalone diagnostic. No updater entry point or authority is linked here.
#[cfg(any(windows, test))]
#[path = "support/probe_report.rs"]
mod report;

#[cfg(windows)]
#[allow(unsafe_code)]
#[path = "support/breakaway_windows.rs"]
mod windows;

fn main() {
    #[cfg(windows)]
    std::process::exit(windows::run());
    #[cfg(not(windows))]
    {
        println!("{{\"schema\":1,\"result\":\"unsupported_platform\"}}");
        std::process::exit(1);
    }
}
