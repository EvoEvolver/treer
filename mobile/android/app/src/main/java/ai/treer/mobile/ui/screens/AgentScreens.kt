package ai.treer.mobile.ui.screens

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import ai.treer.mobile.domain.defaultAgentName
import ai.treer.mobile.domain.defaultProfileAgentName
import ai.treer.mobile.ui.AppScreen
import ai.treer.mobile.ui.AppViewModel
import ai.treer.mobile.ui.components.CapabilityBadge
import ai.treer.mobile.ui.components.ErrorBanner
import ai.treer.mobile.ui.components.StatusDot
import ai.treer.mobile.ui.components.relativeTime

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun AgentDetailScreen(vm: AppViewModel, agentId: String) {
    val agent = vm.snapshot.agent(agentId)
    val context = LocalContext.current
    if (agent == null) {
        Text("Agent is not in the current snapshot.", modifier = Modifier.padding(16.dp))
        return
    }
    val machine = vm.snapshot.machine(agent.serverId)
    val status = agent.displayStatus(machine)
    val canPrompt = agent.supports("prompt.submit") || true
    val canAbort = agent.supports("abort")
    val showTerminal = vm.prefs.showTerminalControls || (!agent.hasUiPath && !agent.supports("prompt.submit"))
    var rename by rememberSaveable(agent.agentId, agent.name) { mutableStateOf(agent.name) }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp)
            .testTag("agent-detail"),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            StatusDot(status)
            Text(agent.name, style = MaterialTheme.typography.headlineSmall)
        }
        Text("${agent.kind} · ${machine?.displayName ?: agent.serverId} · $status")
        Text(
            agent.agentId,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.clickable {
                val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                clipboard.setPrimaryClip(ClipData.newPlainText("agent_id", agent.agentId))
            },
        )
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            CapabilityBadge("prompt.submit", agent.supports("prompt.submit"))
            CapabilityBadge("transcript.read", agent.supports("transcript.read"))
            CapabilityBadge("state.observe", agent.supports("state.observe"))
            CapabilityBadge("abort", canAbort)
            CapabilityBadge("ui_path", agent.hasUiPath)
        }
        Text(vm.transcriptPreview ?: relativeTime(agent.updatedAt) ?: "Waiting for the Agent to become ready.")
        vm.transcriptError?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
        ErrorBanner(vm.error)
        if (status == "offline") {
            Text("This machine is offline. Composer is disabled until it reconnects.")
        } else {
            OutlinedTextField(
                value = vm.composerDraft,
                onValueChange = { vm.composerDraft = it },
                label = { Text("Follow-up") },
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag("follow-up-field"),
                minLines = 3,
            )
            Button(
                onClick = { vm.sendFollowUp(agent.agentId, vm.composerDraft) },
                enabled = canPrompt && vm.composerDraft.isNotBlank() && !vm.busy,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag("send-follow-up"),
            ) { Text("Send") }
        }
        if (agent.hasUiPath) {
            Button(onClick = { vm.openAgentUi(agent.agentId) }, modifier = Modifier.fillMaxWidth()) {
                Text("Open Agent UI")
            }
        }
        OutlinedButton(
            onClick = { vm.requestAbort(agent) },
            enabled = canAbort,
            modifier = Modifier.fillMaxWidth(),
        ) { Text(if (canAbort) "Abort this turn" else "Abort unavailable") }
        OutlinedButton(onClick = { vm.requestStop(agent) }, modifier = Modifier.fillMaxWidth()) { Text("Stop process") }
        OutlinedButton(onClick = { vm.requestDelete(agent) }, modifier = Modifier.fillMaxWidth()) { Text("Delete Agent") }
        if (showTerminal) {
            OutlinedButton(onClick = { vm.openTerminal(agent.agentId) }, modifier = Modifier.fillMaxWidth()) {
                Text("Open terminal")
            }
        }
        OutlinedTextField(value = rename, onValueChange = { rename = it }, label = { Text("Rename") }, modifier = Modifier.fillMaxWidth())
        TextButton(onClick = { vm.rename(agent.agentId, rename.trim()) }, enabled = rename.isNotBlank() && rename != agent.name) {
            Text("Save name")
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun CreateAgentScreen(vm: AppViewModel, serverId: String?) {
    val machines = vm.snapshot.servers.filter { it.isOnline }
    val selectedId = vm.createServerId ?: serverId ?: machines.firstOrNull()?.serverId
    val machine = selectedId?.let { vm.snapshot.machine(it) }
    val (profiles, kinds) = vm.sortedCreateOptions(machine)
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text("Assign work", style = MaterialTheme.typography.headlineSmall)
        ErrorBanner(vm.error)
        if (machines.isEmpty()) {
            Text(
                if (vm.snapshot.servers.isEmpty()) {
                    "No machines enrolled yet. Add one, then Assign can start an Agent."
                } else {
                    "No online machines. Recover a Host from Machines, or add another."
                },
            )
            Button(
                onClick = { vm.go(AppScreen.AddMachine) },
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag("add-machine-cta"),
            ) { Text("Add a machine") }
            return
        }
        Text("Machine", style = MaterialTheme.typography.titleSmall)
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            machines.forEach { item ->
                FilterChip(
                    selected = item.serverId == selectedId,
                    onClick = { vm.createServerId = item.serverId },
                    label = { Text(item.displayName) },
                )
            }
        }
        Text("What to run · AIS first", style = MaterialTheme.typography.titleSmall)
        profiles.forEach { profile ->
            FilterChip(
                selected = vm.createProfileId == profile.profileId,
                onClick = {
                    vm.createProfileId = profile.profileId
                    vm.createKind = profile.commandName
                    vm.createName = defaultProfileAgentName(profile.name)
                },
                label = { Text(profile.name + if (profile.looksAisCapable) " · AIS" else " · TUI") },
                modifier = Modifier.testTag("create-profile-${profile.profileId}"),
            )
        }
        FilterChip(
            selected = vm.createProfileId == null && vm.createKind == "terminal",
            onClick = {
                vm.createProfileId = null
                vm.createKind = "terminal"
                vm.createName = defaultAgentName("terminal")
            },
            label = { Text("Terminal") },
            modifier = Modifier.testTag("create-kind-terminal"),
        )
        kinds.forEach { entry ->
            val installed = machine?.availableAgents?.contains(entry.kind)
            FilterChip(
                selected = vm.createProfileId == null && vm.createKind == entry.kind,
                onClick = {
                    vm.createProfileId = null
                    vm.createKind = entry.kind
                    vm.createName = defaultAgentName(entry.kind)
                },
                enabled = installed != false,
                label = { Text(entry.label + if (installed == false) " · install on machine" else " · TUI") },
                modifier = Modifier.testTag("create-kind-${entry.kind}"),
            )
        }
        OutlinedTextField(
            value = vm.createName,
            onValueChange = { vm.createName = it },
            label = { Text("Name") },
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = vm.createPrompt,
            onValueChange = { vm.createPrompt = it },
            label = { Text("Optional first prompt") },
            modifier = Modifier.fillMaxWidth(),
            minLines = 3,
        )
        Button(
            onClick = { vm.requestCreate() },
            modifier = Modifier
                .fillMaxWidth()
                .testTag("create-submit"),
            enabled = selectedId != null,
        ) {
            Text(if (vm.createPrompt.isBlank()) "Create" else "Create & prompt")
        }
    }
}

