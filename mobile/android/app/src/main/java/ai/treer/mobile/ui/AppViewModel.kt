package ai.treer.mobile.ui

import android.app.Application
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import ai.treer.mobile.data.AppPreferences
import ai.treer.mobile.data.SessionStore
import ai.treer.mobile.data.SystemTts
import ai.treer.mobile.data.TreerApi
import ai.treer.mobile.data.latestTurnText
import ai.treer.mobile.data.parseWorkspaceEvent
import ai.treer.mobile.domain.AGENT_CATALOG
import ai.treer.mobile.domain.Agent
import ai.treer.mobile.domain.ApiException
import ai.treer.mobile.domain.AuthConfig
import ai.treer.mobile.domain.CatalogEntry
import ai.treer.mobile.domain.ConfirmAction
import ai.treer.mobile.domain.ConfirmPayload
import ai.treer.mobile.domain.ConfirmSpec
import ai.treer.mobile.domain.ConnectionState
import ai.treer.mobile.domain.LaunchProfile
import ai.treer.mobile.domain.Machine
import ai.treer.mobile.domain.Organization
import ai.treer.mobile.domain.Snapshot
import ai.treer.mobile.domain.UnauthorizedException
import ai.treer.mobile.domain.User
import ai.treer.mobile.domain.VoiceAsrStatus
import ai.treer.mobile.domain.VoiceCommandStatus
import ai.treer.mobile.domain.VoiceInputMode
import ai.treer.mobile.domain.VoiceLine
import ai.treer.mobile.domain.VoiceTtsStatus
import androidx.compose.runtime.mutableStateListOf
import ai.treer.mobile.domain.Workspace
import ai.treer.mobile.domain.classifyFleet
import ai.treer.mobile.domain.consequenceFor
import ai.treer.mobile.domain.defaultAgentName
import ai.treer.mobile.domain.defaultProfileAgentName
import ai.treer.mobile.domain.objectIdSuffix
import ai.treer.mobile.domain.preferredCreateKind
import ai.treer.mobile.domain.promptExcerpt
import ai.treer.mobile.domain.promptNeedsConfirm
import ai.treer.mobile.domain.wireAgentKind
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString

enum class MainTab { Home, Machines, Inbox }

sealed class AppScreen {
    data object ProxySetup : AppScreen()
    data object Login : AppScreen()
    data object Register : AppScreen()
    data object ForgotPassword : AppScreen()
    data object ResetPassword : AppScreen()
    data object WorkspaceSwitcher : AppScreen()
    data object Main : AppScreen()
    data object Settings : AppScreen()
    data class MachineDetail(val serverId: String) : AppScreen()
    data class AgentDetail(val agentId: String) : AppScreen()
    data class AgentUi(val agentId: String) : AppScreen()
    data class AgentTerminal(val agentId: String) : AppScreen()
    data class CreateAgent(val serverId: String?) : AppScreen()
    data object AddMachine : AppScreen()
}

class AppViewModel(application: Application) : AndroidViewModel(application) {
    val prefs = AppPreferences(application)
    val sessions = SessionStore(application)
    val api = TreerApi()

    var screen by mutableStateOf<AppScreen>(AppScreen.ProxySetup)
        private set
    var tab by mutableStateOf(MainTab.Home)
        private set
    var busy by mutableStateOf(false)
        private set
    var error by mutableStateOf<String?>(null)
        private set
    var stale by mutableStateOf(false)
        private set
    var connection by mutableStateOf(ConnectionState.Offline)
        private set

    var proxyUrl by mutableStateOf(prefs.proxyUrl)
    var user by mutableStateOf<User?>(null)
        private set
    var authConfig by mutableStateOf<AuthConfig?>(null)
        private set

    var organizations by mutableStateOf<List<Organization>>(emptyList())
        private set
    var workspaces by mutableStateOf<List<Workspace>>(emptyList())
        private set
    var selectedOrg by mutableStateOf<Organization?>(null)
        private set
    var selectedWorkspace by mutableStateOf<Workspace?>(null)
        private set
    var snapshot by mutableStateOf(Snapshot())
        private set
    var profiles by mutableStateOf<List<LaunchProfile>>(emptyList())
        private set
    var transcriptPreview by mutableStateOf<String?>(null)
        private set
    var transcriptError by mutableStateOf<String?>(null)
        private set

