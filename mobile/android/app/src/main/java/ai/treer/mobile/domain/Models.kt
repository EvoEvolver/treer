package ai.treer.mobile.domain

data class User(
    val userId: String,
    val email: String,
    val preferredName: String,
    val token: String? = null,
)

data class AuthConfig(
    val invitationRequired: Boolean = false,
    val github: Boolean = false,
    val google: Boolean = false,
)

data class VoiceAsrStatus(
    val enabled: Boolean = false,
    val provider: String? = null,
    val sampleRate: Int = 16000,
    val encoding: String = "pcm16",
)

data class VoiceCommandStatus(
    val enabled: Boolean = false,
    val wireApi: String? = null,
    val model: String? = null,
)

data class VoiceCommandReply(
    val reply: String,
    val utterance: String,
)

data class VoiceLine(
    val role: String,
    val text: String,
)

enum class VoiceInputMode { Hold, Conversation }

data class VoiceTtsStatus(
    val ready: Boolean = false,
    val message: String? = null,
    val volumeMuted: Boolean = false,
    val canInstallEngine: Boolean = false,
    val canInstallVoiceData: Boolean = false,
    val canOpenSettings: Boolean = false,
)

data class Organization(
    val organizationId: String,
    val name: String,
    val role: String = "member",
)

data class Workspace(
    val workspaceId: String,
    val name: String,
    val createdAt: String? = null,
)

data class BuildInfo(
    val version: String = "",
    val gitCommit: String = "",
)

data class MachineSupervision(
    val mode: String = "foreground",
    val fallbackReason: String? = null,
)

data class Machine(
    val serverId: String,
    val workspaceId: String = "",
    val name: String = "",
    val hostname: String = "",
    val root: String = "",
    val controllerBuild: BuildInfo = BuildInfo(),
    val hostBuild: BuildInfo = BuildInfo(),
    val supervision: MachineSupervision? = null,
    val availableAgents: List<String>? = null,
    val status: String = "offline",
    val labels: Map<String, String> = emptyMap(),
    val connectedAt: String? = null,
    val lastSeenAt: String? = null,
) {
    val displayName: String
        get() = name.ifBlank { hostname.ifBlank { serverId } }

    val isOnline: Boolean
        get() = status.equals("online", ignoreCase = true)
}

data class AgentInterface(
    val protocol: String = "",
    val instanceId: String = "",
    val port: Int = 0,
    val capabilities: List<String> = emptyList(),
    val uiPath: String? = null,
    val registeredAt: String? = null,
) {
    fun supports(capability: String): Boolean = capabilities.contains(capability)

    val hasUi: Boolean
        get() = !uiPath.isNullOrBlank()
}

data class Agent(
    val agentId: String,
    val workspaceId: String = "",
    val serverId: String,
    val kind: String,
    val name: String,
    val cwd: String = "",
    val status: String,
    val pid: Int? = null,
    val startedAt: String? = null,
    val updatedAt: String? = null,
    val exitedAt: String? = null,
    val exitCode: Int? = null,
    val outputRevision: Long = 0,
    val interfaceDescriptor: AgentInterface? = null,
) {
    val isTerminalStatus: Boolean
        get() = status.equals("exited", true) || status.equals("failed", true)

    val isNonTerminal: Boolean
        get() = status.lowercase() in NON_TERMINAL_STATUSES

    fun displayStatus(machine: Machine?): String {
        return if (machine?.isOnline == true) status else "offline"
    }

    fun supports(capability: String): Boolean =
        interfaceDescriptor?.supports(capability) == true

    val hasUiPath: Boolean
        get() = interfaceDescriptor?.hasUi == true
}

data class Snapshot(
    val revision: Long = 0,
    val workspace: Workspace? = null,
    val servers: List<Machine> = emptyList(),
    val agents: List<Agent> = emptyList(),
) {
    fun machine(serverId: String): Machine? = servers.firstOrNull { it.serverId == serverId }

    fun agent(agentId: String): Agent? = agents.firstOrNull { it.agentId == agentId }

    val fleetAgents: List<Agent>
        get() = agents.filter { it.kind != "app" }
}

data class LaunchProfile(
    val profileId: String,
    val workspaceId: String = "",
    val name: String,
    val description: String = "",
    val cwd: String = "",
    val command: String,
    val args: List<String> = emptyList(),
    val createdAt: String? = null,
    val createdBy: String? = null,
    val updatedAt: String? = null,
    val updatedBy: String? = null,
) {
    val commandName: String
        get() = command.substringAfterLast('/').substringAfterLast('\\')
            .removeSuffix(".exe")
            .lowercase()

    val looksAisCapable: Boolean
        get() = commandName in AIS_COMMANDS
}

data class TranscriptEntry(
    val id: String,
    val kind: String,
    val role: String? = null,
    val text: String = "",
    val createdAt: String? = null,
)

data class TranscriptPage(
    val agentId: String,
    val entries: List<TranscriptEntry> = emptyList(),
)

data class WorkspaceEvent(
    val revision: Long = 0,
    val workspaceId: String = "",
    val event: String = "",
    val data: Snapshot? = null,
    val agent: Agent? = null,
    val serverId: String? = null,
)

enum class ConnectionState {
    Live,
    Reconnecting,
    Offline,
}

enum class ConfirmAction {
    Create,
    Prompt,
    Abort,
    Stop,
    Delete,
    Launch,
    SwitchProxy,
    Logout,
}

data class ConfirmSpec(
    val action: ConfirmAction,
    val title: String,
    val objectName: String,
    val objectIdSuffix: String? = null,
    val machineHostname: String? = null,
    val promptExcerpt: String? = null,
    val consequence: String,
    val payload: ConfirmPayload,
    val showChange: Boolean = payload !is ConfirmPayload.SwitchProxy && payload !is ConfirmPayload.Logout,
)

