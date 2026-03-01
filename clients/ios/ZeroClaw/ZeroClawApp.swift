import SwiftUI

@main
struct ZeroClawApp: App {
    @StateObject private var agentService = AgentService()
    @StateObject private var settingsManager = SettingsManager()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(agentService)
                .environmentObject(settingsManager)
                .preferredColorScheme(settingsManager.colorScheme)
        }
    }
}
