package ai.treer.mobile.data

import ai.treer.mobile.domain.Agent
import ai.treer.mobile.domain.AgentInterface
import ai.treer.mobile.domain.AuthConfig
import ai.treer.mobile.domain.BuildInfo
import ai.treer.mobile.domain.LaunchProfile
import ai.treer.mobile.domain.Machine
import ai.treer.mobile.domain.MachineSupervision
import ai.treer.mobile.domain.Organization
import ai.treer.mobile.domain.Snapshot
import ai.treer.mobile.domain.TranscriptEntry
import ai.treer.mobile.domain.TranscriptPage
import ai.treer.mobile.domain.User
import ai.treer.mobile.domain.VoiceAsrStatus
import ai.treer.mobile.domain.VoiceCommandReply
import ai.treer.mobile.domain.VoiceCommandStatus
import ai.treer.mobile.domain.Workspace
import ai.treer.mobile.domain.WorkspaceEvent
import com.google.gson.JsonArray
import com.google.gson.JsonElement
import com.google.gson.JsonObject
import com.google.gson.JsonParser

fun JsonElement?.asObj(): JsonObject? = if (this != null && isJsonObject) asJsonObject else null
fun JsonElement?.asArr(): JsonArray? = if (this != null && isJsonArray) asJsonArray else null

fun JsonObject.str(key: String, default: String = ""): String {
    val value = get(key) ?: return default
    if (value.isJsonNull) return default
    return runCatching { value.asString }.getOrDefault(default)
}

fun JsonObject.strOrNull(key: String): String? {
    val value = get(key) ?: return null
    if (value.isJsonNull) return null
    val text = runCatching { value.asString }.getOrNull()
    return text?.takeIf { it.isNotBlank() }
}

fun JsonObject.long(key: String, default: Long = 0): Long {
    val value = get(key) ?: return default
    if (value.isJsonNull) return default
    return runCatching { value.asLong }.getOrDefault(default)
}

fun JsonObject.int(key: String, default: Int = 0): Int {
    val value = get(key) ?: return default
    if (value.isJsonNull) return default
    return runCatching { value.asInt }.getOrDefault(default)
}

fun JsonObject.intOrNull(key: String): Int? {
    val value = get(key) ?: return null
    if (value.isJsonNull) return null
    return runCatching { value.asInt }.getOrNull()
}

fun JsonObject.bool(key: String, default: Boolean = false): Boolean {
    val value = get(key) ?: return default
    if (value.isJsonNull) return default
    return runCatching { value.asBoolean }.getOrDefault(default)
}

fun parseUser(obj: JsonObject, token: String? = obj.strOrNull("token")): User {
    return User(
        userId = obj.str("user_id"),
        email = obj.str("email"),
        preferredName = obj.str("preferred_name"),
        token = token,
    )
}

fun parseAuthConfig(obj: JsonObject): AuthConfig {
    return AuthConfig(
        invitationRequired = obj.bool("invitation_required"),
        github = obj.bool("github"),
        google = obj.bool("google"),
    )
}

fun parseBootstrap(obj: JsonObject): ai.treer.mobile.domain.BootstrapInfo {
    return ai.treer.mobile.domain.BootstrapInfo(
        installCommand = obj.str("install_command"),
        connectCommand = obj.str("connect_command"),
        enrollmentKey = obj.str("enrollment_key"),
        scriptUrl = obj.str("script_url"),
        workspaceId = obj.str("workspace_id"),
    )
}

fun parseVoiceAsrStatus(obj: JsonObject): VoiceAsrStatus {
    return VoiceAsrStatus(
        enabled = obj.bool("enabled"),
        provider = obj.strOrNull("provider"),
        sampleRate = obj.int("sample_rate", 16000),
        encoding = obj.str("encoding", "pcm16"),
    )
}

fun parseVoiceCommandStatus(obj: JsonObject): VoiceCommandStatus {
    return VoiceCommandStatus(
        enabled = obj.bool("enabled"),
        wireApi = obj.strOrNull("wire_api"),
        model = obj.strOrNull("model"),
    )
}

fun parseVoiceCommandReply(obj: JsonObject): VoiceCommandReply {
    return VoiceCommandReply(
        reply = obj.str("reply"),
        utterance = obj.str("utterance"),
    )
}

fun parseOrganization(obj: JsonObject): Organization {
    return Organization(
        organizationId = obj.str("organization_id"),
        name = obj.str("name"),
        role = obj.str("role", "member"),
    )
}

fun parseWorkspace(obj: JsonObject): Workspace {
    return Workspace(
        workspaceId = obj.str("workspace_id"),
        name = obj.str("name"),
        createdAt = obj.strOrNull("created_at"),
    )
}

fun parseBuild(obj: JsonObject?): BuildInfo {
    obj ?: return BuildInfo()
    return BuildInfo(version = obj.str("version"), gitCommit = obj.str("git_commit"))
}

fun parseMachine(obj: JsonObject): Machine {
    val supervision = obj.get("supervision").asObj()?.let {
        MachineSupervision(mode = it.str("mode"), fallbackReason = it.strOrNull("fallback_reason"))
    }
    val available = obj.get("available_agents").asArr()?.map { it.asString }
    val labels = linkedMapOf<String, String>()
    obj.get("labels").asObj()?.entrySet()?.forEach { (key, value) ->
        labels[key] = runCatching { value.asString }.getOrDefault("")
    }
    return Machine(
        serverId = obj.str("server_id"),
        workspaceId = obj.str("workspace_id"),
        name = obj.str("name"),
        hostname = obj.str("hostname"),
        root = obj.str("root"),
        controllerBuild = parseBuild(obj.get("controller_build").asObj()),
        hostBuild = parseBuild(obj.get("host_build").asObj()),
        supervision = supervision,
        availableAgents = available,
        status = obj.str("status", "offline"),
        labels = labels,
        connectedAt = obj.strOrNull("connected_at"),
        lastSeenAt = obj.strOrNull("last_seen_at"),
    )
}

