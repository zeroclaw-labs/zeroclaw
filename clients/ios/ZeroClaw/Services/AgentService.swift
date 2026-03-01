import Foundation

/// Manages ZeroClaw agent lifecycle and message state.
/// Acts as the main coordinator between the UI and the Rust bridge.
@MainActor
final class AgentService: ObservableObject {
    @Published private(set) var status: AgentStatus = .stopped
    @Published private(set) var messages: [ChatMessage] = []

    private let bridge = ZeroClawBridge.shared

    init() {
        let dataDir = Self.defaultDataDir()
        bridge.initialize(dataDir: dataDir)
    }

    func start() {
        guard !status.isActive else { return }

        status = .starting
        do {
            try bridge.start()
            status = .running
        } catch {
            status = .error(message: error.localizedDescription)
        }
    }

    func stop() {
        do {
            try bridge.stop()
            status = .stopped
        } catch {
            status = .error(message: error.localizedDescription)
        }
    }

    func toggle() {
        if status.isActive {
            stop()
        } else {
            start()
        }
    }

    func sendMessage(_ content: String) {
        guard !content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }

        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let userMessage = ChatMessage(
            id: UUID().uuidString,
            content: content,
            role: "user",
            timestampMs: now
        )
        messages.append(userMessage)

        // TODO: Replace with actual bridge call when connected
        let (success, _) = bridge.sendMessage(content)

        if success {
            // Simulate echo response (matches bridge stub behavior)
            let echoMessage = ChatMessage(
                id: UUID().uuidString,
                content: "Echo: \(content)",
                role: "assistant",
                timestampMs: Int64(Date().timeIntervalSince1970 * 1000)
            )
            messages.append(echoMessage)
        }
    }

    func clearMessages() {
        messages.removeAll()
        bridge.clearMessages()
    }

    private static func defaultDataDir() -> String {
        let paths = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)
        return paths[0].appendingPathComponent("zeroclaw").path
    }
}
