import Foundation

// TODO: Import UniFFI-generated bindings once build-ios.sh has been run.
// import ZeroClawCore

/// Thin wrapper over UniFFI-generated Swift bindings.
/// Provides a Swift-native interface to the Rust ZeroClawController.
///
/// Once the Rust library is built and bindings are generated,
/// replace the stub implementations with real UniFFI calls.
final class ZeroClawBridge: @unchecked Sendable {
    static let shared = ZeroClawBridge()

    // TODO: Replace with actual UniFFI controller
    // private var controller: ZeroClawController?
    private var isLoaded = false

    private init() {}

    func initialize(dataDir: String) {
        // TODO: Uncomment when UniFFI bindings are available
        // controller = ZeroClawController.withDefaults(dataDir: dataDir)
        isLoaded = true
    }

    func start() throws {
        guard isLoaded else { throw BridgeError.notInitialized }
        // TODO: try controller?.start()
    }

    func stop() throws {
        guard isLoaded else { throw BridgeError.notInitialized }
        // TODO: try controller?.stop()
    }

    func getStatus() -> AgentStatus {
        // TODO: Map from UniFFI AgentStatus
        // guard let status = controller?.getStatus() else { return .stopped }
        return .stopped
    }

    func sendMessage(_ content: String) -> (success: Bool, messageId: String?) {
        // TODO: Use actual bridge
        // guard let result = controller?.sendMessage(content: content) else {
        //     return (false, nil)
        // }
        // return (result.success, result.messageId)
        return (true, UUID().uuidString)
    }

    func getMessages() -> [ChatMessage] {
        // TODO: Map from UniFFI ChatMessage
        // guard let messages = controller?.getMessages() else { return [] }
        // return messages.map { ... }
        return []
    }

    func clearMessages() {
        // TODO: controller?.clearMessages()
    }

    func isConfigured() -> Bool {
        // TODO: return controller?.isConfigured() ?? false
        return false
    }

    enum BridgeError: LocalizedError {
        case notInitialized
        case alreadyRunning
        case configError(String)
        case gatewayError(String)

        var errorDescription: String? {
            switch self {
            case .notInitialized: "ZeroClaw not initialized"
            case .alreadyRunning: "Gateway already running"
            case .configError(let msg): "Config error: \(msg)"
            case .gatewayError(let msg): "Gateway error: \(msg)"
            }
        }
    }
}