fun parseInterface(obj: JsonObject): AgentInterface {
    val capabilities = obj.get("capabilities").asArr()?.map { it.asString } ?: emptyList()
    return AgentInterface(
        protocol = obj.str("protocol"),
        instanceId = obj.str("instance_id"),
        port = obj.int("port"),
        capabilities = capabilities,
        uiPath = obj.strOrNull("ui_path"),
        registeredAt = obj.strOrNull("registered_at"),
    )
}

fun parseAgent(obj: JsonObject): Agent {
    return Agent(
        agentId = obj.str("agent_id"),
        workspaceId = obj.str("workspace_id"),
        serverId = obj.str("server_id"),
        kind = obj.str("kind"),
        name = obj.str("name"),
        cwd = obj.str("cwd"),
        status = obj.str("status", "unknown"),
        pid = obj.intOrNull("pid"),
        startedAt = obj.strOrNull("started_at"),
        updatedAt = obj.strOrNull("updated_at"),
        exitedAt = obj.strOrNull("exited_at"),
        exitCode = obj.intOrNull("exit_code"),
        outputRevision = obj.long("output_revision"),
        interfaceDescriptor = obj.get("interface").asObj()?.let(::parseInterface),
    )
}

fun parseSnapshot(obj: JsonObject): Snapshot {
    val workspace = obj.get("workspace").asObj()?.let(::parseWorkspace)
    val servers = obj.get("servers").asArr()?.mapNotNull { it.asObj()?.let(::parseMachine) } ?: emptyList()
    val agents = obj.get("agents").asArr()?.mapNotNull { it.asObj()?.let(::parseAgent) } ?: emptyList()
    return Snapshot(
        revision = obj.long("revision"),
        workspace = workspace,
        servers = servers,
        agents = agents,
    )
}

fun parseLaunchProfile(obj: JsonObject): LaunchProfile {
    val args = obj.get("args").asArr()?.map { it.asString } ?: emptyList()
    return LaunchProfile(
        profileId = obj.str("profile_id"),
        workspaceId = obj.str("workspace_id"),
        name = obj.str("name"),
        description = obj.str("description"),
        cwd = obj.str("cwd"),
        command = obj.str("command"),
        args = args,
        createdAt = obj.strOrNull("created_at"),
        createdBy = obj.strOrNull("created_by"),
        updatedAt = obj.strOrNull("updated_at"),
        updatedBy = obj.strOrNull("updated_by"),
    )
}

fun parseTranscript(obj: JsonObject): TranscriptPage {
    val entries = obj.get("entries").asArr()?.mapNotNull { element ->
        val item = element.asObj() ?: return@mapNotNull null
        TranscriptEntry(
            id = item.str("id"),
            kind = item.str("kind"),
            role = item.strOrNull("role"),
            text = extractTranscriptText(item.get("content")),
            createdAt = item.strOrNull("created_at"),
        )
    } ?: emptyList()
    return TranscriptPage(agentId = obj.str("agent_id"), entries = entries)
}

fun extractTranscriptText(content: JsonElement?): String {
    if (content == null || content.isJsonNull) return ""
    if (content.isJsonPrimitive) return content.asString
    if (content.isJsonArray) {
        return content.asJsonArray.joinToString("\n") { extractTranscriptText(it) }.trim()
    }
    val obj = content.asObj() ?: return content.toString()
    obj.strOrNull("text")?.let { return it }
    obj.get("content")?.let { nested ->
        val nestedText = extractTranscriptText(nested)
        if (nestedText.isNotBlank()) return nestedText
    }
    obj.get("parts").asArr()?.let { parts ->
        val joined = parts.joinToString("") { extractTranscriptText(it) }
        if (joined.isNotBlank()) return joined
    }
    return obj.toString()
}

fun parseWorkspaceEvent(raw: String): WorkspaceEvent {
    val obj = JsonParser.parseString(raw).asJsonObject
    val event = obj.str("event")
    val data = obj.get("data")
    val snapshot = when (event) {
        "workspace.snapshot" -> data.asObj()?.let(::parseSnapshot)
        else -> null
    }
    val agent = when (event) {
        "agent.updated" -> data.asObj()?.let(::parseAgent)
        else -> null
    }
    val serverId = when (event) {
        "server.offline", "server.online" -> data.asObj()?.strOrNull("server_id") ?: data.asObj()?.strOrNull("id")
        else -> null
    }
    return WorkspaceEvent(
        revision = obj.long("revision"),
        workspaceId = obj.str("workspace_id"),
        event = event,
        data = snapshot,
        agent = agent,
        serverId = serverId,
    )
}

fun parseErrorMessage(body: String, fallback: String): Pair<String, String?> {
    return runCatching {
        val obj = JsonParser.parseString(body).asJsonObject
        val error = obj.get("error").asObj()
        val message = error?.strOrNull("message") ?: obj.strOrNull("message") ?: fallback
        val code = error?.strOrNull("code")
        message to code
    }.getOrDefault(fallback to null)
}

fun latestTurnText(page: TranscriptPage): String? {
    val last = page.entries.lastOrNull { entry ->
        val role = entry.role?.lowercase()
        role == "user" || role == "assistant" || entry.kind == "message"
    } ?: page.entries.lastOrNull { it.text.isNotBlank() }
    val text = last?.text?.trim().orEmpty()
    if (text.isBlank()) return null
    return if (text.length <= 400) text else text.take(397) + "…"
}
