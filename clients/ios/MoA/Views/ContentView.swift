import SwiftUI

/// Root view — routes between setup wizard and main app.
struct ContentView: View {
    @EnvironmentObject var settings: SettingsStore
    @EnvironmentObject var agent: AgentManager

    var body: some View {
        Group {
            if !settings.setupComplete {
                SetupWizardView()
            } else {
                MainTabView()
            }
        }
        .preferredColorScheme(.dark)
    }
}

/// Main tab navigation after setup is complete.
struct MainTabView: View {
    @State private var selectedTab = 0

    var body: some View {
        TabView(selection: $selectedTab) {
            ChatView()
                .tabItem {
                    Image(systemName: "message.fill")
                    Text("Chat")
                }
                .tag(0)

            SettingsView()
                .tabItem {
                    Image(systemName: "gearshape.fill")
                    Text("Settings")
                }
                .tag(1)
        }
        .tint(.blue)
    }
}
