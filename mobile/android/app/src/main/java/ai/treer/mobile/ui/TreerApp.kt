package ai.treer.mobile.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.lifecycle.viewmodel.compose.viewModel
import ai.treer.mobile.ui.components.ConfirmCard
import ai.treer.mobile.ui.components.VoicePreviewSheet
import ai.treer.mobile.ui.screens.AgentDetailScreen
import ai.treer.mobile.ui.screens.AgentTerminalScreen
import ai.treer.mobile.ui.screens.AddMachineScreen
import ai.treer.mobile.ui.screens.CreateAgentScreen
import ai.treer.mobile.ui.screens.ForgotPasswordScreen
import ai.treer.mobile.ui.screens.LoginScreen
import ai.treer.mobile.ui.screens.MachineDetailScreen
import ai.treer.mobile.ui.screens.MainScaffold
import ai.treer.mobile.ui.screens.ProxySetupScreen
import ai.treer.mobile.ui.screens.RegisterScreen
import ai.treer.mobile.ui.screens.ResetPasswordScreen
import ai.treer.mobile.ui.screens.SettingsScreen
import ai.treer.mobile.ui.screens.WorkspaceSwitcherScreen
import ai.treer.mobile.ui.theme.TreerTheme
import ai.treer.mobile.ui.webview.AgentWebViewScreen

@OptIn(ExperimentalMaterial3Api::class, ExperimentalComposeUiApi::class)
@Composable
fun TreerApp(vm: AppViewModel = viewModel()) {
    TreerTheme(theme = vm.theme) {
        BackHandler(enabled = vm.screen !is AppScreen.Main && vm.screen !is AppScreen.Login && vm.screen !is AppScreen.ProxySetup) {
            vm.back()
        }
        val main = vm.screen is AppScreen.Main
        Scaffold(
            modifier = Modifier
                .fillMaxSize()
                .semantics { testTagsAsResourceId = true },
            contentWindowInsets = if (main) WindowInsets(0, 0, 0, 0) else WindowInsets.safeDrawing,
            topBar = {
                if (showsBack(vm.screen)) {
                    TopAppBar(
                        title = { Text(titleFor(vm.screen)) },
                        navigationIcon = {
                            IconButton(onClick = { vm.back() }) {
                                Icon(Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = "Back")
                            }
                        },
                    )
                }
            },
        ) { padding ->
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding),
            ) {
                when (val screen = vm.screen) {
                    AppScreen.ProxySetup -> ProxySetupScreen(vm)
                    AppScreen.Login -> LoginScreen(vm)
                    AppScreen.Register -> RegisterScreen(vm)
                    AppScreen.ForgotPassword -> ForgotPasswordScreen(vm)
                    AppScreen.ResetPassword -> ResetPasswordScreen(vm)
                    AppScreen.WorkspaceSwitcher -> WorkspaceSwitcherScreen(vm)
                    AppScreen.Main -> MainScaffold(vm)
                    AppScreen.Settings -> SettingsScreen(vm)
                    is AppScreen.MachineDetail -> MachineDetailScreen(vm, screen.serverId)
                    is AppScreen.AgentDetail -> AgentDetailScreen(vm, screen.agentId)
                    is AppScreen.AgentUi -> {
                        val token = vm.token
                        val workspaceId = vm.selectedWorkspace?.workspaceId
                        if (token != null && workspaceId != null) {
                            AgentWebViewScreen(
                                api = vm.api,
                                baseUrl = vm.baseUrl,
                                token = token,
                                workspaceId = workspaceId,
                                agentId = screen.agentId,
                                uiPath = vm.snapshot.agent(screen.agentId)?.interfaceDescriptor?.uiPath,
                            )
                        }
                    }
                    is AppScreen.AgentTerminal -> {
                        val token = vm.token
                        val workspaceId = vm.selectedWorkspace?.workspaceId
                        if (token != null && workspaceId != null) {
                            AgentTerminalScreen(
                                api = vm.api,
                                baseUrl = vm.baseUrl,
                                token = token,
                                workspaceId = workspaceId,
                                agentId = screen.agentId,
                            )
                        }
                    }
                    is AppScreen.CreateAgent -> CreateAgentScreen(vm, screen.serverId)
                    AppScreen.AddMachine -> AddMachineScreen(vm)
                }
            }
        }
        vm.confirm?.let { spec ->
            ConfirmCard(
                spec = spec,
                onConfirm = { vm.executeConfirm() },
                onChange = { vm.changeConfirm() },
                onCancel = { vm.cancelConfirm() },
            )
        }
        if (vm.voiceOpen) {
            VoicePreviewSheet(
                workspaceId = vm.selectedWorkspace?.workspaceId,
                baseUrl = vm.baseUrl,
                token = vm.token,
                asr = vm.voiceAsr,
                lines = vm.voiceLines,
                busy = vm.voiceBusy,
                speaking = vm.voiceSpeaking,
                mode = vm.voiceMode,
                tts = vm.voiceTtsStatus,
                onRefreshAsr = { vm.loadVoiceAsr() },
                onUtterance = { vm.submitVoiceUtterance(it) },
                onHoldStart = { vm.stopVoiceSpeech() },
                onMode = { vm.selectVoiceMode(it) },
                onInstallTts = { vm.installTtsEngine() },
                onInstallTtsVoice = { vm.installTtsVoiceData() },
                onOpenTtsSettings = { vm.openTtsSettings() },
                onDismiss = { vm.closeVoice() },
            )
        }
    }
}

private fun showsBack(screen: AppScreen): Boolean {
    return when (screen) {
        AppScreen.ProxySetup, AppScreen.Login, AppScreen.Main -> false
        else -> true
    }
}

private fun titleFor(screen: AppScreen): String {
    return when (screen) {
        AppScreen.Register -> "Register"
        AppScreen.ForgotPassword, AppScreen.ResetPassword -> "Reset password"
        AppScreen.WorkspaceSwitcher -> "Workspace"
        AppScreen.Settings -> "Settings"
        is AppScreen.MachineDetail -> "Machine"
        is AppScreen.AgentDetail -> "Agent"
        is AppScreen.AgentUi -> "Agent UI"
        is AppScreen.AgentTerminal -> "Terminal"
        is AppScreen.CreateAgent -> "Assign"
        AppScreen.AddMachine -> "Add machine"
        else -> "Treer"
    }
}
