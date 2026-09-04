package ai.treer.mobile.data

import ai.treer.mobile.domain.Agent
import ai.treer.mobile.domain.ApiException
import ai.treer.mobile.domain.AuthConfig
import ai.treer.mobile.domain.BootstrapInfo
import ai.treer.mobile.domain.LaunchProfile
import ai.treer.mobile.domain.Organization
import ai.treer.mobile.domain.Snapshot
import ai.treer.mobile.domain.TranscriptPage
import ai.treer.mobile.domain.UnauthorizedException
import ai.treer.mobile.domain.User
import ai.treer.mobile.domain.VoiceAsrStatus
import ai.treer.mobile.domain.VoiceCommandReply
import ai.treer.mobile.domain.VoiceCommandStatus
import ai.treer.mobile.domain.VoiceLine
import ai.treer.mobile.domain.Workspace
import com.google.gson.JsonObject
import com.google.gson.JsonParser
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.util.concurrent.TimeUnit

class TreerApi {
    data class ForwardedResponse(
        val status: Int,
        val headers: Map<String, String>,
        val body: ByteArray,
        val mimeType: String,
        val encoding: String?,
    )

    fun health(baseUrl: String): JsonObject {
        val json = request(baseUrl, null, "GET", "/api/health")
        return JsonParser.parseString(json).asJsonObject
    }

    fun authConfig(baseUrl: String): AuthConfig {
        val json = request(baseUrl, null, "GET", "/api/auth/config")
        return parseAuthConfig(JsonParser.parseString(json).asJsonObject)
    }

    fun login(
        baseUrl: String,
        email: String,
        password: String,
        deviceId: String,
        deviceName: String,
    ): User {
        val body = JsonObject().apply {
            addProperty("email", email)
            addProperty("password", password)
            addProperty("device_id", deviceId)
            addProperty("device_name", deviceName)
        }
        val json = request(baseUrl, null, "POST", "/api/auth/login", body.toString(), nativeClient = true)
        return parseUser(JsonParser.parseString(json).asJsonObject)
    }

    fun register(
        baseUrl: String,
        email: String,
        preferredName: String,
        password: String,
        invite: String?,
        deviceId: String,
        deviceName: String,
    ): User {
        val body = JsonObject().apply {
            addProperty("email", email)
            addProperty("preferred_name", preferredName)
            addProperty("password", password)
            if (!invite.isNullOrBlank()) addProperty("invite", invite)
            addProperty("device_id", deviceId)
            addProperty("device_name", deviceName)
        }
        val json = request(baseUrl, null, "POST", "/api/auth/register", body.toString(), nativeClient = true)
        return parseUser(JsonParser.parseString(json).asJsonObject)
    }

    fun requestPasswordReset(baseUrl: String, email: String) {
        val body = JsonObject().apply { addProperty("email", email) }
        request(baseUrl, null, "POST", "/api/auth/request-password-reset", body.toString())
    }

    fun resetPassword(baseUrl: String, token: String, password: String) {
        val body = JsonObject().apply {
            addProperty("token", token)
            addProperty("password", password)
        }
        request(baseUrl, null, "POST", "/api/auth/reset-password", body.toString())
    }

    fun me(baseUrl: String, token: String): User {
        val json = request(baseUrl, token, "GET", "/api/auth/me")
        return parseUser(JsonParser.parseString(json).asJsonObject, token)
    }

    fun updateProfile(baseUrl: String, token: String, email: String, preferredName: String): User {
        val body = JsonObject().apply {
            addProperty("email", email)
            addProperty("preferred_name", preferredName)
        }
        val json = request(baseUrl, token, "PATCH", "/api/auth/profile", body.toString())
        return parseUser(JsonParser.parseString(json).asJsonObject, token)
    }

    fun logout(baseUrl: String, token: String) {
        runCatching { request(baseUrl, token, "POST", "/api/auth/logout", "{}") }
    }

    fun organizations(baseUrl: String, token: String): List<Organization> {
        val json = request(baseUrl, token, "GET", "/api/organizations")
        val obj = JsonParser.parseString(json).asJsonObject
        return obj.get("organizations").asArr()?.mapNotNull { it.asObj()?.let(::parseOrganization) } ?: emptyList()
    }

    fun workspaces(baseUrl: String, token: String, organizationId: String): List<Workspace> {
        val json = request(
            baseUrl,
            token,
            "GET",
            "/api/workspaces?organization_id=${organizationId.encodeQuery()}",
        )
        val obj = JsonParser.parseString(json).asJsonObject
        return obj.get("workspaces").asArr()?.mapNotNull { it.asObj()?.let(::parseWorkspace) } ?: emptyList()
    }