    var confirm by mutableStateOf<ConfirmSpec?>(null)
        private set
    var voiceOpen by mutableStateOf(false)
        private set
    var voiceAsr by mutableStateOf(VoiceAsrStatus())
        private set
    var voiceCommand by mutableStateOf(VoiceCommandStatus())
        private set
    var voiceReply by mutableStateOf<String?>(null)
        private set
    var voiceBusy by mutableStateOf(false)
        private set
    var voiceSpeaking by mutableStateOf(false)
        private set
    var voiceTtsStatus by mutableStateOf(VoiceTtsStatus())
        private set
    var voiceMode by mutableStateOf(VoiceInputMode.Hold)
        private set
    val voiceLines = mutableStateListOf<VoiceLine>()
    private val voiceTts = SystemTts(
        application,
        onStatus = { voiceTtsStatus = it },
        onSpeaking = { voiceSpeaking = it },
    )
    var composerDraft by mutableStateOf("")
    var createName by mutableStateOf("")
    var createPrompt by mutableStateOf("")
    var createKind by mutableStateOf("terminal")
    var createProfileId by mutableStateOf<String?>(null)
    var createServerId by mutableStateOf<String?>(null)
    var bootstrapInstall by mutableStateOf("")
    var bootstrapConnect by mutableStateOf("")
    var resetToken by mutableStateOf("")
    var profileNameDraft by mutableStateOf("")
    var profileEmailDraft by mutableStateOf("")
    var proxyDraft by mutableStateOf("")
    var theme by mutableStateOf(prefs.theme)

    private val backStack = ArrayDeque<AppScreen>()
    private var eventsSocket: WebSocket? = null
    private var eventsJob: Job? = null
    private var reconnectAttempt = 0

    val fleet get() = classifyFleet(snapshot)
    val token: String? get() = user?.token ?: sessions.token()
    val baseUrl: String get() = proxyUrl.ifBlank { prefs.proxyUrl }

    init {
        restore()
    }

    fun restore() {
        proxyUrl = prefs.proxyUrl
        proxyDraft = prefs.proxyUrl
        val storedToken = sessions.token()
        if (proxyUrl.isBlank()) {
            screen = AppScreen.ProxySetup
            return
        }
        if (storedToken.isNullOrBlank()) {
            screen = AppScreen.Login
            loadAuthConfig()
            return
        }
        user = User(
            userId = sessions.userId().orEmpty(),
            email = sessions.email().orEmpty(),
            preferredName = sessions.preferredName().orEmpty(),
            token = storedToken,
        )
        profileNameDraft = user?.preferredName.orEmpty()
        profileEmailDraft = user?.email.orEmpty()
        viewModelScope.launch {
            try {
                val me = withContext(Dispatchers.IO) { api.me(baseUrl, storedToken) }
                applyUser(me.copy(token = storedToken))
                enterAfterAuth()
            } catch (ex: UnauthorizedException) {
                sessions.clear()
                user = null
                screen = AppScreen.Login
                loadAuthConfig()
            } catch (ex: Exception) {
                error = ex.message
                enterAfterAuth()
            }
        }
    }

    fun continueProxy(raw: String) {
        viewModelScope.launch {
            busy = true
            error = null
            try {
                val normalized = api.normalizedProxyUrl(raw)
                val health = withContext(Dispatchers.IO) { api.health(normalized) }
                val service = health.get("service")?.asString
                if (service != null && service != "treer-proxy") {
                    throw ApiException("That URL is not a Treer Proxy")
                }
                proxyUrl = normalized
                prefs.proxyUrl = normalized
                proxyDraft = normalized
                loadAuthConfig()
                screen = AppScreen.Login
            } catch (ex: Exception) {
                error = ex.message ?: "Unable to reach Proxy"
            } finally {
                busy = false
            }
        }
    }

