use super::*;
pub(super) fn interactive_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.trim().is_empty())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/bin/zsh".to_string()
            } else if std::path::Path::new("/bin/bash").is_file() {
                "/bin/bash".to_string()
            } else {
                "/bin/sh".to_string()
            }
        })
}

pub(super) fn shell_join(command: &str, args: &[String]) -> String {
    std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(super) fn encode_prompt_text(text: &str, bracketed_paste: bool) -> Vec<u8> {
    if !bracketed_paste {
        return text.as_bytes().to_vec();
    }
    let mut encoded = Vec::with_capacity(text.len() + 12);
    encoded.extend_from_slice(b"\x1b[200~");
    encoded.extend_from_slice(text.as_bytes());
    encoded.extend_from_slice(b"\x1b[201~");
    encoded
}

pub(super) fn decode_replay(replay: &HostOutputReplay) -> Result<Vec<u8>, ProtocolError> {
    let mut result = Vec::new();
    for chunk in &replay.chunks {
        result.extend_from_slice(&chunk.data);
    }
    Ok(result)
}

pub(super) fn plain_text(replay: &HostOutputReplay) -> Result<String, ProtocolError> {
    let raw = decode_replay(replay)?;
    Ok(String::from_utf8_lossy(&strip_ansi_escapes::strip(raw)).into_owned())
}

pub(super) fn trim_text(text: &mut String) {
    if text.len() <= OUTPUT_LIMIT_BYTES + OUTPUT_TRIM_SLACK_BYTES {
        return;
    }
    let mut split = text.len().saturating_sub(OUTPUT_LIMIT_BYTES);
    while split < text.len() && !text.is_char_boundary(split) {
        split += 1;
    }
    text.drain(..split);
}

pub(super) fn select_lines(text: &str, lines: Option<usize>) -> String {
    let Some(lines) = lines else {
        return text.to_string();
    };
    if lines == 0 {
        return String::new();
    }
    let line_count = text.lines().count();
    if line_count <= lines {
        text.to_string()
    } else {
        text.lines()
            .skip(line_count - lines)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub(super) fn detect_status(text: &str) -> Option<AgentStatus> {
    let text = recent_text(text, STATUS_SCAN_LIMIT_BYTES);
    let lower = text.to_lowercase();
    let blocked = [
        "allow command?",
        "do you want to proceed",
        "press enter to confirm",
        "enter to submit answer",
        "[y/n]",
        "action required",
    ];
    if blocked.iter().any(|pattern| lower.contains(pattern)) {
        return Some(AgentStatus::Blocked);
    }
    let working = [
        "esc to interrupt",
        "working (",
        "thinking (",
        "running tool",
    ];
    if working.iter().any(|pattern| lower.contains(pattern)) {
        return Some(AgentStatus::Working);
    }
    let last = text.lines().next_back().unwrap_or_default().trim_end();
    if last.ends_with('❯') || last.ends_with('›') || last.ends_with('$') || last.ends_with('#')
    {
        return Some(AgentStatus::Idle);
    }
    None
}

pub(super) fn recent_text(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut split = text.len() - limit;
    while split < text.len() && !text.is_char_boundary(split) {
        split += 1;
    }
    &text[split..]
}

pub(super) fn protocol_error(code: &str, error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new(code, error.to_string())
}

pub(super) fn startup_store_error(error: impl std::fmt::Display) -> ProtocolError {
    protocol_error("startup_store_error", error)
}

pub(super) fn agent_network_proxy_url(base: &str, agent_id: &str) -> String {
    let Ok(mut url) = url::Url::parse(base) else {
        return base.to_string();
    };
    if url.set_username(agent_id).is_err() || url.set_password(Some("treer")).is_err() {
        return base.to_string();
    }
    url.to_string()
}

pub(super) fn http_proxy_url(network_proxy_url: &str) -> String {
    let Ok(parsed) = url::Url::parse(network_proxy_url) else {
        return network_proxy_url.to_string();
    };
    let mut rewritten = String::from("http://");
    if !parsed.username().is_empty() {
        rewritten.push_str(parsed.username());
        if let Some(password) = parsed.password() {
            rewritten.push(':');
            rewritten.push_str(password);
        }
        rewritten.push('@');
    }
    match (parsed.host_str(), parsed.port()) {
        (Some(host), Some(port)) => {
            rewritten.push_str(host);
            rewritten.push(':');
            rewritten.push_str(&port.to_string());
        }
        (Some(host), None) => rewritten.push_str(host),
        _ => return network_proxy_url.to_string(),
    }
    rewritten
}

pub(super) fn network_environment(
    network_proxy_url: String,
    transparent: bool,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([("TREER_NETWORK_PROXY".to_string(), network_proxy_url.clone())]);
    if transparent {
        // The Controller's loopback is outside the agent network namespace.
        // Proxy-aware applications must use the TUN path instead of dialing it directly.
        for name in [
            "ALL_PROXY",
            "all_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
        ] {
            env.insert(name.to_string(), String::new());
        }
    } else {
        env.insert("ALL_PROXY".to_string(), network_proxy_url.clone());
        env.insert("all_proxy".to_string(), network_proxy_url.clone());
        let http_proxy = http_proxy_url(&network_proxy_url);
        // Plain HTTP proxy requests use absolute-form request targets, while this
        // listener intentionally implements CONNECT only. Let HTTP continue to
        // use the SOCKS5h ALL_PROXY path and reserve this URL for HTTPS tunnels.
        for name in ["HTTPS_PROXY", "https_proxy"] {
            env.insert(name.to_string(), http_proxy.clone());
        }
        env.insert("GIT_PROXY_COMMAND".to_string(), "treer".to_string());
        env.insert("TREER_GIT_PROXY_MODE".to_string(), "1".to_string());
        for name in ["NO_PROXY", "no_proxy"] {
            env.insert(name.to_string(), "127.0.0.1,localhost,::1".to_string());
        }
    }
    env
}
