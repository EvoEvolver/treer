package ai.treer.mobile.domain

data class FleetLists(
    val attention: List<Agent>,
    val working: List<Agent>,
    val idle: List<Agent>,
    val homeAttention: List<Agent>,
    val inboxOverflow: Int,
    val onlineMachines: Int,
    val machineCount: Int,
    val workingCount: Int,
    val blockedCount: Int,
    val idleCount: Int,
)

fun agentNeedsAttention(agent: Agent, machine: Machine?): Boolean {
    if (agent.kind == "app") return false
    val status = agent.status.lowercase()
    if (status in ATTENTION_STATUSES) return true
    val machineOnline = machine?.isOnline == true
    return !machineOnline && status in NON_TERMINAL_STATUSES
}

fun agentSummary(agent: Agent, machine: Machine?): String {
    val machineName = machine?.displayName ?: agent.serverId
    return "${agent.name} is ${agent.status} on $machineName"
}

fun classifyFleet(snapshot: Snapshot, homeAttentionLimit: Int = 8): FleetLists {
    val agents = snapshot.fleetAgents
    val attention = agents.filter { agentNeedsAttention(it, snapshot.machine(it.serverId)) }
    val working = agents.filter { it.status.lowercase() in WORKING_STATUSES }
    val idle = agents.filter { agent ->
        val status = agent.status.lowercase()
        status == "idle" || status == "unknown"
    }
    val homeAttention = attention.take(homeAttentionLimit)
    val overflow = (attention.size - homeAttentionLimit).coerceAtLeast(0)
    val online = snapshot.servers.count { it.isOnline }
    return FleetLists(
        attention = attention,
        working = working,
        idle = idle,
        homeAttention = homeAttention,
        inboxOverflow = overflow,
        onlineMachines = online,
        machineCount = snapshot.servers.size,
        workingCount = working.size,
        blockedCount = agents.count { it.status.equals("blocked", true) },
        idleCount = idle.size,
    )
}
