use std::io::{IsTerminal, Stdin, Stdout};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use reqwest::Client;
use serde::Deserialize;
use treer_protocol::{AgentInfo, AgentStatus, OPERATOR_CREDENTIAL_HEADER};

use crate::service;

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(900);

#[derive(Debug, Deserialize)]
struct Health {
    workspace_id: String,
    server_id: String,
    controller_epoch: String,
    #[serde(default)]
    proxy_connected: bool,
    #[serde(default)]
    connection_state: Option<String>,
    #[serde(default)]
    proxy_last_error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AgentList {
    agents: Vec<AgentInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirmation {
    Stop,
    Restart,
}

impl Confirmation {
    fn prompt(self) -> &'static str {
        match self {
            Self::Stop => "Stop the Host service? This terminates all Agents and PTYs.",
            Self::Restart => "Restart the Host service? This terminates all Agents and PTYs.",
        }
    }
}

struct App {
    workspace: String,
    client: Client,
    address: Option<SocketAddr>,
    health: Option<Health>,
    proxy_reachable: bool,
    service_manager: Option<service::ServiceManager>,
    service_fallback_reason: Option<String>,
    agents: Vec<AgentInfo>,
    table_state: TableState,
    message: String,
    last_refresh: Option<Instant>,
    confirmation: Option<Confirmation>,
    show_help: bool,
}

impl App {
    fn new(workspace: &str) -> Result<Self> {
        Ok(Self {
            workspace: workspace.to_string(),
            client: Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .no_proxy()
                .build()
                .context("failed to create local TUI client")?,
            address: None,
            health: None,
            proxy_reachable: false,
            service_manager: None,
            service_fallback_reason: None,
            agents: Vec::new(),
            table_state: TableState::default(),
            message: "Loading local Controller state...".to_string(),
            last_refresh: None,
            confirmation: None,
            show_help: false,
        })
    }

    async fn refresh(&mut self) {
        let config = match service::registered_config(&self.workspace) {
            Ok(Some(config)) => config,
            Ok(None) => {
                self.address = None;
                self.health = None;
                self.proxy_reachable = false;
                self.service_manager = None;
                self.service_fallback_reason = None;
                self.agents.clear();
                self.message = format!(
                    "Workspace '{}' is not connected on this machine.",
                    self.workspace
                );
                self.last_refresh = Some(Instant::now());
                return;
            }
            Err(error) => {
                self.message = format!("Could not load service configuration: {error:#}");
                self.last_refresh = Some(Instant::now());
                return;
            }
        };
        self.service_manager = Some(config.service_manager);
        self.service_fallback_reason = config.service_fallback_reason.clone();

        let address = match config.listen.parse::<SocketAddr>() {
            Ok(address) => address,
            Err(error) => {
                self.address = None;
                self.health = None;
                self.proxy_reachable = false;
                self.agents.clear();
                self.message = format!("Installed listen address is invalid: {error}");
                self.last_refresh = Some(Instant::now());
                return;
            }
        };
        self.address = Some(address);

        let health_url = format!("http://{address}/api/health");
        match self.client.get(health_url).send().await {
            Ok(response) => match response.error_for_status() {
                Ok(response) => match response.json::<Health>().await {
                    Ok(health)
                        if health.workspace_id == self.workspace
                            && health.server_id == config.server_id =>
                    {
                        self.proxy_reachable = health.proxy_connected;
                        self.health = Some(health);
                    }
                    Ok(_) => {
                        self.health = None;
                        self.message =
                            "Another Controller is using the configured address.".to_string();
                    }
                    Err(error) => {
                        self.health = None;
                        self.message = format!("Invalid Controller health response: {error}");
                    }
                },
                Err(error) => {
                    self.health = None;
                    self.message = format!("Controller health check failed: {error}");
                }
            },
            Err(_) => {
                self.health = None;
                self.proxy_reachable = false;
                self.agents.clear();
                self.message = "Controller is not reachable. Press s to start it.".to_string();
                self.last_refresh = Some(Instant::now());
                return;
            }
        }

        if self.health.is_some() {
            let local_agents_url = format!("http://{address}/api/local/agents");
            let local_agents_available = match self
                .client
                .get(local_agents_url)
                .header(OPERATOR_CREDENTIAL_HEADER, &config.operator_credential)
                .send()
                .await
            {
                Ok(response) => match response.error_for_status() {
                    Ok(response) => match response.json::<AgentList>().await {
                        Ok(mut list) => {
                            list.agents
                                .retain(|agent| agent.server_id == config.server_id);
                            list.agents.sort_by(|left, right| {
                                left.name
                                    .to_ascii_lowercase()
                                    .cmp(&right.name.to_ascii_lowercase())
                            });
                            self.agents = list.agents;
                            true
                        }
                        Err(error) => {
                            self.agents.clear();
                            self.message = format!("Invalid local Agent response: {error}");
                            false
                        }
                    },
                    Err(error) => {
                        self.agents.clear();
                        self.message = format!("Local Agent request failed: {error}");
                        false
                    }
                },
                Err(error) => {
                    self.agents.clear();
                    self.message = format!("Local Agent state is unavailable: {error}");
                    false
                }
            };

            if let Some(health) = &self.health {
                self.proxy_reachable = health.proxy_connected;
                if health.proxy_connected {
                    if local_agents_available {
                        self.message = if let Some(reason) = &self.service_fallback_reason {
                            format!(
                                "Controller and Proxy are reachable. Foreground fallback: {reason}"
                            )
                        } else {
                            "Controller and Proxy are reachable; showing local Host state."
                                .to_string()
                        };
                    }
                } else if local_agents_available {
                    let detail = match health.connection_state.as_deref() {
                        Some("fenced") => "Proxy fenced this Controller as a duplicate".to_string(),
                        _ => health
                            .proxy_last_error
                            .clone()
                            .unwrap_or_else(|| "Proxy lease is not current".to_string()),
                    };
                    self.mark_proxy_unreachable(detail);
                }
            }
        }

        self.clamp_selection();
        self.last_refresh = Some(Instant::now());
    }

