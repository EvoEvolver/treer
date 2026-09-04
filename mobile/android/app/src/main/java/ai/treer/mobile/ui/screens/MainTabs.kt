package ai.treer.mobile.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.OutlinedButton
import androidx.compose.runtime.LaunchedEffect
import kotlinx.coroutines.delay
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Inbox
import androidx.compose.material.icons.outlined.Mic
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.Dns
import androidx.compose.material.icons.outlined.Home
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FabPosition
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import ai.treer.mobile.domain.ConnectionState
import ai.treer.mobile.domain.machineRecoveryCommands
import ai.treer.mobile.ui.AppScreen
import ai.treer.mobile.ui.AppViewModel
import ai.treer.mobile.ui.MainTab
import ai.treer.mobile.ui.components.AgentRow
import ai.treer.mobile.ui.components.ErrorBanner
import ai.treer.mobile.ui.components.MonoBlock
import ai.treer.mobile.ui.components.SectionHeader
import ai.treer.mobile.ui.components.StatusDot
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.ui.platform.LocalContext

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MainScaffold(vm: AppViewModel) {
    val orgName = vm.selectedOrg?.name ?: "Org"
    val workspaceName = vm.selectedWorkspace?.name ?: "Workspace"
    Scaffold(
        contentWindowInsets = WindowInsets(0, 0, 0, 0),
        topBar = {
            TopAppBar(
                windowInsets = WindowInsets.statusBars,
                title = {
                    Column(
                        modifier = Modifier
                            .testTag("workspace-switcher")
                            .clickable { vm.go(AppScreen.WorkspaceSwitcher) },
                    ) {
                        Text("$orgName / $workspaceName", style = MaterialTheme.typography.titleSmall, maxLines = 1)
                        val connection = when (vm.connection) {
                            ConnectionState.Live -> "live"
                            ConnectionState.Reconnecting -> "reconnecting"
                            ConnectionState.Offline -> "offline"
                        }
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            StatusDot(if (vm.connection == ConnectionState.Live) "online" else "offline")
                            Text(connection + if (vm.stale) " · stale" else "", style = MaterialTheme.typography.labelSmall)
                        }
                    }
                },
                actions = {
                    if (vm.tab == MainTab.Machines) {
                        IconButton(
                            onClick = { vm.go(AppScreen.AddMachine) },
                            enabled = vm.connection == ConnectionState.Live || !vm.stale,
                            modifier = Modifier.testTag("add-machine-button"),
                        ) { Icon(Icons.Outlined.Add, contentDescription = "Add machine") }
                    } else {
                        IconButton(
                            onClick = { vm.go(AppScreen.CreateAgent(null)) },
                            enabled = !vm.stale || vm.connection == ConnectionState.Live,
                            modifier = Modifier.testTag("assign-button"),
                        ) { Icon(Icons.Outlined.Add, contentDescription = "Assign") }
                    }
                    IconButton(
                        onClick = { vm.go(AppScreen.Settings) },
                        modifier = Modifier.testTag("settings-button"),
                    ) { Icon(Icons.Outlined.Settings, contentDescription = "Settings") }
                },
            )
        },
        bottomBar = {
            NavigationBar(
                windowInsets = WindowInsets.navigationBars,
                tonalElevation = 0.dp,
            ) {
                NavigationBarItem(
                    selected = vm.tab == MainTab.Home,
                    onClick = { vm.selectTab(MainTab.Home) },
                    icon = { Icon(Icons.Outlined.Home, contentDescription = "Home") },
                    label = { Text("Home") },
                    alwaysShowLabel = true,
                    modifier = Modifier.testTag("home-tab"),
                )
                NavigationBarItem(
                    selected = vm.tab == MainTab.Machines,
                    onClick = { vm.selectTab(MainTab.Machines) },
                    icon = { Icon(Icons.Outlined.Dns, contentDescription = "Machines") },
                    label = { Text("Machines") },
                    alwaysShowLabel = true,
                    modifier = Modifier.testTag("machines-tab"),
                )
                NavigationBarItem(
                    selected = vm.tab == MainTab.Inbox,
                    onClick = { vm.selectTab(MainTab.Inbox) },
                    icon = { Icon(Icons.Outlined.Inbox, contentDescription = "Inbox") },
                    label = { Text("Inbox") },
                    alwaysShowLabel = true,
                    modifier = Modifier.testTag("inbox-tab"),
                )
            }
        },
        floatingActionButton = {
            FloatingActionButton(
                onClick = { vm.openVoice() },
                modifier = Modifier
                    .testTag("voice-button")
                    .semantics { contentDescription = "Voice" },
            ) {
                Icon(Icons.Outlined.Mic, contentDescription = "Voice")
            }
        },
        floatingActionButtonPosition = FabPosition.End,
    ) { padding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            when (vm.tab) {
                MainTab.Home -> HomeScreen(vm)
                MainTab.Machines -> MachinesScreen(vm)
                MainTab.Inbox -> InboxScreen(vm)
            }
        }
    }
}

