use treer_protocol::AgentTranscriptEntry;

pub fn is_user_prompt_entry(entry: &AgentTranscriptEntry) -> bool {
    entry.kind == "message" && entry.role.as_deref() == Some("user")
}

pub fn group_transcript_turns(entries: &[AgentTranscriptEntry]) -> Vec<Vec<AgentTranscriptEntry>> {
    let mut turns = Vec::new();
    let mut current = Vec::new();
    let mut seen_user = false;
    for entry in entries {
        if is_user_prompt_entry(entry) && seen_user {
            turns.push(current);
            current = vec![entry.clone()];
        } else {
            current.push(entry.clone());
            if is_user_prompt_entry(entry) {
                seen_user = true;
            }
        }
    }
    if !current.is_empty() {
        turns.push(current);
    }
    turns
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptPage {
    pub page: u32,
    pub page_count: u32,
    pub next_page: Option<u32>,
    pub cursor: String,
    pub next_cursor: Option<String>,
    pub entries: Vec<AgentTranscriptEntry>,
}

pub fn page_turns(turns: &[Vec<AgentTranscriptEntry>], page: u32, limit: u32) -> TranscriptPage {
    let start = page as usize;
    let count = limit.clamp(1, 1000) as usize;
    let selected = if start >= turns.len() {
        &[][..]
    } else {
        let end = (start + count).min(turns.len());
        &turns[start..end]
    };
    let next_page = if start + selected.len() < turns.len() {
        Some((start + selected.len()) as u32)
    } else {
        None
    };
    TranscriptPage {
        page,
        page_count: turns.len() as u32,
        next_page,
        cursor: page.to_string(),
        next_cursor: next_page.map(|value| value.to_string()),
        entries: selected.iter().flatten().cloned().collect(),
    }
}

pub fn transcript_page_from_entries(
    entries: &[AgentTranscriptEntry],
    page: u32,
    limit: u32,
) -> TranscriptPage {
    page_turns(&group_transcript_turns(entries), page, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, role: Option<&str>) -> AgentTranscriptEntry {
        AgentTranscriptEntry {
            id: id.into(),
            kind: "message".into(),
            role: role.map(str::to_string),
            content: serde_json::json!(id),
            created_at: None,
        }
    }

    #[test]
    fn groups_and_pages_turns() {
        let entries = vec![
            entry("u1", Some("user")),
            entry("a1", Some("assistant")),
            entry("u2", Some("user")),
            entry("a2", Some("assistant")),
        ];
        let first = transcript_page_from_entries(&entries, 0, 1);
        assert_eq!(first.page_count, 2);
        assert_eq!(first.next_page, Some(1));
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["u1", "a1"]
        );
        let second = transcript_page_from_entries(&entries, 1, 1);
        assert_eq!(second.next_page, None);
        assert_eq!(
            second
                .entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["u2", "a2"]
        );
    }
}