    fun loadAuthConfig() {
        val url = baseUrl
        if (url.isBlank()) return
        viewModelScope.launch {
            authConfig = runCatching {
                withContext(Dispatchers.IO) { api.authConfig(url) }
            }.getOrNull()
        }
    }

    fun login(email: String, password: String) {
        viewModelScope.launch {
            busy = true
            error = null
            try {
                val result = withContext(Dispatchers.IO) {
                    api.login(baseUrl, email, password, prefs.deviceId, prefs.deviceName)
                }
                val token = result.token ?: throw ApiException("Proxy did not return a native session token")
                applyUser(result.copy(token = token))
                sessions.save(token, result.userId, result.email, result.preferredName)
                enterAfterAuth()
            } catch (ex: Exception) {
                error = ex.message ?: "Login failed"
            } finally {
                busy = false
            }
        }
    }

    fun register(email: String, preferredName: String, password: String, invite: String?) {
        viewModelScope.launch {
            busy = true
            error = null
            try {
                val result = withContext(Dispatchers.IO) {
                    api.register(baseUrl, email, preferredName, password, invite, prefs.deviceId, prefs.deviceName)
                }
                val token = result.token ?: throw ApiException("Proxy did not return a native session token")
                applyUser(result.copy(token = token))
                sessions.save(token, result.userId, result.email, result.preferredName)
                enterAfterAuth()
            } catch (ex: Exception) {
                error = ex.message ?: "Register failed"
            } finally {
                busy = false
            }
        }
    }

    fun requestPasswordReset(email: String) {
        viewModelScope.launch {
            busy = true
            error = null
            try {
                withContext(Dispatchers.IO) { api.requestPasswordReset(baseUrl, email) }
                error = "If you do not receive email, ask the person who deployed this Proxy to confirm sending is configured."
            } catch (ex: Exception) {
                error = ex.message
            } finally {
                busy = false
            }
        }
    }

    fun resetPassword(token: String, password: String) {
        viewModelScope.launch {
            busy = true
            error = null
            try {
                withContext(Dispatchers.IO) { api.resetPassword(baseUrl, token, password) }
                screen = AppScreen.Login
                error = "Password updated. Sign in."
            } catch (ex: Exception) {
                error = ex.message
            } finally {
                busy = false
            }
        }
    }

    fun go(target: AppScreen, push: Boolean = true) {
        if (push && screen != target) {
            backStack.addLast(screen)
        }
        screen = target
        error = null
        if (target is AppScreen.AgentDetail) {
            loadTranscript(target.agentId)
        }
        if (target is AppScreen.CreateAgent) {
            createServerId = target.serverId ?: snapshot.servers.firstOrNull { it.isOnline }?.serverId
            createProfileId = null
            createKind = preferredCreateKind(createServerId?.let { snapshot.machine(it) })
            createName = defaultAgentName(createKind)
            createPrompt = ""
            loadProfiles()
        }
        if (target is AppScreen.AddMachine) {
            loadBootstrap()
        }
        if (target is AppScreen.Settings) {
            profileNameDraft = user?.preferredName.orEmpty()
            profileEmailDraft = user?.email.orEmpty()
            proxyDraft = baseUrl
        }
    }

    fun back() {
        val previous = backStack.removeLastOrNull()
        if (previous != null) {
            screen = previous
        } else if (screen !is AppScreen.Main && user != null && selectedWorkspace != null) {
            screen = AppScreen.Main
        }
    }

    fun selectTab(next: MainTab) {
        tab = next
        prefs.lastTab = next.name.lowercase()
        if (screen !is AppScreen.Main) {
            backStack.clear()
            screen = AppScreen.Main
        }
    }

    fun openVoice() {
        voiceOpen = true
        voiceBusy = false
        loadVoiceAsr()
    }

    fun closeVoice() {
        voiceTts.stop()
        voiceOpen = false
        voiceBusy = false
    }

    fun stopVoiceSpeech() {
        voiceTts.stop()
    }

