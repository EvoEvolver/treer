pub fn session_ids_match(stored: &str, candidate: &str) -> bool {
    if stored == candidate {
        return true;
    }
    raw_session_id(stored) == raw_session_id(candidate)
}

pub fn raw_session_id(value: &str) -> &str {
    value
        .split_once("::")
        .map(|(_, rest)| rest)
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_raw_and_scoped_ids() {
        assert!(session_ids_match(
            "codex::01a0634a-23df-7191-acd2-1fca43a10418",
            "01a0634a-23df-7191-acd2-1fca43a10418"
        ));
        assert!(session_ids_match(
            "01a0634a-23df-7191-acd2-1fca43a10418",
            "codex::01a0634a-23df-7191-acd2-1fca43a10418"
        ));
    }
}