    fun createWorkspace(baseUrl: String, token: String, organizationId: String, name: String): Workspace {
        val body = JsonObject().apply {
            addProperty("organization_id", organizationId)
            addProperty("name", name)
        }
        val json = request(baseUrl, token, "POST", "/api/workspaces", body.toString())
        val obj = JsonParser.parseString(json).asJsonObject
        val workspace = obj.get("workspace").asObj() ?: obj
        return parseWorkspace(workspace)
    }

    fun snapshot(baseUrl: String, token: String, workspaceId: String): Snapshot {
        val json = request(baseUrl, token, "GET", "/api/workspaces/${workspaceId.encodePath()}/snapshot")
        return parseSnapshot(JsonParser.parseString(json).asJsonObject)
    }

    fun bootstrap(baseUrl: String, token: String, workspaceId: String): BootstrapInfo {
        val json = request(
            baseUrl,
            token,
            "POST",
            "/api/workspaces/${workspaceId.encodePath()}/bootstrap",
            "{}",
        )
        return parseBootstrap(JsonParser.parseString(json).asJsonObject)
    }

    fun launchProfiles(baseUrl: String, token: String, workspaceId: String): List<LaunchProfile> {
        val json = request(baseUrl, token, "GET", "/api/workspaces/${workspaceId.encodePath()}/launch-profiles")
        val obj = JsonParser.parseString(json).asJsonObject
        return obj.get("profiles").asArr()?.mapNotNull { it.asObj()?.let(::parseLaunchProfile) } ?: emptyList()
    }

    fun createAgent(
        baseUrl: String,
        token: String,
        workspaceId: String,
        serverId: String,
        kind: String,
        name: String,
    ): Agent {
        val body = JsonObject().apply {
            addProperty("server_id", serverId)
            addProperty("kind", kind)
            addProperty("name", name)
            addProperty("cwd", "")
            add("args", com.google.gson.JsonArray())
            addProperty("cols", 120)
            addProperty("rows", 36)
        }
        val json = request(baseUrl, token, "POST", "/api/workspaces/${workspaceId.encodePath()}/agents", body.toString())
        return parseAgent(JsonParser.parseString(json).asJsonObject)
    }

    fun launchProfile(
        baseUrl: String,
        token: String,
        workspaceId: String,
        profileId: String,
        serverId: String,
        agentName: String,
    ): Agent {
        val body = JsonObject().apply {
            addProperty("server_id", serverId)
            addProperty("agent_name", agentName)
            addProperty("cols", 120)
            addProperty("rows", 36)
        }
        val json = request(
            baseUrl,
            token,
            "POST",
            "/api/workspaces/${workspaceId.encodePath()}/launch-profiles/${profileId.encodePath()}/launch",
            body.toString(),
        )
        return parseAgent(JsonParser.parseString(json).asJsonObject)
    }

    fun promptAgent(baseUrl: String, token: String, workspaceId: String, agentId: String, text: String) {
        val body = JsonObject().apply { addProperty("text", text) }
        request(
            baseUrl,
            token,
            "POST",
            "/api/workspaces/${workspaceId.encodePath()}/agents/${agentId.encodePath()}/prompt",
            body.toString(),
        )
    }

    fun stopAgent(baseUrl: String, token: String, workspaceId: String, agentId: String) {
        request(
            baseUrl,
            token,
            "POST",
            "/api/workspaces/${workspaceId.encodePath()}/agents/${agentId.encodePath()}/stop",
            "{}",
        )
    }

    fun abortAgent(baseUrl: String, token: String, workspaceId: String, agentId: String) {
        request(
            baseUrl,
            token,
            "POST",
            "/api/workspaces/${workspaceId.encodePath()}/agents/${agentId.encodePath()}/abort",
            "{}",
        )
    }

    fun deleteAgent(baseUrl: String, token: String, workspaceId: String, agentId: String) {
        request(
            baseUrl,
            token,
            "DELETE",
            "/api/workspaces/${workspaceId.encodePath()}/agents/${agentId.encodePath()}",
        )
    }

    fun renameAgent(baseUrl: String, token: String, workspaceId: String, agentId: String, name: String) {
        val body = JsonObject().apply { addProperty("name", name) }
        request(
            baseUrl,
            token,
            "PATCH",
            "/api/workspaces/${workspaceId.encodePath()}/agents/${agentId.encodePath()}",
            body.toString(),
        )
    }

    fun transcript(baseUrl: String, token: String, workspaceId: String, agentId: String): TranscriptPage {
        val json = request(
            baseUrl,
            token,
            "GET",
            "/api/workspaces/${workspaceId.encodePath()}/agents/${agentId.encodePath()}/transcript",
        )
        return parseTranscript(JsonParser.parseString(json).asJsonObject)
    }

