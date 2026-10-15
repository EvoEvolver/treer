//! Client-only presentation; authenticated Controller argv and JSON stay unchanged.
use anyhow::{bail, Context};
use serde_json::Value;

const MAX_BUFFER: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
enum View {
    Status,
    Agents,
    Machines,
}

pub(crate) struct Output {
    view: Option<View>,
    buffer: Vec<u8>,
}

impl Output {
    pub(crate) fn parse(raw: Vec<String>, tty: bool) -> anyhow::Result<(Vec<String>, Self)> {
        let mut args = Vec::new();
        let mut format = None;
        let mut iter = raw.into_iter().peekable();
        // Only transport options precede the Controller command.
        while let Some(arg) = iter.peek() {
            if matches!(arg.as_str(), "--url" | "--workspace") {
                args.push(iter.next().unwrap());
                args.push(iter.next().context("transport option requires a value")?);
            } else if arg.starts_with("--url=") || arg.starts_with("--workspace=") {
                args.push(iter.next().unwrap());
            } else if arg == "--format" || arg.starts_with("--format=") {
                read_format(iter.next().unwrap(), &mut iter, &mut format)?;
            } else {
                break;
            }
        }
        let mut command: Vec<_> = iter.collect();
        let view = match command.first().map(String::as_str) {
            Some("status") => Some(View::Status),
            Some("agent") if command.get(1).is_some_and(|s| s == "ls") => Some(View::Agents),
            Some("machine") if command.get(1).is_some_and(|s| s == "list") => Some(View::Machines),
            _ => None,
        };
        if view.is_some() {
            let mut remaining = command.into_iter();
            command = Vec::new();
            while let Some(arg) = remaining.next() {
                if arg == "--" {
                    command.push(arg);
                    command.extend(remaining);
                    break;
                }
                if arg == "--format" || arg.starts_with("--format=") {
                    read_format(arg, &mut remaining, &mut format)?;
                } else {
                    command.push(arg);
                }
            }
        }
        let table = match format.as_deref() {
            Some("table") => {
                anyhow::ensure!(
                    view.is_some(),
                    "--format table supports status, agent ls and machine list only"
                );
                true
            }
            Some("json") => false,
            None => tty,
            _ => unreachable!(),
        };
        args.extend(command);
        Ok((
            args,
            Self {
                view: view.filter(|_| table),
                buffer: Vec::new(),
            },
        ))
    }

    pub(crate) fn push(&mut self, data: Vec<u8>) -> Option<Vec<u8>> {
        if self.view.is_none() {
            return Some(data);
        }
        if data.len() > MAX_BUFFER.saturating_sub(self.buffer.len()) {
            // Oversized replies remain lossless streams instead of unbounded allocations.
            self.view = None;
            let mut bytes = std::mem::take(&mut self.buffer);
            bytes.extend(data);
            return Some(bytes);
        }
        self.buffer.extend(data);
        None
    }

    pub(crate) fn finish(&mut self, code: i32) -> Vec<u8> {
        let bytes = std::mem::take(&mut self.buffer);
        if code == 0 {
            if let Some(view) = self.view {
                if let Ok(value) = serde_json::from_slice(&bytes) {
                    if let Some(text) = render(view, &value) {
                        return text.into_bytes();
                    }
                }
            }
        }
        // Help, errors and future response schemas are never hidden or reinterpreted.
        bytes
    }
}

fn read_format(
    arg: String,
    iter: &mut impl Iterator<Item = String>,
    format: &mut Option<String>,
) -> anyhow::Result<()> {
    let value = match arg.strip_prefix("--format=") {
        Some(value) => value.to_owned(),
        None => iter.next().context("--format requires json or table")?,
    };
    if !matches!(value.as_str(), "json" | "table") {
        bail!("--format requires json or table");
    }
    anyhow::ensure!(format.is_none(), "--format may only be specified once");
    *format = Some(value);
    Ok(())
}

fn cell(value: &Value, key: &str) -> String {
    match &value[key] {
        Value::String(text) => clean(text),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        _ => "-".into(),
    }
}