    fn mark_proxy_unreachable(&mut self, detail: String) {
        self.proxy_reachable = false;
        self.message = format!("{detail}. Showing local Host state.");
    }

    fn clamp_selection(&mut self) {
        if self.agents.is_empty() {
            self.table_state.select(None);
        } else {
            let selected = self
                .table_state
                .selected()
                .unwrap_or(0)
                .min(self.agents.len() - 1);
            self.table_state.select(Some(selected));
        }
    }

    fn select_next(&mut self) {
        if !self.agents.is_empty() {
            let next = self
                .table_state
                .selected()
                .map_or(0, |selected| (selected + 1).min(self.agents.len() - 1));
            self.table_state.select(Some(next));
        }
    }

    fn select_previous(&mut self) {
        if !self.agents.is_empty() {
            let previous = self.table_state.selected().unwrap_or(0).saturating_sub(1);
            self.table_state.select(Some(previous));
        }
    }

    fn refresh_due(&self) -> bool {
        self.last_refresh
            .is_none_or(|last| last.elapsed() >= REFRESH_INTERVAL)
    }

    async fn execute(&mut self, action: Confirmation) {
        let workspace = self.workspace.clone();
        self.message = match tokio::task::spawn_blocking(move || match action {
            Confirmation::Stop => service::stop(&workspace),
            Confirmation::Restart => service::restart(&workspace),
        })
        .await
        {
            Ok(Ok(())) => match action {
                Confirmation::Stop => "Host service stopped.".to_string(),
                Confirmation::Restart => "Host service restarted.".to_string(),
            },
            Ok(Err(error)) => format!("Service action failed: {error:#}"),
            Err(error) => format!("Service action task failed: {error}"),
        };
        self.last_refresh = None;
    }

    async fn start(&mut self) {
        let workspace = self.workspace.clone();
        self.message = match tokio::task::spawn_blocking(move || service::start(&workspace)).await {
            Ok(Ok(())) => "Host service started.".to_string(),
            Ok(Err(error)) => format!("Could not start Host service: {error:#}"),
            Err(error) => format!("Service action task failed: {error}"),
        };
        self.last_refresh = None;
    }