@Composable
fun SettingsScreen(vm: AppViewModel) {
    var terminal by remember { mutableStateOf(vm.prefs.showTerminalControls) }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("Settings", style = MaterialTheme.typography.headlineSmall)
        ErrorBanner(vm.error)
        Text("Proxy", style = MaterialTheme.typography.titleSmall)
        Text(vm.baseUrl)
        OutlinedTextField(
            value = vm.proxyDraft,
            onValueChange = { vm.proxyDraft = it },
            label = { Text("Proxy URL") },
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedButton(
            onClick = { vm.requestSwitchProxy(vm.proxyDraft) },
            enabled = vm.proxyDraft.isNotBlank() && vm.proxyDraft.trim().trimEnd('/') != vm.baseUrl,
            modifier = Modifier.fillMaxWidth(),
        ) { Text("Switch Proxy") }
        Text("Account", style = MaterialTheme.typography.titleSmall)
        OutlinedTextField(value = vm.profileNameDraft, onValueChange = { vm.profileNameDraft = it }, label = { Text("Preferred name") }, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(value = vm.profileEmailDraft, onValueChange = { vm.profileEmailDraft = it }, label = { Text("Email") }, modifier = Modifier.fillMaxWidth())
        Button(onClick = { vm.saveProfile() }, modifier = Modifier.fillMaxWidth()) { Text("Save profile") }
        OutlinedButton(onClick = { vm.requestLogout() }, modifier = Modifier.fillMaxWidth()) { Text("Sign out") }
        Text("Notifications", style = MaterialTheme.typography.titleSmall)
        Text(
            "Notifications unavailable until this Proxy has APNs/FCM configured",
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text("Voice", style = MaterialTheme.typography.titleSmall)
        Text(
            if (vm.voiceAsr.enabled) {
                if (vm.voiceCommand.enabled) {
                    "Hold-to-talk ASR is on (${vm.voiceAsr.provider ?: "qwen"}). Spoken text is sent to Treer command on this Proxy."
                } else {
                    "Hold-to-talk ASR is on (${vm.voiceAsr.provider ?: "qwen"}). Command LLM is off until TREER_VOICE_LLM_API_KEY is set on the Proxy."
                }
            } else {
                "Voice Live unavailable until this Proxy has a voice provider"
            },
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text("Appearance", style = MaterialTheme.typography.titleSmall)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            listOf("system" to "System", "light" to "Light", "dark" to "Dark").forEach { (key, label) ->
                FilterChip(
                    selected = vm.theme == key,
                    onClick = {
                        vm.theme = key
                        vm.prefs.theme = key
                    },
                    label = { Text(label) },
                )
            }
        }
        Text("Advanced", style = MaterialTheme.typography.titleSmall)
        FilterChip(
            selected = terminal,
            onClick = {
                terminal = !terminal
                vm.prefs.showTerminalControls = terminal
            },
            label = { Text(if (terminal) "Show terminal controls" else "Hide terminal controls") },
        )
        Text("user ${vm.user?.userId.orEmpty()}", style = MaterialTheme.typography.labelSmall)
        Text("workspace ${vm.selectedWorkspace?.workspaceId.orEmpty()}", style = MaterialTheme.typography.labelSmall)
        Text("Usage & billing is not available on this Proxy.", color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodySmall)
    }
}
