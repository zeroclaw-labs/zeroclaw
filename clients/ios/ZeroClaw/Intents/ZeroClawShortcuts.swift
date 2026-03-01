import AppIntents

/// Provides suggested Siri phrases and Shortcuts for ZeroClaw.
struct ZeroClawShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: AskZeroClawIntent(),
            phrases: [
                "Ask \(.applicationName)",
                "Message \(.applicationName)",
                "Talk to \(.applicationName)",
            ],
            shortTitle: "Ask ZeroClaw",
            systemImageName: "brain"
        )

        AppShortcut(
            intent: CheckStatusIntent(),
            phrases: [
                "\(.applicationName) status",
                "Is \(.applicationName) running",
                "Check \(.applicationName)",
            ],
            shortTitle: "Check Status",
            systemImageName: "antenna.radiowaves.left.and.right"
        )

        AppShortcut(
            intent: ToggleAgentIntent(),
            phrases: [
                "Toggle \(.applicationName)",
                "Connect \(.applicationName)",
                "Disconnect \(.applicationName)",
            ],
            shortTitle: "Toggle Agent",
            systemImageName: "power"
        )
    }
}