    async fn restart_controller(&mut self) {
        let workspace = self.workspace.clone();
        self.message = match tokio::task::spawn_blocking(move || {
            service::restart_controller(&workspace)
        })
        .await
        {
            Ok(Ok(())) => {
                "Controller restart requested; Host and Agents are preserved.".to_string()
            }
            Ok(Err(error)) => format!("Could not restart Controller: {error:#}"),
            Err(error) => format!("Controller action task failed: {error}"),
        };
        self.last_refresh = None;
    }
}

pub async fn run(workspace: &str) -> Result<()> {
    require_interactive_terminal(std::io::stdin(), std::io::stdout())?;
    let mut terminal = ratatui::try_init().context("failed to initialize terminal")?;
    let result = run_loop(&mut terminal, workspace).await;
    let restore = ratatui::try_restore().context("failed to restore terminal");
    result.and(restore)
}

fn require_interactive_terminal(stdin: Stdin, stdout: Stdout) -> Result<()> {
    if !stdin.is_terminal() || !stdout.is_terminal() {
        anyhow::bail!("--tui requires an interactive terminal");
    }
    Ok(())
}

async fn run_loop(terminal: &mut ratatui::DefaultTerminal, workspace: &str) -> Result<()> {
    let mut app = App::new(workspace)?;
    app.refresh().await;
    let mut redraw = true;

    loop {
        if redraw {
            terminal
                .draw(|frame| draw(frame, &mut app))
                .context("failed to draw TUI")?;
            redraw = false;
        }

        if event::poll(Duration::from_millis(100)).context("failed to poll terminal events")? {
            let event = event::read().context("failed to read terminal event")?;
            if let Event::Key(key) = event {
                if key.kind == KeyEventKind::Press && handle_key(&mut app, key).await? {
                    return Ok(());
                }
                redraw = true;
            } else if matches!(event, Event::Resize(_, _)) {
                redraw = true;
            }
        }

        if app.refresh_due() && app.confirmation.is_none() {
            app.refresh().await;
            redraw = true;
        }
    }
}

async fn handle_key(app: &mut App, key: KeyEvent) -> Result<bool> {
    if app.show_help {
        app.show_help = false;
        return Ok(false);
    }
    if let Some(action) = app.confirmation.take() {
        match key.code {
            KeyCode::Char('y' | 'Y') => app.execute(action).await,
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                app.message = "Action cancelled.".to_string();
            }
            _ => app.confirmation = Some(action),
        }
        return Ok(false);
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
        KeyCode::Char('?') => app.show_help = true,
        KeyCode::Char('r') => app.refresh().await,
        KeyCode::Char('s') => app.start().await,
        KeyCode::Char('x') => app.confirmation = Some(Confirmation::Stop),
        KeyCode::Char('R') => app.confirmation = Some(Confirmation::Restart),
        KeyCode::Char('c') => app.restart_controller().await,
        KeyCode::Down | KeyCode::Char('j') => app.select_next(),
        KeyCode::Up | KeyCode::Char('k') => app.select_previous(),
        _ => {}
    }
    Ok(false)
}

fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let sections = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(5),
        Constraint::Min(7),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);

    let title = Line::from(vec![
        Span::styled(
            " Treer Agent Server ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  workspace: {}", app.workspace)),
    ]);
    frame.render_widget(
        Paragraph::new(title)
            .block(Block::default().borders(Borders::BOTTOM))
            .alignment(Alignment::Left),
        sections[0],
    );

    draw_summary(frame, app, sections[1]);
    draw_agents(frame, app, sections[2]);

    let message_style =
        if app.health.is_some() && app.proxy_reachable && app.service_fallback_reason.is_none() {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::Yellow)
        };
    frame.render_widget(
        Paragraph::new(app.message.as_str())
            .style(message_style)
            .wrap(Wrap { trim: true })
            .block(Block::bordered().title(" Status ")),
        sections[3],
    );
    frame.render_widget(
        Paragraph::new("? help  r refresh  s start  c controller  x stop  R restart  q quit")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center),
        sections[4],
    );

    if let Some(action) = app.confirmation {
        draw_confirmation(frame, action);
    } else if app.show_help {
        draw_help(frame);
    }
}

fn draw_summary(frame: &mut Frame, app: &App, area: Rect) {
    let columns = Layout::horizontal([
        Constraint::Percentage(25),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
    ])
    .split(area);
    let controller = match (&app.health, app.address) {
        (Some(health), Some(address)) => format!(
            "ONLINE\n{} · {}",
            address,
            short_id(&health.controller_epoch)
        ),
        _ => "OFFLINE".to_string(),
    };
    let proxy = if app.proxy_reachable {
        "CONNECTED"
    } else {
        "UNAVAILABLE"
    };
    let supervision = app.service_manager.map_or_else(
        || "UNKNOWN".to_string(),
        |manager| {
            if app.service_fallback_reason.is_some() {
                format!("{}\nFALLBACK", manager.to_string().to_uppercase())
            } else {
                manager.to_string().to_uppercase()
            }
        },
    );
    summary_panel(
        frame,
        columns[0],
        "Controller",
        &controller,
        app.health.is_some(),
    );
    summary_panel(frame, columns[1], "Proxy", proxy, app.proxy_reachable);
    summary_panel(
        frame,
        columns[2],
        "Supervision",
        &supervision,
        app.service_fallback_reason.is_none(),
    );
    summary_panel(
        frame,
        columns[3],
        "Agents",
        &app.agents.len().to_string(),
        app.health.is_some(),
    );
}

