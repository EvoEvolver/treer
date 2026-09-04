import SwiftUI

enum MainTab: Hashable {
    case home, machines, inbox
}

struct MainTabView: View {
    @EnvironmentObject private var session: AppSession
    @State private var tab: MainTab = .home

    var body: some View {
        TabView(selection: $tab) {
            HomeScreen()
                .tabItem {
                    Label("Home", systemImage: "house")
                        .accessibilityIdentifier("home-tab")
                }
                .tag(MainTab.home)
            MachinesScreen()
                .tabItem {
                    Label("Machines", systemImage: "desktopcomputer")
                        .accessibilityIdentifier("machines-tab")
                }
                .tag(MainTab.machines)
            InboxScreen()
                .tabItem {
                    Label("Inbox", systemImage: "tray")
                        .accessibilityIdentifier("inbox-tab")
                }
                .tag(MainTab.inbox)
        }
        .safeAreaInset(edge: .bottom, alignment: .trailing) {
            VoiceButton { session.showVoicePreview = true }
                .padding(.trailing, 16)
                .padding(.top, 4)
                .padding(.bottom, 8)
        }
        .task {
            await session.refreshSnapshot()
        }
    }
}
