pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_COMMIT: &str = env!("TREER_BUILD_COMMIT");
pub const DISPLAY: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("TREER_BUILD_COMMIT"),
    ")"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_contains_version_and_commit() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        assert!(DISPLAY.contains(VERSION));
        assert!(DISPLAY.contains(GIT_COMMIT));
    }
}
