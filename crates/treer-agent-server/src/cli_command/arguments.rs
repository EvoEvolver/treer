use super::*;

#[derive(Debug, Parser)]
#[command(
    name = "treer",
    about = "Discover and coordinate Treer agents",
    version = treer_build_info::DISPLAY,
    arg_required_else_help = true
)]
pub(super) struct Args {
    #[arg(
        long,
        visible_alias = "skills",
        num_args = 0..=1,
        default_missing_value = "treer",
        value_name = "NAME",
        help = "Print a bundled skill and exit (`treer`, `install`, or `macos-container`)"
    )]
    pub(super) skill: Option<String>,
    #[command(subcommand)]
    pub(super) command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(super) enum Command {
    #[command(about = "Register and explicitly forward workspace service endpoints")]
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommand,
    },
    #[command(about = "Run a bounded command on a workspace machine and return its output")]
    Exec {
        #[arg(long)]
        machine: String,
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(long, default_value_t = 30_000, value_name = "MS")]
        timeout: u64,
        #[arg(last = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    #[command(about = "Control and communicate with workspace agents")]
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    #[command(about = "Manage long-running workspace Apps")]
    App {
        #[command(subcommand)]
        command: AppCommand,
    },
    #[command(about = "Discover human members of the workspace organization")]
    Member {
        #[command(subcommand)]
        command: MemberCommand,
    },
    #[command(about = "Manage workspace machines")]
    Machine {
        #[command(subcommand)]
        command: MachineCommand,
    },
    #[command(about = "Connect through the Agent's private Treer network")]
    Network {
        #[command(subcommand)]
        command: NetworkCommand,
    },
    #[command(about = "Register this Agent's semantic interface server")]
    Interface {
        #[command(subcommand)]
        command: InterfaceCommand,
    },
    #[command(about = "Obtain a short-lived identity token for a workspace service")]
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    #[command(about = "Exchange durable contextual Messages")]
    Message {
        #[command(subcommand)]
        command: MessageCommand,
    },
    #[command(about = "Show the current managed agent identity")]
    Whoami,
    #[command(about = "Show this workspace, its machines, and its agents")]
    Status,
}

#[derive(Debug, Subcommand)]
pub(super) enum ServiceCommand {
    List,
    Register {
        #[arg(long)]
        name: String,
        #[arg(long)]
        port: u16,
        #[arg(long)]
        machine: Option<String>,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value = "tcp")]
        protocol: String,
        #[arg(
            long,
            help = "Register or update this Agent's stable primary HTTP endpoint"
        )]
        primary: bool,
    },
    Forward {
        service: String,
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: String,
    },
    Publish {
        service: String,
        #[arg(long, default_value = "workspace")]
        access: String,
        #[arg(long)]
        tcp: bool,
    },
    Delete {
        service: String,
    },
    Unpublish {
        service: String,
        #[arg(long)]
        tcp: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(super) enum RuntimeCommand {
    Restore {
        #[arg(long, required = true)]
        all: bool,
        #[arg(long)]
        server: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub(super) enum AppCommand {
    #[command(about = "List Apps in the current workspace")]
    List,
    #[command(about = "Show an App by unique name or id")]
    Show { target: String },
    #[command(about = "Create and start an App with one HTTP UI endpoint")]
    Create {
        #[arg(long)]
        machine: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(long)]
        port: u16,
        #[arg(
            long,
            help = "Expose the App without Treer authentication on its managed public URL"
        )]
        public: bool,
        #[arg(value_name = "COMMAND")]
        executable: String,
        #[arg(last = true)]
        args: Vec<String>,
    },
    #[command(about = "Start a stopped App")]
    Start { target: String },
    #[command(about = "Stop an App without deleting its service or virtual host")]
    Stop { target: String },
    #[command(about = "Replace an App runtime while preserving its service and virtual host")]
    Restart { target: String },
    #[command(about = "Stop and permanently delete an App and its routes")]
    Delete { target: String },
}

#[derive(Debug, Subcommand)]
pub(super) enum MemberCommand {
    #[command(about = "List organization members addressable from this workspace")]
    List,
}