fn clean(text: &str) -> String {
    let filtered: String = text.chars().filter(|c| {
        !c.is_control() && !matches!(*c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    }).take(65).collect();
    if filtered.chars().count() > 64 {
        format!("{}…", filtered.chars().take(63).collect::<String>())
    } else {
        filtered
    }
}

fn table(headers: &[&str], rows: Vec<Vec<String>>) -> String {
    let mut widths: Vec<_> = headers.iter().map(|s| s.len()).collect();
    for row in &rows {
        for (width, value) in widths.iter_mut().zip(row) {
            *width = (*width).max(value.chars().count());
        }
    }
    let mut result = String::new();
    for row in std::iter::once(headers.iter().map(|s| s.to_string()).collect()).chain(rows) {
        for (i, value) in row.iter().enumerate() {
            result.push_str(value);
            if i + 1 < row.len() {
                result.push_str(&" ".repeat(widths[i] - value.chars().count() + 2));
            }
        }
        result.push('\n');
    }
    result
}

fn machines(values: &[Value]) -> String {
    table(
        &["MACHINE", "ID", "STATUS", "HOST"],
        values
            .iter()
            .map(|v| {
                vec![
                    cell(v, "name"),
                    cell(v, "server_id"),
                    cell(v, "status"),
                    cell(v, "hostname"),
                ]
            })
            .collect(),
    )
}

fn agents(values: &[Value]) -> String {
    table(
        &["AGENT / PATH", "ID", "KIND", "STATUS", "MACHINE"],
        values
            .iter()
            .map(|v| {
                vec![
                    cell(
                        v,
                        if v["path"].is_string() {
                            "path"
                        } else {
                            "name"
                        },
                    ),
                    cell(v, "agent_id"),
                    cell(v, "kind"),
                    cell(v, "status"),
                    cell(v, "server_id"),
                ]
            })
            .collect(),
    )
}

fn render(view: View, value: &Value) -> Option<String> {
    Some(match view {
        View::Machines => machines(value.as_array()?),
        View::Agents => {
            let folders = value["folders"].as_array()?;
            let workloads = value["agents"].as_array()?;
            let mut result = format!("Folder: {}\n\n", cell(value, "path"));
            if !folders.is_empty() {
                result.push_str(&table(
                    &["FOLDER", "ID"],
                    folders
                        .iter()
                        .map(|v| vec![cell(v, "path"), cell(v, "folder_id")])
                        .collect(),
                ));
                result.push('\n');
            }
            result.push_str(&agents(workloads));
            result
        }
        View::Status => {
            let servers = value["servers"].as_array()?;
            let workloads = value["agents"].as_array()?;
            format!(
                "Workspace: {} ({})\n\nMachines ({})\n{}\nAgents ({})\n{}",
                cell(&value["workspace"], "name"),
                cell(&value["workspace"], "workspace_id"),
                servers.len(),
                machines(servers),
                workloads.len(),
                agents(workloads)
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str], tty: bool) -> (Vec<String>, Output) {
        Output::parse(args.iter().map(|s| s.to_string()).collect(), tty).unwrap()
    }

    #[test]
    fn tty_defaults_and_explicit_overrides_preserve_transport_and_literals() {
        for command in [vec!["status"], vec!["agent", "ls"], vec!["machine", "list"]] {
            assert!(parse(&command, true).1.view.is_some());
            assert!(parse(&command, false).1.view.is_none());
        }
        let (args, output) = parse(
            &[
                "--url",
                "http://localhost:1234",
                "--format=table",
                "--workspace=ws",
                "agent",
                "ls",
                "-R",
                "/research",
            ],
            false,
        );
        assert!(output.view.is_some());
        assert_eq!(
            args,
            [
                "--url",
                "http://localhost:1234",
                "--workspace=ws",
                "agent",
                "ls",
                "-R",
                "/research"
            ]
        );
        assert!(parse(&["status", "--format", "json"], true)
            .1
            .view
            .is_none());
        assert_eq!(
            parse(&["agent", "ls", "--", "--format=table"], false).0,
            ["agent", "ls", "--", "--format=table"]
        );
        assert_eq!(
            parse(&["exec", "--", "tool", "--format=table"], true).0,
            ["exec", "--", "tool", "--format=table"]
        );
    }

    #[test]
    fn invalid_formats_fail_before_dispatch() {
        for args in [
            vec!["--format", "csv", "status"],
            vec!["status", "--format"],
            vec!["--format=table", "exec"],
            vec!["--format=json", "status", "--format=json"],
        ] {
            assert!(Output::parse(args.into_iter().map(str::to_owned).collect(), false).is_err());
        }
    }

    #[test]
    fn tables_show_paths_folders_empty_lists_and_sanitized_cells() {
        let value = serde_json::json!({"path":"/research", "folders":[{"path":"/research/experiments", "folder_id":"fld_one"}], "agents":[{"path":"/research/reviewer\n\u{001b}[31m", "agent_id":"ag_one", "kind":"app", "status":"idle", "server_id":"srv_one"}]});
        let text = render(View::Agents, &value).unwrap();
        for expected in [
            "/research/experiments",
            "/research/reviewer",
            "fld_one",
            "ag_one",
            "app",
            "idle",
            "srv_one",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains('\u{001b}'));
        assert!(!text.contains("reviewer\n"));
        assert!(render(View::Machines, &serde_json::json!([]))
            .unwrap()
            .starts_with("MACHINE"));
        assert_eq!(clean(&"x".repeat(1000)).chars().count(), 64);
        assert_eq!(clean("a\u{202e}b\t"), "ab");
    }

    #[test]
    fn fragmented_json_renders_only_successful_complete_responses() {
        let (_, mut output) = parse(&["status"], true);
        for data in [
            br#"{"workspace":{"name":"demo","workspace_id":"ws_one"},"servers":[]"#.to_vec(),
            br#", "agents":[]}"#.to_vec(),
        ] {
            assert!(output.push(data).is_none());
        }
        let text = String::from_utf8(output.finish(0)).unwrap();
        assert!(text.contains("Workspace: demo (ws_one)"));
        assert!(text.contains("Machines (0)"));
        assert!(text.contains("Agents (0)"));
        for (bytes, code) in [
            (b"help\n".as_slice(), 0),
            (b"{\"future_schema\":true}", 0),
            (b"[]", 7),
        ] {
            let (_, mut output) = parse(&["machine", "list"], true);
            assert!(output.push(bytes.to_vec()).is_none());
            assert_eq!(output.finish(code), bytes);
        }
    }

    #[test]
    fn json_is_byte_preserving_and_large_replies_switch_to_lossless_streaming() {
        let (_, mut raw) = parse(&["status"], false);
        assert_eq!(raw.push(b"out\0\n".to_vec()).unwrap(), b"out\0\n");
        assert!(raw.finish(0).is_empty());
        let (_, mut table) = parse(&["status"], true);
        assert!(table.push(vec![b' '; MAX_BUFFER]).is_none());
        let bytes = table.push(b"end".to_vec()).unwrap();
        assert_eq!(bytes.len(), MAX_BUFFER + 3);
        assert!(bytes.ends_with(b"end"));
        assert_eq!(table.push(b"next".to_vec()).unwrap(), b"next");
        assert!(table.finish(0).is_empty());
    }
}
