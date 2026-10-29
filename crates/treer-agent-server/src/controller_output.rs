use super::*;
pub(super) fn interactive_shell() -> String {
    #[cfg(windows)]
    return std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
    #[cfg(not(windows))]
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

/// Terminal bytes for a replay from `cursor`. When the Host ring has evicted
/// output right before the first retained chunk, that chunk can start inside
/// an escape sequence or UTF-8 character, which a fresh terminal would print
/// as garbage (or misparse what follows), so it starts at the first sequence
/// or line boundary instead.
pub(super) fn terminal_replay(
    replay: &HostOutputReplay,
    cursor: Option<&TerminalCursor>,
) -> Result<Vec<u8>, ProtocolError> {
    let resumed = cursor
        .filter(|cursor| cursor.stream_epoch == replay.stream_epoch)
        .map_or(0, |cursor| cursor.revision);
    let mut data = decode_replay(replay)?;
    if replay
        .chunks
        .first()
        .is_some_and(|chunk| chunk.revision > resumed.saturating_add(1))
    {
        let scan = &data[..data.len().min(REPLAY_BOUNDARY_SCAN_BYTES)];
        if let Some(start) = scan.iter().position(|byte| matches!(byte, 0x1b | b'\n')) {
            let start = if data[start] == b'\n' {
                start + 1
            } else {
                start
            };
            data.drain(..start);
        }
    }
    Ok(data)
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

/// One-shot status from retained text (Controller recovery); live output goes
/// through [`TerminalActivity`] instead.
pub(super) fn detect_status(text: &str) -> Option<AgentStatus> {
    let signals = tail_signals(text);
    if signals.blocked {
        Some(AgentStatus::Blocked)
    } else if signals.marker {
        Some(AgentStatus::Working)
    } else if signals.prompt {
        Some(AgentStatus::Idle)
    } else {
        None
    }
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

/// A Host error that names an absent stop target keeps its code
/// (`process_not_found`, `process_not_running`) so the Proxy can treat the
/// stop as complete; every other Host error stays `host_error` (#150).
pub(super) fn host_stop_error(error: impl std::fmt::Display) -> ProtocolError {
    let text = error.to_string();
    for code in ["process_not_found", "process_not_running"] {
        if text
            .strip_prefix(code)
            .is_some_and(|rest| rest.starts_with(':'))
        {
            return ProtocolError::new(code, text);
        }
    }
    protocol_error("host_error", text)
}

pub(super) fn startup_store_error(error: impl std::fmt::Display) -> ProtocolError {
    protocol_error("startup_store_error", error)
}
