import SwiftUI

/// MoA — Master of AI
///
/// Local-first AI assistant app for iOS.
/// ZeroClaw runs in-process as a static library (iOS does not allow sidecar processes).
/// All chat messages are processed locally — nothing is sent to external servers
/// except the LLM API call from the local ZeroClaw engine.
@main
struct MoAApp: App {
    @StateObject private var agentManager = AgentManager()
    @StateObject private var settingsStore = SettingsStore()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(agentManager)
                .environmentObject(settingsStore)
        }
    }
}