#[derive(Debug, Subcommand)]
pub(super) enum MessageCommand {
    #[command(about = "Send a durable Message")]
    Send {
        #[arg(long = "to", required = true)]
        recipients: Vec<String>,
        #[arg(long = "context")]
        context_ids: Vec<String>,
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with = "body", hide = true)]
        body_file: Option<PathBuf>,
        #[arg(long)]
        idempotency_key: Option<String>,
        #[arg(long, value_name = "RFC3339")]
        expires_at: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        trace_id: Option<String>,
        #[arg(long, value_name = "PATH")]
        external_source_file: Option<PathBuf>,
    },
    #[command(about = "Reply with a context edge to an existing Message")]
    Reply {
        message_id: String,
        #[arg(long = "to")]
        recipients: Vec<String>,
        #[arg(long = "context")]
        additional_context_ids: Vec<String>,
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with = "body", hide = true)]
        body_file: Option<PathBuf>,
        #[arg(long)]
        idempotency_key: Option<String>,
        #[arg(long, value_name = "PATH")]
        external_source_file: Option<PathBuf>,
    },
    #[command(about = "Get one visible Message by stable ID")]
    Get { message_id: String },
    #[command(about = "List visible Message history without acknowledging it")]
    List {
        #[arg(long)]
        before: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u16,
    },
    #[command(about = "Receive unacknowledged deliveries")]
    Receive {
        #[arg(long, default_value_t = 0, value_name = "MS")]
        wait: u64,
        #[arg(long, default_value_t = 50)]
        limit: u16,
    },
    #[command(about = "Explicitly acknowledge one or more deliveries")]
    Ack {
        #[arg(required = true)]
        delivery_ids: Vec<String>,
        #[arg(long)]
        operation_id: Option<String>,
    },
    #[command(about = "Import a bounded operator-authorized legacy Message batch")]
    Import {
        #[arg(long, default_value = "legacy-mail-v1")]
        format: String,
        #[arg(long, value_name = "PATH", hide = true)]
        body_file: Option<PathBuf>,
        #[arg(long)]
        operation_id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub(super) enum AgentCommand {
    #[command(about = "Create a workspace folder (paths start at workspace root)")]
    Mkdir {
        path: String,
        #[arg(short = 'p', long)]
        parents: bool,
    },
    #[command(about = "List folders and Agent/App workloads")]
    Ls {
        #[arg(default_value = ".")]
        path: String,
        #[arg(short = 'R', long)]
        recursive: bool,
    },
    #[command(about = "Move an Agent/App into a folder, or move/rename a folder")]
    Mv { source: String, destination: String },
    #[command(about = "Remove an empty workspace folder")]
    Rmdir { path: String },
    #[command(about = "Start an Agent or App from its saved launch")]
    Start { target: String },
    #[command(about = "Stop an Agent or App")]
    Stop { target: String },
    #[command(about = "Restart an Agent or App preserving its identity")]
    Restart { target: String },
    #[command(about = "Show one agent by unique name or id")]
    Show { target: String },
    #[command(about = "Print an Agent's primary HTTP service URL for curl or other HTTP clients")]
    Address {
        #[arg(default_value = "self")]
        target: String,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Attach the current terminal; press Ctrl-] to detach")]
    Attach { target: String },
    #[command(about = "Submit a prompt to another agent")]
    Prompt {
        target: String,
        text: String,
        #[command(flatten)]
        wait: PromptWaitArgs,
    },
    #[command(about = "Read recent terminal output")]
    Read {
        target: String,
        #[arg(long, default_value_t = 100)]
        lines: usize,
    },
    #[command(about = "Read a structured transcript, one conversation turn per page")]
    Transcript {
        target: String,
        /// 0-based conversation turn. A turn starts at a user prompt.
        #[arg(long)]
        page: Option<u32>,
        /// Alias for `--page`.
        #[arg(long)]
        cursor: Option<String>,
        /// Number of turns to return. Default is one turn.
        #[arg(long, default_value_t = 1)]
        limit: usize,
    },
    #[command(about = "Send raw terminal key presses")]
    SendKeys {
        target: String,
        #[arg(required = true, num_args = 1..)]
        keys: Vec<String>,
    },
    #[command(about = "Wait for an agent status")]
    Wait {
        target: String,
        #[arg(long, value_enum)]
        until: Vec<WaitStatus>,
        #[arg(long, value_name = "MS")]
        timeout: Option<u64>,
    },
    #[command(about = "Manage this Agent's command for recovery after a Host restart")]
    Startup {
        #[command(subcommand)]
        command: AgentStartupCommand,
    },
    #[command(about = "Manage Agent lifecycle and launch profiles")]
    Admin {
        #[command(subcommand)]
        command: AgentAdminCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(super) enum AgentStartupCommand {
    #[command(about = "Register this Agent's Host-restart command")]
    Set {
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(
            long = "publish",
            value_name = "PORT",
            help = "Retired; register process ports with treer service register instead"
        )]
        publish_ports: Vec<u16>,
        #[arg(last = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    #[command(about = "Show this Agent's registered Host-restart command")]
    Show,
    #[command(about = "Disable and remove this Agent's Host-restart command")]
    Clear,
}

#[derive(Debug, Subcommand)]
pub(super) enum AgentAdminCommand {
    Start {
        target: String,
    },
    Restart {
        target: String,
    },
    #[command(about = "Create an agent")]
    Create {
        #[arg(long)]
        machine: Option<String>,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(
            long = "publish",
            value_name = "PORT",
            help = "Retired; register process ports with treer service register instead"
        )]
        publish_ports: Vec<u16>,
        #[arg(
            long,
            value_name = "URL",
            help = "Retired; launch an explicit installer command or profile instead"
        )]
        recipe: Option<String>,
        #[arg(last = true)]
        args: Vec<String>,
    },
    #[command(about = "Rename an agent")]
    Rename {
        target: String,
        name: String,
    },
    #[command(about = "Stop an agent")]
    Stop {
        target: String,
    },
    #[command(about = "Stop and permanently remove an agent")]
    Delete {
        target: String,
    },
    #[command(about = "Manage reusable Agent launch profiles")]
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(super) enum ProfileCommand {
    #[command(about = "List launch profiles in the current workspace")]
    List,
    #[command(about = "Show a launch profile by unique name or id")]
    Show { target: String },
    #[command(about = "Create a reusable command-based launch profile")]
    Create {
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(value_name = "COMMAND")]
        executable: String,
        #[arg(last = true)]
        args: Vec<String>,
    },
    #[command(about = "Update a launch profile")]
    Update {
        target: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long = "command")]
        executable: Option<String>,
        #[arg(long = "arg", allow_hyphen_values = true)]
        args: Vec<String>,
        #[arg(long, conflicts_with = "args")]
        clear_args: bool,
    },
    #[command(about = "Delete a launch profile")]
    Delete { target: String },
    #[command(about = "Create an Agent from a launch profile")]
    Launch {
        target: String,
        #[arg(long)]
        machine: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, help = "Override the profile working directory for this launch")]
        cwd: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub(super) enum MachineCommand {
    #[command(about = "List machines in the current workspace")]
    List,
    #[command(about = "Show one machine by unique name or id")]
    Show { target: String },
    #[command(about = "Rename a machine")]
    Rename { target: String, name: String },
    #[command(about = "Remove a machine and revoke its credential")]
    Delete { target: String },
}