    fun selectVoiceMode(mode: VoiceInputMode) {
        if (mode != voiceMode) {
            stopVoiceSpeech()
            voiceMode = mode
        }
    }

    fun installTtsEngine() = voiceTts.installEngine()

    fun installTtsVoiceData() = voiceTts.installVoiceData()

    fun openTtsSettings() = voiceTts.openTtsSettings()

    fun selectOrganization(org: Organization) {
        selectedOrg = org
        prefs.lastOrganizationId = org.organizationId
        viewModelScope.launch { loadWorkspaces(org.organizationId) }
    }

    fun selectWorkspace(workspace: Workspace) {
        selectedWorkspace = workspace
        prefs.lastWorkspaceId = workspace.workspaceId
        backStack.clear()
        screen = AppScreen.Main
        connectWorkspace()
    }

    fun createWorkspace(name: String) {
        val org = selectedOrg ?: return
        viewModelScope.launch {
            busy = true
            error = null
            try {
                val created = withContext(Dispatchers.IO) {
                    api.createWorkspace(baseUrl, requireToken(), org.organizationId, name)
                }
                workspaces = workspaces + created
                selectWorkspace(created)
            } catch (ex: Exception) {
                handleAuth(ex)
            } finally {
                busy = false
            }
        }
    }

    fun refreshSnapshot() {
        connectWorkspace(force = true)
    }

    fun reloadSnapshot() {
        val workspaceId = selectedWorkspace?.workspaceId ?: return
        val token = token ?: return
        viewModelScope.launch {
            runCatching {
                snapshot = withContext(Dispatchers.IO) { api.snapshot(baseUrl, token, workspaceId) }
                stale = false
                connection = ConnectionState.Live
            }
        }
    }

    fun openAgent(agentId: String) {
        go(AppScreen.AgentDetail(agentId))
    }

    fun openAgentUi(agentId: String) {
        go(AppScreen.AgentUi(agentId))
    }

    fun openTerminal(agentId: String) {
        go(AppScreen.AgentTerminal(agentId))
    }

    fun sendFollowUp(agentId: String, text: String, force: Boolean = false) {
        val agent = snapshot.agent(agentId) ?: return
        val machine = snapshot.machine(agent.serverId)
        if (!force && promptNeedsConfirm(text, agent.status)) {
            confirm = ConfirmSpec(
                action = ConfirmAction.Prompt,
                title = "Send follow-up",
                objectName = agent.name,
                objectIdSuffix = objectIdSuffix(agent.agentId),
                machineHostname = machine?.displayName,
                promptExcerpt = promptExcerpt(text),
                consequence = consequenceFor(ConfirmAction.Prompt, machine = machine?.displayName.orEmpty(), name = agent.name),
                payload = ConfirmPayload.Prompt(agentId, text),
            )
            return
        }
        viewModelScope.launch { runPrompt(agentId, text) }
    }

    fun requestCreate() {
        val serverId = createServerId ?: return
        val machine = snapshot.machine(serverId)
        val profile = profiles.firstOrNull { it.profileId == createProfileId }
        val name = createName.ifBlank {
            profile?.let { defaultProfileAgentName(it.name) } ?: defaultAgentName(createKind)
        }
        val kindLabel = profile?.name ?: createKind
        val prompt = createPrompt.trim().ifBlank { null }
        confirm = ConfirmSpec(
            action = if (profile != null) ConfirmAction.Launch else ConfirmAction.Create,
            title = if (profile != null) "Start ${profile.name}" else "Start $kindLabel",
            objectName = name,
            objectIdSuffix = objectIdSuffix(profile?.profileId ?: serverId),
            machineHostname = machine?.displayName,
            promptExcerpt = prompt?.let(::promptExcerpt),
            consequence = consequenceFor(
                if (profile != null) ConfirmAction.Launch else ConfirmAction.Create,
                kindOrProfile = kindLabel,
                machine = machine?.displayName.orEmpty(),
                name = name,
            ),
            payload = if (profile != null) {
                ConfirmPayload.Launch(profile.profileId, serverId, name, prompt)
            } else {
                ConfirmPayload.Create(serverId, createKind, name, prompt, null)
            },
        )
    }