    fun voiceAsrStatus(baseUrl: String, token: String, workspaceId: String): VoiceAsrStatus {
        val json = request(
            baseUrl,
            token,
            "GET",
            "/api/workspaces/${workspaceId.encodePath()}/voice/asr",
        )
        return parseVoiceAsrStatus(JsonParser.parseString(json).asJsonObject)
    }

    fun voiceCommandStatus(baseUrl: String, token: String, workspaceId: String): VoiceCommandStatus {
        val json = request(
            baseUrl,
            token,
            "GET",
            "/api/workspaces/${workspaceId.encodePath()}/voice/command",
        )
        return parseVoiceCommandStatus(JsonParser.parseString(json).asJsonObject)
    }

    fun voiceCommand(
        baseUrl: String,
        token: String,
        workspaceId: String,
        text: String,
        history: List<VoiceLine> = emptyList(),
    ): VoiceCommandReply {
        val body = JsonObject().apply {
            addProperty("text", text)
            val turns = com.google.gson.JsonArray()
            history.takeLast(12).forEach { line ->
                if (line.role == "user" || line.role == "assistant") {
                    val item = JsonObject()
                    item.addProperty("role", line.role)
                    item.addProperty("text", line.text)
                    turns.add(item)
                }
            }
            add("history", turns)
        }
        val json = request(
            baseUrl,
            token,
            "POST",
            "/api/workspaces/${workspaceId.encodePath()}/voice/command",
            body.toString(),
        )
        return parseVoiceCommandReply(JsonParser.parseString(json).asJsonObject)
    }

    fun voiceAsrSocket(
        baseUrl: String,
        token: String,
        workspaceId: String,
        listener: WebSocketListener,
    ): WebSocket {
        val url = websocketUrl(baseUrl, "/api/workspaces/${workspaceId.encodePath()}/voice/asr/stream")
        val request = Request.Builder()
            .url(url)
            .header("Authorization", "Bearer $token")
            .build()
        return asrClient.newWebSocket(request, listener)
    }

    fun eventsSocket(
        baseUrl: String,
        token: String,
        workspaceId: String,
        listener: WebSocketListener,
    ): WebSocket {
        val url = websocketUrl(baseUrl, "/api/workspaces/${workspaceId.encodePath()}/events")
        val request = Request.Builder()
            .url(url)
            .header("Authorization", "Bearer $token")
            .build()
        return client.newWebSocket(request, listener)
    }

    fun terminalSocket(
        baseUrl: String,
        token: String,
        workspaceId: String,
        agentId: String,
        cols: Int,
        rows: Int,
        listener: WebSocketListener,
    ): WebSocket {
        val url = websocketUrl(
            baseUrl,
            "/api/workspaces/${workspaceId.encodePath()}/agents/${agentId.encodePath()}/terminal?cols=$cols&rows=$rows",
        )
        val request = Request.Builder()
            .url(url)
            .header("Authorization", "Bearer $token")
            .build()
        return client.newWebSocket(request, listener)
    }

    fun uiTunnelSocket(
        baseUrl: String,
        token: String,
        workspaceId: String,
        agentId: String,
        pathAndQuery: String,
        listener: WebSocketListener,
    ): WebSocket {
        val suffix = pathAndQuery.trimStart('/')
        val url = websocketUrl(
            baseUrl,
            "/api/workspaces/${workspaceId.encodePath()}/agents/${agentId.encodePath()}/interface/ui/$suffix",
        )
        val request = Request.Builder()
            .url(url)
            .header("Authorization", "Bearer $token")
            .header("Cookie", "treer_session=$token")
            .build()
        return client.newWebSocket(request, listener)
    }