#[derive(Debug, Subcommand)]
pub(super) enum TokenCommand {
    #[command(about = "Create an audience-bound Bearer token")]
    Create {
        #[arg(value_name = "SERVICE")]
        audience: String,
        #[arg(long, help = "Print the complete JSON token response")]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(super) enum InterfaceCommand {
    #[command(about = "Show this Agent's registered interface")]
    Show,
    #[command(about = "Register or refresh this Agent's interface")]
    Register {
        #[arg(long)]
        port: u16,
        #[arg(long)]
        instance_id: String,
        #[arg(long = "capability")]
        capabilities: Vec<String>,
        #[arg(long)]
        ui_path: Option<String>,
    },
    #[command(about = "Remove this Agent's interface registration")]
    Clear,
}

#[derive(Debug, Subcommand)]
pub(super) enum NetworkCommand {
    #[command(about = "Bridge stdin/stdout to a host through the Agent's Treer network")]
    Connect { host: String, port: u16 },
}

#[derive(Debug, clap::Args)]
pub(super) struct PromptWaitArgs {
    #[arg(
        long,
        help = "Wait for a matching status after observed agent activity"
    )]
    pub(super) wait: bool,
    #[arg(long, value_enum, requires = "wait")]
    pub(super) until: Vec<WaitStatus>,
    #[arg(long, value_name = "MS", requires = "wait")]
    pub(super) timeout: Option<u64>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum WaitStatus {
    Starting,
    Working,
    Idle,
    Blocked,
    Exited,
    Failed,
    Unknown,
}

impl WaitStatus {
    pub(super) fn matches(self, status: AgentStatus) -> bool {
        matches!(
            (self, status),
            (Self::Starting, AgentStatus::Starting)
                | (Self::Working, AgentStatus::Working)
                | (Self::Idle, AgentStatus::Idle)
                | (Self::Blocked, AgentStatus::Blocked)
                | (Self::Exited, AgentStatus::Exited)
                | (Self::Failed, AgentStatus::Failed)
                | (Self::Unknown, AgentStatus::Unknown)
        )
    }
}