    fun requestAbort(agent: Agent) {
        val machine = snapshot.machine(agent.serverId)
        confirm = ConfirmSpec(
            action = ConfirmAction.Abort,
            title = "Abort this turn",
            objectName = agent.name,
            objectIdSuffix = objectIdSuffix(agent.agentId),
            machineHostname = machine?.displayName,
            consequence = consequenceFor(ConfirmAction.Abort),
            payload = ConfirmPayload.Abort(agent.agentId),
        )
    }

    fun requestStop(agent: Agent) {
        val machine = snapshot.machine(agent.serverId)
        confirm = ConfirmSpec(
            action = ConfirmAction.Stop,
            title = "Stop ${agent.name}",
            objectName = agent.name,
            objectIdSuffix = objectIdSuffix(agent.agentId),
            machineHostname = machine?.displayName,
            consequence = consequenceFor(ConfirmAction.Stop),
            payload = ConfirmPayload.Stop(agent.agentId),
        )
    }

    fun requestDelete(agent: Agent) {
        val machine = snapshot.machine(agent.serverId)
        confirm = ConfirmSpec(
            action = ConfirmAction.Delete,
            title = "Delete ${agent.name}",
            objectName = agent.name,
            objectIdSuffix = objectIdSuffix(agent.agentId),
            machineHostname = machine?.displayName,
            consequence = consequenceFor(ConfirmAction.Delete),
            payload = ConfirmPayload.Delete(agent.agentId),
        )
    }

    fun requestLogout() {
        confirm = ConfirmSpec(
            action = ConfirmAction.Logout,
            title = "Sign out",
            objectName = baseUrl,
            consequence = consequenceFor(ConfirmAction.Logout),
            payload = ConfirmPayload.Logout,
            showChange = false,
        )
    }

    fun requestSwitchProxy(newUrl: String) {
        confirm = ConfirmSpec(
            action = ConfirmAction.SwitchProxy,
            title = "Switch Proxy",
            objectName = baseUrl,
            consequence = consequenceFor(ConfirmAction.SwitchProxy),
            payload = ConfirmPayload.SwitchProxy(newUrl),
        )
    }

    fun cancelConfirm() {
        confirm = null
    }

    fun changeConfirm() {
        confirm = null
    }

    fun executeConfirm() {
        val spec = confirm ?: return
        confirm = null
        viewModelScope.launch {
            try {
                when (val payload = spec.payload) {
                    is ConfirmPayload.Create -> {
                        val agent = withContext(Dispatchers.IO) {
                            api.createAgent(
                                baseUrl,
                                requireToken(),
                                requireWorkspace(),
                                payload.serverId,
                                wireAgentKind(payload.kind),
                                payload.name,
                            )
                        }
                        if (!payload.prompt.isNullOrBlank()) {
                            withContext(Dispatchers.IO) {
                                api.promptAgent(baseUrl, requireToken(), requireWorkspace(), agent.agentId, payload.prompt)
                            }
                        }
                        go(AppScreen.AgentDetail(agent.agentId), push = false)
                    }
                    is ConfirmPayload.Launch -> {
                        val agent = withContext(Dispatchers.IO) {
                            api.launchProfile(baseUrl, requireToken(), requireWorkspace(), payload.profileId, payload.serverId, payload.name)
                        }
                        if (!payload.prompt.isNullOrBlank()) {
                            withContext(Dispatchers.IO) {
                                api.promptAgent(baseUrl, requireToken(), requireWorkspace(), agent.agentId, payload.prompt)
                            }
                        }
                        go(AppScreen.AgentDetail(agent.agentId), push = false)
                    }
                    is ConfirmPayload.Prompt -> runPrompt(payload.agentId, payload.text)
                    is ConfirmPayload.Abort -> withContext(Dispatchers.IO) {
                        api.abortAgent(baseUrl, requireToken(), requireWorkspace(), payload.agentId)
                    }
                    is ConfirmPayload.Stop -> withContext(Dispatchers.IO) {
                        api.stopAgent(baseUrl, requireToken(), requireWorkspace(), payload.agentId)
                    }
                    is ConfirmPayload.Delete -> {
                        withContext(Dispatchers.IO) {
                            api.deleteAgent(baseUrl, requireToken(), requireWorkspace(), payload.agentId)
                        }
                        screen = AppScreen.Main
                    }
                    is ConfirmPayload.SwitchProxy -> performSwitchProxy(payload.newUrl)
                    ConfirmPayload.Logout -> performLogout()
                }
            } catch (ex: Exception) {
                handleAuth(ex)
            }
        }
    }