fn summary_panel(frame: &mut Frame, area: Rect, title: &str, value: &str, healthy: bool) {
    let color = if healthy { Color::Green } else { Color::Yellow };
    frame.render_widget(
        Paragraph::new(value)
            .style(Style::default().fg(color).add_modifier(Modifier::BOLD))
            .alignment(Alignment::Center)
            .block(Block::bordered().title(format!(" {title} "))),
        area,
    );
}

fn draw_agents(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.agents.iter().map(|agent| {
        Row::new([
            Cell::from(agent.name.clone()),
            Cell::from(agent.kind.clone()),
            Cell::from(status_name(agent.status)),
            Cell::from(agent.cwd.clone()),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(28),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Percentage(50),
        ],
    )
    .header(
        Row::new(["Name", "Kind", "Status", "Working directory"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("› ")
    .block(Block::bordered().title(" Agents on this machine "));
    frame.render_stateful_widget(table, area, &mut app.table_state);

    if app.agents.is_empty() {
        frame.render_widget(
            Paragraph::new("No Agents are currently running on this machine.")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center),
            centered_rect(50, 3, area),
        );
    }
}

fn draw_confirmation(frame: &mut Frame, action: Confirmation) {
    let area = centered_rect(68, 7, frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(action.prompt()),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "y",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" confirm    "),
                Span::styled("n", Style::default().fg(Color::Cyan)),
                Span::raw(" cancel"),
            ]),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(Block::bordered().title(" Confirm destructive action ")),
        area,
    );
}

fn draw_help(frame: &mut Frame) {
    let area = centered_rect(72, 14, frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("j / down     Select next Agent"),
            Line::from("k / up       Select previous Agent"),
            Line::from("r            Refresh now"),
            Line::from("s            Start Host service"),
            Line::from("c            Hot restart Controller (Agents keep running)"),
            Line::from("x            Stop Host (ends Agents; confirm)"),
            Line::from("R            Restart Host (ends Agents; confirm)"),
            Line::from("q / escape   Quit"),
            Line::from(""),
            Line::styled(
                "Press any key to close help.",
                Style::default().fg(Color::DarkGray),
            ),
        ])
        .block(Block::bordered().title(" Help ")),
        area,
    );
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height.min(area.height)),
        Constraint::Fill(1),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}

fn status_name(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Starting => "starting",
        AgentStatus::Working => "working",
        AgentStatus::Idle => "idle",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Exited => "exited",
        AgentStatus::Failed => "failed",
        AgentStatus::Unknown => "unknown",
    }
}

fn short_id(value: &str) -> &str {
    value.get(..value.len().min(12)).unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn destructive_actions_explain_their_runtime_effect() {
        assert!(Confirmation::Stop
            .prompt()
            .contains("terminates all Agents"));
        assert!(Confirmation::Restart
            .prompt()
            .contains("terminates all Agents"));
    }

    #[test]
    fn dashboard_renders_in_a_compact_terminal() {
        let mut app = App::new("default").expect("app");
        app.message = "Controller is not reachable.".to_string();
        app.service_manager = Some(service::ServiceManager::Foreground);
        app.service_fallback_reason = Some("Failed to connect to bus".to_string());
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("draw dashboard");
        let rendered =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                });
        assert!(rendered.contains("Treer Agent Server"));
        assert!(rendered.contains("Controller is not reachable"));
        assert!(rendered.contains("FOREGROUND"));
        assert!(rendered.contains("FALLBACK"));
    }

    #[test]
    fn proxy_failure_preserves_local_agents() {
        let mut app = App::new("default").expect("app");
        app.agents.push(AgentInfo {
            agent_id: "agent-a".to_string(),
            workspace_id: "default".to_string(),
            server_id: "machine-a".to_string(),
            kind: "shell".to_string(),
            name: "Local agent".to_string(),
            cwd: "/workspace".to_string(),
            status: AgentStatus::Working,
            pid: Some(42),
            started_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            exited_at: None,
            exit_code: None,
            output_revision: 0,
            interface: None,
        });

        app.mark_proxy_unreachable("Proxy request failed".to_string());

        assert_eq!(app.agents.len(), 1);
        assert!(!app.proxy_reachable);
        assert!(app.message.contains("Showing local Host state"));
    }
}