sealed class ConfirmPayload {
    data class Create(
        val serverId: String,
        val kind: String,
        val name: String,
        val prompt: String?,
        val profileId: String?,
    ) : ConfirmPayload()

    data class Prompt(val agentId: String, val text: String) : ConfirmPayload()
    data class Abort(val agentId: String) : ConfirmPayload()
    data class Stop(val agentId: String) : ConfirmPayload()
    data class Delete(val agentId: String) : ConfirmPayload()
    data class Launch(
        val profileId: String,
        val serverId: String,
        val name: String,
        val prompt: String?,
    ) : ConfirmPayload()

    data class SwitchProxy(val newUrl: String) : ConfirmPayload()
    data object Logout : ConfirmPayload()
}

data class CatalogEntry(
    val kind: String,
    val command: String,
    val label: String,
)

data class BootstrapInfo(
    val installCommand: String,
    val connectCommand: String,
    val enrollmentKey: String = "",
    val scriptUrl: String = "",
    val workspaceId: String = "",
)

val AGENT_CATALOG: List<CatalogEntry> = listOf(
    CatalogEntry("claude", "claude", "Claude"),
    CatalogEntry("cursor", "cursor-agent", "Cursor"),
    CatalogEntry("grok", "grok", "Grok"),
    CatalogEntry("opencode", "opencode", "OpenCode"),
    CatalogEntry("pi", "pi", "Pi"),
    CatalogEntry("codex", "codex", "Codex"),
)

fun wireAgentKind(kind: String): String = when (kind) {
    "terminal" -> "command"
    "cursor-agent" -> "cursor"
    else -> kind
}

fun preferredCreateKind(machine: Machine?): String {
    val available = machine?.availableAgents
    return if (available != null && "codex" in available) "codex" else "terminal"
}

private val AIS_COMMANDS = setOf(
    "codex", "claude", "pi", "opencode", "grok", "cursor-agent", "cursor",
)

val NON_TERMINAL_STATUSES = setOf("starting", "working", "idle", "blocked", "unknown")
val WORKING_STATUSES = setOf("starting", "working")
val ATTENTION_STATUSES = setOf("blocked", "failed")

fun objectIdSuffix(id: String): String {
    val stripped = id
        .removePrefix("ag_")
        .removePrefix("srv_")
        .removePrefix("ws_")
        .removePrefix("org_")
        .removePrefix("lp_")
    return stripped.takeLast(6)
}

fun promptExcerpt(text: String): String {
    val trimmed = text.trim()
    return if (trimmed.length <= 80) trimmed else trimmed.take(77) + "…"
}

fun defaultAgentName(kind: String): String {
    val now = java.util.Calendar.getInstance()
    val month = (now.get(java.util.Calendar.MONTH) + 1).toString().padStart(2, '0')
    val day = now.get(java.util.Calendar.DAY_OF_MONTH).toString().padStart(2, '0')
    val prefix = when (kind) {
        "terminal" -> "terminal"
        "command" -> "cmd"
        "codex", "claude", "installer" -> kind
        else -> "agent"
    }
    return "$prefix-${now.get(java.util.Calendar.YEAR)}-$month-$day"
}

fun defaultProfileAgentName(profileName: String): String {
    val now = java.util.Calendar.getInstance()
    fun pad(value: Int) = value.toString().padStart(2, '0')
    val slug = profileName.lowercase()
        .replace(Regex("[^a-z0-9]+"), "-")
        .trim('-')
        .take(40)
        .ifBlank { "agent" }
    val stamp = "${now.get(java.util.Calendar.YEAR)}-${pad(now.get(java.util.Calendar.MONTH) + 1)}-" +
        "${pad(now.get(java.util.Calendar.DAY_OF_MONTH))}-" +
        "${pad(now.get(java.util.Calendar.HOUR_OF_DAY))}${pad(now.get(java.util.Calendar.MINUTE))}" +
        pad(now.get(java.util.Calendar.SECOND))
    return "$slug-$stamp".take(80)
}

fun machineRecoveryCommands(workspaceId: String): Pair<String, String> {
    val restart = "treer-agent-server service --workspace $workspaceId restart-controller"
    val start = "treer-agent-server service --workspace $workspaceId start"
    return restart to start
}

fun consequenceFor(action: ConfirmAction, kindOrProfile: String = "", machine: String = "", name: String = ""): String {
    return when (action) {
        ConfirmAction.Abort -> "Cancel the current turn. The Agent process stays running."
        ConfirmAction.Stop -> "Stop the process. You can Launch again. Transcript/PTY history follows Host retention."
        ConfirmAction.Delete -> "Remove this Agent from the workspace. The process is stopped and the workspace entry is deleted."
        ConfirmAction.Create, ConfirmAction.Launch -> "Start $kindOrProfile on $machine as $name."
        ConfirmAction.Prompt -> "Send this follow-up to $name on $machine."
        ConfirmAction.SwitchProxy -> "Leave this control plane and clear the Keychain session. You will sign in to the new Proxy URL. Agents on the old Proxy keep running."
        ConfirmAction.Logout -> "Sign out this device. Other devices stay signed in. The Agent fleet is unchanged."
    }
}

fun promptNeedsConfirm(text: String, agentStatus: String): Boolean {
    return text.length > 500 || agentStatus.lowercase() in WORKING_STATUSES
}

open class ApiException(
    message: String,
    val status: Int = 0,
    val code: String? = null,
) : RuntimeException(message)

class UnauthorizedException(message: String = "authentication required") : ApiException(message, 401, "authentication_required")