    fun saveProfile() {
        viewModelScope.launch {
            busy = true
            error = null
            try {
                val updated = withContext(Dispatchers.IO) {
                    api.updateProfile(baseUrl, requireToken(), profileEmailDraft, profileNameDraft)
                }
                applyUser(updated.copy(token = requireToken()))
            } catch (ex: Exception) {
                handleAuth(ex)
            } finally {
                busy = false
            }
        }
    }

    fun rename(agentId: String, name: String) {
        viewModelScope.launch {
            try {
                withContext(Dispatchers.IO) {
                    api.renameAgent(baseUrl, requireToken(), requireWorkspace(), agentId, name)
                }
            } catch (ex: Exception) {
                handleAuth(ex)
            }
        }
    }

    fun sortedCreateOptions(machine: Machine?): Pair<List<LaunchProfile>, List<CatalogEntry>> {
        val ais = profiles.filter { it.looksAisCapable }.sortedBy { it.name.lowercase() }
        val tuiProfiles = profiles.filter { !it.looksAisCapable }.sortedBy { it.name.lowercase() }
        val kinds = if (machine?.availableAgents != null) {
            AGENT_CATALOG.filter { machine.availableAgents.contains(it.kind) }
        } else {
            AGENT_CATALOG
        }
        return (ais + tuiProfiles) to kinds
    }

    override fun onCleared() {
        eventsSocket?.close(1000, "leave")
        eventsSocket = null
        voiceTts.shutdown()
        super.onCleared()
    }

    private fun applyUser(next: User) {
        user = next
        profileNameDraft = next.preferredName
        profileEmailDraft = next.email
        sessions.save(next.token.orEmpty(), next.userId, next.email, next.preferredName)
    }

    private suspend fun enterAfterAuth() {
        try {
            organizations = withContext(Dispatchers.IO) { api.organizations(baseUrl, requireToken()) }
            val lastOrgId = prefs.lastOrganizationId
            val org = organizations.firstOrNull { it.organizationId == lastOrgId } ?: organizations.firstOrNull()
            if (org == null) {
                screen = AppScreen.WorkspaceSwitcher
                return
            }
            selectedOrg = org
            loadWorkspaces(org.organizationId)
            val lastWorkspaceId = prefs.lastWorkspaceId
            val workspace = workspaces.firstOrNull { it.workspaceId == lastWorkspaceId }
            if (workspace != null) {
                selectedWorkspace = workspace
                screen = AppScreen.Main
                tab = when (prefs.lastTab) {
                    "machines" -> MainTab.Machines
                    "inbox" -> MainTab.Inbox
                    else -> MainTab.Home
                }
                connectWorkspace()
            } else {
                screen = AppScreen.WorkspaceSwitcher
            }
        } catch (ex: Exception) {
            handleAuth(ex)
            if (screen !is AppScreen.Login) {
                screen = AppScreen.WorkspaceSwitcher
            }
        }
    }

    private suspend fun loadWorkspaces(organizationId: String) {
        workspaces = withContext(Dispatchers.IO) { api.workspaces(baseUrl, requireToken(), organizationId) }
    }

