import SwiftUI

struct InboxScreen: View {
    @EnvironmentObject private var session: AppSession
    @State private var query = ""

    var body: some View {
        NavigationStack {
            List {
                if let snapshot = session.snapshot {
                    let items = Attention.items(in: snapshot).filter { agent in
                        guard !query.isEmpty else { return true }
                        let machine = snapshot.machine(for: agent)?.displayName ?? ""
                        return agent.name.localizedCaseInsensitiveContains(query)
                            || agent.kind.localizedCaseInsensitiveContains(query)
                            || machine.localizedCaseInsensitiveContains(query)
                    }
                    if items.isEmpty {
                        Text(query.isEmpty ? "No Agents need you." : "No matches.")
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(items) { agent in
                            NavigationLink {
                                AgentDetailScreen(agentId: agent.agentId)
                            } label: {
                                AgentRow(agent: agent, machine: snapshot.machine(for: agent))
                            }
                        }
                    }
                } else {
                    ProgressView()
                }
            }
            .navigationTitle("")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { FleetToolbar() }
            .searchable(text: $query, prompt: "Search attention")
            .refreshable { await session.refreshSnapshot() }
        }
    }
}
