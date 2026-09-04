package ai.treer.mobile

import ai.treer.mobile.domain.Agent
import ai.treer.mobile.domain.Machine
import ai.treer.mobile.domain.Snapshot
import ai.treer.mobile.domain.Workspace
import ai.treer.mobile.domain.agentNeedsAttention
import ai.treer.mobile.domain.classifyFleet
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AttentionTest {
    private val online = machine("srv_1", "online")
    private val offline = machine("srv_2", "offline")

    @Test
    fun blockedIsAttention() {
        assertTrue(agentNeedsAttention(agent("a1", "srv_1", "blocked"), online))
    }

    @Test
    fun failedIsAttention() {
        assertTrue(agentNeedsAttention(agent("a1", "srv_1", "failed"), online))
    }

    @Test
    fun longWorkingIsNotAttention() {
        assertFalse(agentNeedsAttention(agent("a1", "srv_1", "working"), online))
        assertFalse(agentNeedsAttention(agent("a1", "srv_1", "starting"), online))
    }

    @Test
    fun idleOnOnlineMachineIsNotAttention() {
        assertFalse(agentNeedsAttention(agent("a1", "srv_1", "idle"), online))
    }

    @Test
    fun nonTerminalOnOfflineMachineIsAttention() {
        listOf("starting", "working", "idle", "blocked", "unknown").forEach { status ->
            assertTrue(status, agentNeedsAttention(agent("a1", "srv_2", status), offline))
        }
    }

    @Test
    fun exitedOnOfflineMachineIsNotAttention() {
        assertFalse(agentNeedsAttention(agent("a1", "srv_2", "exited"), offline))
    }

    @Test
    fun homeCapsAttentionAtEight() {
        val agents = (1..12).map { index ->
            agent("ag_$index", "srv_2", "idle")
        }
        val lists = classifyFleet(
            Snapshot(
                workspace = Workspace("ws_1", "lab"),
                servers = listOf(offline),
                agents = agents,
            ),
        )
        assertEquals(12, lists.attention.size)
        assertEquals(8, lists.homeAttention.size)
        assertEquals(4, lists.inboxOverflow)
    }

    @Test
    fun appKindIsIgnored() {
        val app = agent("ag_app", "srv_2", "working").copy(kind = "app")
        assertFalse(agentNeedsAttention(app, offline))
        val lists = classifyFleet(
            Snapshot(servers = listOf(offline), agents = listOf(app)),
        )
        assertTrue(lists.attention.isEmpty())
        assertTrue(lists.working.isEmpty())
    }

    private fun agent(id: String, serverId: String, status: String) = Agent(
        agentId = id,
        serverId = serverId,
        kind = "codex",
        name = id,
        status = status,
    )

    private fun machine(id: String, status: String) = Machine(
        serverId = id,
        name = id,
        hostname = id,
        status = status,
    )
}