    fun loadVoiceAsr() {
        val workspaceId = selectedWorkspace?.workspaceId ?: return
        val token = token ?: return
        viewModelScope.launch {
            voiceAsr = runCatching {
                withContext(Dispatchers.IO) { api.voiceAsrStatus(baseUrl, token, workspaceId) }
            }.getOrElse { VoiceAsrStatus() }
            voiceCommand = runCatching {
                withContext(Dispatchers.IO) { api.voiceCommandStatus(baseUrl, token, workspaceId) }
            }.getOrElse { VoiceCommandStatus() }
        }
    }

    fun submitVoiceUtterance(text: String) {
        val workspaceId = selectedWorkspace?.workspaceId ?: return
        val token = token ?: return
        val trimmed = text.trim()
        if (trimmed.isEmpty()) return
        val history = voiceLines.toList()
        voiceLines.add(VoiceLine("user", trimmed))
        voiceBusy = true
        viewModelScope.launch {
            if (!voiceCommand.enabled) {
                loadVoiceAsr()
            }
            val reply = runCatching {
                withContext(Dispatchers.IO) {
                    api.voiceCommand(baseUrl, token, workspaceId, trimmed, history).reply
                }
            }.getOrElse { it.message ?: "command failed" }
            val spoken = reply.ifBlank { "（没有回复）" }
            voiceLines.add(VoiceLine("assistant", spoken))
            voiceReply = spoken
            voiceBusy = false
            voiceTts.speak(spoken)
        }
    }

    private fun loadProfiles() {
        val workspaceId = selectedWorkspace?.workspaceId ?: return
        viewModelScope.launch {
            profiles = runCatching {
                withContext(Dispatchers.IO) { api.launchProfiles(baseUrl, requireToken(), workspaceId) }
            }.getOrElse { emptyList() }
        }
    }

    fun loadBootstrap() {
        val workspaceId = selectedWorkspace?.workspaceId ?: return
        viewModelScope.launch {
            busy = true
            error = null
            try {
                val info = withContext(Dispatchers.IO) {
                    api.bootstrap(baseUrl, requireToken(), workspaceId)
                }
                bootstrapInstall = info.installCommand
                bootstrapConnect = info.connectCommand
            } catch (ex: Exception) {
                handleAuth(ex)
            } finally {
                busy = false
            }
        }
    }

    private fun loadTranscript(agentId: String) {
        val workspaceId = selectedWorkspace?.workspaceId ?: return
        val agent = snapshot.agent(agentId)
        transcriptPreview = null
        transcriptError = null
        if (agent?.supports("transcript.read") != true) {
            transcriptPreview = "output revision ${agent?.outputRevision ?: 0}"
            return
        }
        viewModelScope.launch {
            try {
                val page = withContext(Dispatchers.IO) {
                    api.transcript(baseUrl, requireToken(), workspaceId, agentId)
                }
                transcriptPreview = latestTurnText(page) ?: "Waiting for the Agent to become ready."
            } catch (ex: Exception) {
                transcriptError = ex.message
                transcriptPreview = "output revision ${agent.outputRevision}"
            }
        }
    }

    private fun connectWorkspace(force: Boolean = false) {
        val workspaceId = selectedWorkspace?.workspaceId ?: return
        val token = token ?: return
        eventsSocket?.close(1000, "refresh")
        eventsSocket = null
        eventsJob?.cancel()
        viewModelScope.launch {
            try {
                snapshot = withContext(Dispatchers.IO) { api.snapshot(baseUrl, token, workspaceId) }
                stale = false
                connection = ConnectionState.Live
                loadProfiles()
                loadVoiceAsr()
            } catch (ex: Exception) {
                handleAuth(ex)
                connection = ConnectionState.Offline
                stale = snapshot.revision > 0
            }
        }
        reconnectAttempt = if (force) 0 else reconnectAttempt
        eventsJob = viewModelScope.launch {
            delay(150)
            openEvents(workspaceId, token)
        }
    }