    fun forwardUiRequest(
        baseUrl: String,
        token: String,
        workspaceId: String,
        agentId: String,
        method: String,
        pathAndQuery: String,
        headers: Map<String, String>,
        body: ByteArray?,
        contentType: String?,
    ): ForwardedResponse {
        val suffix = pathAndQuery.trimStart('/')
        val url = joinUrl(
            baseUrl,
            "/api/workspaces/${workspaceId.encodePath()}/agents/${agentId.encodePath()}/interface/ui/$suffix",
        )
        val builder = Request.Builder().url(url)
            .header("Authorization", "Bearer $token")
            .header("Cookie", "treer_session=$token")
        headers.forEach { (key, value) ->
            if (key.equals("host", true) ||
                key.equals("origin", true) ||
                key.equals("referer", true) ||
                key.equals("cookie", true) ||
                key.equals("authorization", true)
            ) {
                return@forEach
            }
            builder.header(key, value)
        }
        val requestBody: RequestBody? = if (method.equals("GET", true) || method.equals("HEAD", true)) {
            null
        } else {
            val media = (contentType ?: "application/octet-stream").toMediaTypeOrNullSafe()
            (body ?: ByteArray(0)).toRequestBody(media)
        }
        builder.method(method.uppercase(), requestBody)
        client.newCall(builder.build()).execute().use { response ->
            val bytes = response.body?.bytes() ?: ByteArray(0)
            val responseHeaders = linkedMapOf<String, String>()
            response.headers.forEach { (name, value) ->
                if (!name.equals("content-encoding", true) &&
                    !name.equals("transfer-encoding", true) &&
                    !name.equals("content-length", true)
                ) {
                    responseHeaders[name] = value
                }
            }
            val mime = response.body?.contentType()?.let { "${it.type}/${it.subtype}" }
                ?: response.header("content-type")?.substringBefore(";")?.trim()
                ?: "application/octet-stream"
            val charset = response.body?.contentType()?.charset()?.name()
            return ForwardedResponse(
                status = response.code,
                headers = responseHeaders,
                body = bytes,
                mimeType = mime,
                encoding = charset,
            )
        }
    }

    fun normalizedProxyUrl(raw: String): String {
        val trimmed = raw.trim()
        val withScheme = if (trimmed.startsWith("http://") || trimmed.startsWith("https://")) {
            trimmed
        } else {
            "http://$trimmed"
        }
        val url = withScheme.toHttpUrlOrNull() ?: throw ApiException("Enter a valid Proxy URL")
        if (url.username.isNotEmpty() || url.password.isNotEmpty()) {
            throw ApiException("Proxy URL must not include credentials")
        }
        return url.newBuilder().encodedPath("/").query(null).fragment(null).build().toString().trimEnd('/')
    }

    private fun request(
        baseUrl: String,
        token: String?,
        method: String,
        path: String,
        jsonBody: String? = null,
        nativeClient: Boolean = false,
    ): String {
        val builder = Request.Builder().url(joinUrl(baseUrl, path))
        if (token != null) {
            builder.header("Authorization", "Bearer $token")
        }
        if (nativeClient) {
            builder.header(CLIENT_HEADER, CLIENT_VALUE)
        }
        val body = jsonBody?.toRequestBody(JSON)
        builder.method(method, if (method == "GET" || method == "HEAD") null else (body ?: EMPTY))
        if (jsonBody != null) {
            builder.header("Content-Type", "application/json")
        }
        return client.newCall(builder.build()).execute().use { readJson(it) }
    }

    private fun readJson(response: Response): String {
        val body = response.body?.string().orEmpty()
        if (response.code == 401) {
            val (message, _) = parseErrorMessage(body, "authentication required")
            throw UnauthorizedException(message)
        }
        if (!response.isSuccessful) {
            val (message, code) = parseErrorMessage(body, "HTTP ${response.code}")
            throw ApiException(message, response.code, code)
        }
        return body.ifBlank { "{}" }
    }

    companion object {
        const val CLIENT_HEADER = "X-Treer-Client"
        const val CLIENT_VALUE = "mobile_android"
        private val JSON = "application/json; charset=utf-8".toMediaType()
        private val EMPTY = ByteArray(0).toRequestBody(null)
        val client: OkHttpClient = OkHttpClient.Builder()
            .connectTimeout(15, TimeUnit.SECONDS)
            .readTimeout(90, TimeUnit.SECONDS)
            .writeTimeout(30, TimeUnit.SECONDS)
            .pingInterval(20, TimeUnit.SECONDS)
            .build()
        val asrClient: OkHttpClient = OkHttpClient.Builder()
            .connectTimeout(15, TimeUnit.SECONDS)
            .readTimeout(0, TimeUnit.MILLISECONDS)
            .writeTimeout(0, TimeUnit.MILLISECONDS)
            .pingInterval(15, TimeUnit.SECONDS)
            .build()
    }
}

fun joinUrl(baseUrl: String, path: String): String {
    val base = baseUrl.trimEnd('/')
    return if (path.startsWith("http://") || path.startsWith("https://")) path else base + path
}

fun websocketUrl(baseUrl: String, path: String): String {
    val http = joinUrl(baseUrl, path)
    return when {
        http.startsWith("https://") -> "wss://" + http.removePrefix("https://")
        http.startsWith("http://") -> "ws://" + http.removePrefix("http://")
        else -> http
    }
}

private fun String.encodePath(): String = java.net.URLEncoder.encode(this, "UTF-8").replace("+", "%20")
private fun String.encodeQuery(): String = java.net.URLEncoder.encode(this, "UTF-8")

private fun String.toMediaTypeOrNullSafe() = runCatching { toMediaType() }.getOrNull()
