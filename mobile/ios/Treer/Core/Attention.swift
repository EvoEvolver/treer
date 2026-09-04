import Foundation

enum Attention {
    static let homeLimit = 8

    static func needsAttention(agent: AgentInfo, machine: ServerInfo?) -> Bool {
        if agent.status == .blocked || agent.status == .failed {
            return true
        }
        if machine?.isOnline != true && agent.status.isNonTerminal {
            return true
        }
        return false
    }

    static func items(in snapshot: WorkspaceSnapshot) -> [AgentInfo] {
        snapshot.agents
            .filter { agent in
                needsAttention(agent: agent, machine: snapshot.machine(for: agent))
            }
            .sorted { lhs, rhs in
                if lhs.updatedAt != rhs.updatedAt {
                    return lhs.updatedAt > rhs.updatedAt
                }
                return lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
            }
    }

    static func working(in snapshot: WorkspaceSnapshot) -> [AgentInfo] {
        snapshot.agents
            .filter { agent in
                guard snapshot.machine(for: agent)?.isOnline == true else { return false }
                return agent.status == .starting || agent.status == .working
            }
            .sorted { $0.updatedAt > $1.updatedAt }
    }

    static func idle(in snapshot: WorkspaceSnapshot) -> [AgentInfo] {
        snapshot.agents
            .filter { agent in
                guard snapshot.machine(for: agent)?.isOnline == true else { return false }
                return agent.status == .idle || agent.status == .unknown
            }
            .sorted { lhs, rhs in
                let leftMachine = snapshot.machine(for: lhs)?.displayName ?? lhs.serverId
                let rightMachine = snapshot.machine(for: rhs)?.displayName ?? rhs.serverId
                if leftMachine != rightMachine {
                    return leftMachine.localizedCaseInsensitiveCompare(rightMachine) == .orderedAscending
                }
                return lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
            }
    }

    static func summary(for agent: AgentInfo, machine: ServerInfo?) -> String {
        let status = agent.displayStatus(machine: machine)
        let machineName = machine?.displayName ?? agent.serverId
        return "\(agent.name) is \(status) on \(machineName)"
    }

    static func homeAttention(in snapshot: WorkspaceSnapshot) -> (items: [AgentInfo], overflow: Int) {
        let all = items(in: snapshot)
        if all.count <= homeLimit {
            return (all, 0)
        }
        return (Array(all.prefix(homeLimit)), all.count - homeLimit)
    }
}