    private fun openEvents(workspaceId: String, token: String) {
        connection = if (connection == ConnectionState.Offline) ConnectionState.Reconnecting else connection
        eventsSocket = api.eventsSocket(baseUrl, token, workspaceId, object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                viewModelScope.launch {
                    reconnectAttempt = 0
                    connection = ConnectionState.Live
                    stale = false
                }
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                viewModelScope.launch { applyEvent(text) }
            }

            override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
                viewModelScope.launch { applyEvent(bytes.utf8()) }
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                viewModelScope.launch { scheduleReconnect(workspaceId, token) }
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                if (code != 1000) {
                    viewModelScope.launch { scheduleReconnect(workspaceId, token) }
                }
            }
        })
    }

    private fun scheduleReconnect(workspaceId: String, token: String) {
        connection = ConnectionState.Reconnecting
        stale = true
        eventsJob?.cancel()
        eventsJob = viewModelScope.launch {
            val delayMs = (1000L * (1 shl reconnectAttempt.coerceAtMost(4))).coerceAtMost(10_000L)
            reconnectAttempt += 1
            delay(delayMs)
            openEvents(workspaceId, token)
        }
    }

    private fun applyEvent(text: String) {
        val event = runCatching { parseWorkspaceEvent(text) }.getOrNull() ?: return
        when (event.event) {
            "workspace.snapshot" -> event.data?.let { snapshot = it }
            "agent.updated" -> event.agent?.let { updated ->
                snapshot = snapshot.copy(
                    revision = event.revision.takeIf { it > 0 } ?: snapshot.revision,
                    agents = snapshot.agents.map { if (it.agentId == updated.agentId) updated else it }.let { list ->
                        if (list.any { it.agentId == updated.agentId }) list else list + updated
                    },
                )
            }
            "agent.deleted" -> {
                val agentId = event.agent?.agentId
                if (agentId != null) {
                    snapshot = snapshot.copy(agents = snapshot.agents.filterNot { it.agentId == agentId })
                }
            }
            "server.offline", "server.online" -> {
                val serverId = event.serverId ?: return
                val status = if (event.event == "server.online") "online" else "offline"
                snapshot = snapshot.copy(
                    servers = snapshot.servers.map {
                        if (it.serverId == serverId) it.copy(status = status) else it
                    },
                )
            }
        }
    }

    private suspend fun runPrompt(agentId: String, text: String) {
        busy = true
        error = null
        try {
            withContext(Dispatchers.IO) {
                api.promptAgent(baseUrl, requireToken(), requireWorkspace(), agentId, text)
            }
            composerDraft = ""
        } catch (ex: Exception) {
            handleAuth(ex)
        } finally {
            busy = false
        }
    }

    private fun performLogout() {
        val current = token
        viewModelScope.launch {
            if (current != null) {
                withContext(Dispatchers.IO) { api.logout(baseUrl, current) }
            }
            eventsSocket?.close(1000, "logout")
            eventsSocket = null
            sessions.clear()
            user = null
            selectedWorkspace = null
            snapshot = Snapshot()
            prefs.clearWorkspaceMemory()
            screen = AppScreen.Login
        }
    }

    private fun performSwitchProxy(newUrl: String) {
        viewModelScope.launch {
            val current = token
            if (current != null) {
                runCatching { withContext(Dispatchers.IO) { api.logout(baseUrl, current) } }
            }
            eventsSocket?.close(1000, "switch")
            eventsSocket = null
            sessions.clear()
            user = null
            selectedWorkspace = null
            snapshot = Snapshot()
            prefs.clearProxy()
            val normalized = runCatching { api.normalizedProxyUrl(newUrl) }.getOrElse {
                error = it.message
                return@launch
            }
            proxyUrl = ""
            proxyDraft = normalized
            prefs.proxyUrl = ""
            screen = AppScreen.ProxySetup
            error = null
            continueProxy(normalized)
        }
    }

    private fun handleAuth(ex: Exception) {
        if (ex is UnauthorizedException) {
            sessions.clear()
            user = null
            screen = AppScreen.Login
            error = "Session expired. Sign in again."
        } else {
            error = ex.message ?: "Request failed"
        }
    }

    private fun requireToken(): String = token ?: throw UnauthorizedException()
    private fun requireWorkspace(): String = selectedWorkspace?.workspaceId ?: throw ApiException("No workspace selected")
}