@Composable
fun HomeScreen(vm: AppViewModel) {
    val fleet = vm.fleet
    val snapshot = vm.snapshot
    LazyColumn(
        modifier = Modifier
            .fillMaxSize()
            .testTag("home-screen"),
        contentPadding = PaddingValues(bottom = 72.dp),
    ) {
        item { ErrorBanner(vm.error) }
        if (vm.stale) {
            item {
                Text(
                    "Showing the last snapshot. Assign is disabled until the Proxy is reachable.",
                    modifier = Modifier.padding(16.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        item {
            Text(
                "Online machines ${fleet.onlineMachines}/${fleet.machineCount} · working ${fleet.workingCount} · blocked ${fleet.blockedCount} · idle ${fleet.idleCount}",
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                style = MaterialTheme.typography.labelMedium,
            )
        }
        if (snapshot.servers.isEmpty()) {
            item {
                EmptyCopy(
                    "No machines yet. Open Machines and tap + to copy install and connect commands. Run them on a computer — this phone does not run a Host.",
                )
            }
        } else if (snapshot.fleetAgents.isEmpty()) {
            item { EmptyCopy("No Agents yet. Tap Assign to start one on an online machine.") }
        } else {
            item {
                SectionHeader(
                    "Needs attention",
                    if (fleet.inboxOverflow > 0) "+${fleet.inboxOverflow} in Inbox" else null,
                )
            }
            if (fleet.homeAttention.isEmpty()) {
                item { EmptyCopy("Nothing needs you right now.") }
            }
            items(fleet.homeAttention, key = { it.agentId }) { agent ->
                AgentRow(agent = agent, machine = snapshot.machine(agent.serverId), onClick = { vm.openAgent(agent.agentId) })
            }
            item { SectionHeader("Working now") }
            if (fleet.working.isEmpty()) {
                item { EmptyCopy("No Agents are working.") }
            }
            items(fleet.working, key = { "w-${it.agentId}" }) { agent ->
                AgentRow(agent = agent, machine = snapshot.machine(agent.serverId), onClick = { vm.openAgent(agent.agentId) })
            }
            item { SectionHeader("Idle & ready") }
            if (fleet.idle.isEmpty()) {
                item { EmptyCopy("No idle Agents.") }
            }
            items(fleet.idle, key = { "i-${it.agentId}" }) { agent ->
                AgentRow(agent = agent, machine = snapshot.machine(agent.serverId), onClick = { vm.openAgent(agent.agentId) })
            }
        }
    }
}

@Composable
fun MachinesScreen(vm: AppViewModel) {
    val snapshot = vm.snapshot
    LazyColumn(modifier = Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 24.dp)) {
        item { ErrorBanner(vm.error) }
        if (snapshot.servers.isEmpty()) {
            item { EmptyCopy("No machines enrolled. Tap + to copy install and connect commands for a computer.") }
        }
        items(snapshot.servers, key = { it.serverId }) { machine ->
            val agents = snapshot.fleetAgents.filter { it.serverId == machine.serverId }
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable { vm.go(AppScreen.MachineDetail(machine.serverId)) }
                    .padding(horizontal = 16.dp, vertical = 12.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    StatusDot(if (machine.isOnline) "online" else "offline")
                    Text(machine.displayName, style = MaterialTheme.typography.bodyLarge)
                }
                Text(machine.root.ifBlank { machine.hostname }, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Text(
                    "working ${agents.count { it.status == "working" || it.status == "starting" }} · blocked ${agents.count { it.status == "blocked" }} · idle ${agents.count { it.status == "idle" }}",
                    style = MaterialTheme.typography.labelSmall,
                )
                val kinds = machine.availableAgents.orEmpty()
                if (kinds.isNotEmpty()) {
                    Text(kinds.joinToString(" · "), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                if (!machine.isOnline) {
                    Text(
                        "This machine is not connected to the control plane. It may be stopped, waking from sleep, or fenced as a duplicate.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.padding(top = 6.dp),
                    )
                }
            }
        }
    }
}

@Composable
fun InboxScreen(vm: AppViewModel) {
    val fleet = vm.fleet
    val snapshot = vm.snapshot
    LazyColumn(modifier = Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 24.dp)) {
        item { ErrorBanner(vm.error) }
        if (fleet.attention.isEmpty()) {
            item { EmptyCopy("No Agents need you.") }
        }
        items(fleet.attention, key = { it.agentId }) { agent ->
            AgentRow(agent = agent, machine = snapshot.machine(agent.serverId), onClick = { vm.openAgent(agent.agentId) })
        }
    }
}

@Composable
fun MachineDetailScreen(vm: AppViewModel, serverId: String) {
    val machine = vm.snapshot.machine(serverId)
    val context = LocalContext.current
    if (machine == null) {
        EmptyCopy("Machine not in the latest snapshot.")
        return
    }
    val agents = vm.snapshot.fleetAgents.filter { it.serverId == serverId }
    val workspaceId = vm.selectedWorkspace?.workspaceId.orEmpty()
    val (restart, start) = machineRecoveryCommands(workspaceId)
    LazyColumn(modifier = Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 24.dp)) {
        item {
            Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    StatusDot(if (machine.isOnline) "online" else "offline")
                    Text(machine.displayName, style = MaterialTheme.typography.headlineSmall)
                }
                Text(machine.hostname, style = MaterialTheme.typography.bodySmall)
                Text(machine.root, style = MaterialTheme.typography.bodySmall)
                machine.supervision?.let {
                    Text("supervision ${it.mode}${it.fallbackReason?.let { reason -> " · $reason" } ?: ""}")
                }
                Text("controller ${machine.controllerBuild.version} · host ${machine.hostBuild.version}", style = MaterialTheme.typography.labelSmall)
                if (!machine.isOnline) {
                    Text("This machine is not connected to the control plane. It may be stopped, waking from sleep, or fenced as a duplicate.")
                    MonoBlock(restart)
                    Text(
                        "Copy restart-controller",
                        modifier = Modifier.clickable { copy(context, restart) },
                        color = MaterialTheme.colorScheme.primary,
                    )
                    MonoBlock(start)
                    Text(
                        "Copy start",
                        modifier = Modifier.clickable { copy(context, start) },
                        color = MaterialTheme.colorScheme.primary,
                    )
                }
                Text(
                    "Assign on this machine",
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.clickable { vm.go(AppScreen.CreateAgent(serverId)) },
                )
            }
        }
        items(agents, key = { it.agentId }) { agent ->
            AgentRow(agent = agent, machine = machine, onClick = { vm.openAgent(agent.agentId) })
        }
        if (agents.isEmpty()) {
            item { EmptyCopy("Online but no Agents. Assign one on this machine.") }
        }
    }
}

@Composable
fun EmptyCopy(text: String) {
    Text(
        text,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

fun copy(context: Context, text: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    clipboard.setPrimaryClip(ClipData.newPlainText("treer", text))
}

@Composable
fun AddMachineScreen(vm: AppViewModel) {
    val context = LocalContext.current
    val online = vm.snapshot.servers.filter { it.isOnline }
    LaunchedEffect(Unit) {
        while (true) {
            delay(4000)
            vm.reloadSnapshot()
        }
    }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp)
            .testTag("add-machine-screen"),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text("Add a machine", style = MaterialTheme.typography.headlineSmall)
        Text(
            "This phone does not run a Host. Copy both commands and run them on the computer you want to enroll. Step 1 installs Treer. Step 2 is a 10-minute, single-use enrollment key.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        ErrorBanner(vm.error)
        Text("1. Install", style = MaterialTheme.typography.titleSmall)
        MonoBlock(vm.bootstrapInstall.ifBlank { "Loading…" })
        Text(
            "Copy install command",
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier
                .clickable(enabled = vm.bootstrapInstall.isNotBlank()) { copy(context, vm.bootstrapInstall) }
                .testTag("copy-install-command"),
        )
        Text("2. Connect this workspace", style = MaterialTheme.typography.titleSmall)
        MonoBlock(vm.bootstrapConnect.ifBlank { "Loading…" })
        Text(
            "Copy connect command",
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier
                .clickable(enabled = vm.bootstrapConnect.isNotBlank()) { copy(context, vm.bootstrapConnect) }
                .testTag("copy-connect-command"),
        )
        OutlinedButton(
            onClick = {
                vm.loadBootstrap()
                vm.refreshSnapshot()
            },
            enabled = !vm.busy,
            modifier = Modifier
                .fillMaxWidth()
                .testTag("refresh-machines"),
        ) { Text(if (vm.busy) "Refreshing…" else "I've run the command — refresh") }
        if (online.isNotEmpty()) {
            Text(
                "Online: ${online.joinToString { it.displayName }}",
                style = MaterialTheme.typography.bodyMedium,
            )
            Button(
                onClick = { vm.go(AppScreen.CreateAgent(online.first().serverId)) },
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag("assign-after-enroll"),
            ) { Text("Assign an Agent") }
        } else if (vm.snapshot.servers.isNotEmpty()) {
            Text("A machine is enrolled but offline. Open it from Machines to copy recovery commands.")
        } else {
            Text(
                "The machine appears here as soon as the Host connects.",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
