package ai.treer.mobile

import ai.treer.mobile.data.parseBootstrap
import ai.treer.mobile.domain.Machine
import ai.treer.mobile.domain.preferredCreateKind
import ai.treer.mobile.domain.wireAgentKind
import com.google.gson.JsonParser
import org.junit.Assert.assertEquals
import org.junit.Test

class CreateFlowTest {
    @Test
    fun terminalKindWiresToCommand() {
        assertEquals("command", wireAgentKind("terminal"))
        assertEquals("codex", wireAgentKind("codex"))
        assertEquals("cursor", wireAgentKind("cursor-agent"))
    }

    @Test
    fun prefersCodexWhenTheMachineReportsIt() {
        val withCodex = Machine(serverId = "srv_1", availableAgents = listOf("claude", "codex"))
        val without = Machine(serverId = "srv_1", availableAgents = listOf("claude"))
        val unknown = Machine(serverId = "srv_1", availableAgents = null)
        assertEquals("codex", preferredCreateKind(withCodex))
        assertEquals("terminal", preferredCreateKind(without))
        assertEquals("terminal", preferredCreateKind(unknown))
        assertEquals("terminal", preferredCreateKind(null))
    }

    @Test
    fun parsesBootstrapCommands() {
        val json = JsonParser.parseString(
            """
            {
              "install_command": "curl -fsSL 'http://proxy.example/install.sh' | sh",
              "connect_command": "TREER_ENROLLMENT_KEY='enr_v1_x' treer-agent-server connect --proxy 'http://proxy.example/'",
              "enrollment_key": "enr_v1_x",
              "script_url": "http://proxy.example/install.sh",
              "workspace_id": "ws_lab"
            }
            """.trimIndent(),
        ).asJsonObject
        val info = parseBootstrap(json)
        assertEquals("curl -fsSL 'http://proxy.example/install.sh' | sh", info.installCommand)
        assertEquals(
            "TREER_ENROLLMENT_KEY='enr_v1_x' treer-agent-server connect --proxy 'http://proxy.example/'",
            info.connectCommand,
        )
        assertEquals("ws_lab", info.workspaceId)
    }
}
