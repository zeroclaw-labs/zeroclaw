import SwiftUI
import WidgetKit

@main
struct ZeroClawApp: App {
    @StateObject private var agentService = AgentService()
    @StateObject private var settingsManager = SettingsManager()
    @Environment(\.scenePhase) private var scenePhase

    init() {
        // Register notification categories and handlers
        NotificationManager.shared.setup()
        NotificationManager.shared.requestPermission()

        // Register background tasks
        BackgroundTaskManager.shared.registerTasks()
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(agentService)
                .environmentObject(settingsManager)
                .preferredColorScheme(settingsManager.colorScheme)
                .onAppear {
                    agentService.configure(settings: settingsManager)
                    Task { await agentService.runDiagnostics() }
                }
                .onChange(of: scenePhase) {
                    switch scenePhase {
                    case .active:
                        // Process pending messages from Share Extension
                        agentService.processPendingSharedMessages()
                    case .background:
                        // Schedule background health check
                        BackgroundTaskManager.shared.scheduleHealthCheck()
                        // Update widget
                        agentService.updateSharedState()
                        WidgetCenter.shared.reloadAllTimelines()
                    default:
                        break
                    }
                }
                .onReceive(NotificationCenter.default.publisher(for: .zeroClawNotificationReply)) { notification in
                    if let message = notification.userInfo?["message"] as? String {
                        agentService.sendMessage(message)
                    }
                }
        }
    }
}
