//! Only fixed categories escape bounded, transient CLI diagnostics.
use super::*;
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Auth,
    Network,
    Other,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Auth => "message_auth: correct credentials and restart watcher",
            Self::Network => "message_network: Controller or Proxy unavailable",
            Self::Other => "message_other: operation failed",
        })
    }
}
impl std::error::Error for Failure {}
fn auth_code(code: &str) -> bool {
    matches!(
        code,
        "authentication_required"
            | "invalid_credentials"
            | "workload_credential_invalid"
            | "unauthorized"
            | "forbidden"
    )
}
pub(super) fn classify(bytes: &[u8]) -> Failure {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    // Structured command errors, never a bare occurrence in arbitrary prose.
    for line in text.lines().chain(std::iter::once(text.as_str())) {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if [v.get("code"), v.get("error").and_then(|e| e.get("code"))]
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(auth_code)
            {
                return Failure::Auth;
            }
        }
        let line = line
            .trim_start()
            .strip_prefix("treer: ")
            .unwrap_or(line.trim_start());
        if [
            "authentication_required",
            "invalid_credentials",
            "workload_credential_invalid",
        ]
        .iter()
        .any(|code| {
            line.strip_prefix(code).is_some_and(|tail| {
                tail.is_empty() || tail.starts_with(':') || tail.starts_with(char::is_whitespace)
            })
        }) {
            return Failure::Auth;
        }
        // Standard HTTP status diagnostics (including HTTP/1.1 and HTTP error).
        let tokens = line
            .split(|c: char| c.is_whitespace() || c == ':' || c == '=')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        if tokens
            .windows(2)
            .any(|p| (p[0] == "http" || p[0].starts_with("http/")) && matches!(p[1], "401" | "403"))
            || tokens
                .windows(3)
                .any(|p| p[0] == "http" && p[1] == "error" && matches!(p[2], "401" | "403"))
        {
            return Failure::Auth;
        }
    }
    if [
        "proxy_unavailable",
        "controller_unavailable",
        "econnrefused",
        "econnreset",
        "etimedout",
        "enotfound",
        "failed to connect",
        "connection refused",
    ]
    .iter()
    .any(|word| text.contains(word))
    {
        Failure::Network
    } else {
        Failure::Other
    }
}
pub(super) fn retry_delay(failures: u32, jitter: u16) -> Duration {
    let base = 1000u64 * (1u64 << failures.min(6));
    Duration::from_millis((base + base * u64::from(jitter) / (u64::from(u16::MAX) * 5)).min(60_000))
}
pub(super) async fn diagnostic_prefix(
    mut stderr: tokio::process::ChildStderr,
) -> std::io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut buf = [0; 4096];
    loop {
        let n = stderr.read(&mut buf).await?;
        if n == 0 {
            return Ok(kept);
        }
        let keep = n.min(16_384usize.saturating_sub(kept.len()));
        kept.extend_from_slice(&buf[..keep]);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_classification_never_formats_diagnostic_contents() {
        for s in [
            "treer: invalid_credentials: private-canary",
            "HTTP/1.1 403",
            "HTTP error: 401",
            r#"{"error":{"code":"forbidden","secret":"private-canary"}}"#,
        ] {
            let category = classify(s.as_bytes());
            assert_eq!(category, Failure::Auth);
            assert!(!category.to_string().contains("private-canary"));
        }
        assert_eq!(classify(b"ECONNREFUSED private-canary"), Failure::Network);
        assert_eq!(
            classify(b"the message mentions invalid_credentials incident"),
            Failure::Other
        );
        assert_eq!(classify(b"invalid_credentials_extra"), Failure::Other);
    }
    #[test]
    fn exponential_jitter_is_bounded_and_resets_with_failure_count() {
        for n in 0..100 {
            let lo = retry_delay(n, 0).as_millis();
            let hi = retry_delay(n, u16::MAX).as_millis();
            assert_eq!(lo, (1000u128 << n.min(6)).min(60_000));
            assert!(hi >= lo && hi <= (lo * 6 / 5).min(60_000));
        }
        assert_eq!(retry_delay(0, 0), Duration::from_secs(1));
    }
}
