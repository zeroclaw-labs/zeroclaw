import Foundation

// AgentStatus is defined by UniFFI in zeroclaw_ios.swift.
// These extensions add display helpers for the UI layer.

extension AgentStatus {
    var displayText: String {
        switch self {
        case .stopped: "Stopped"
        case .starting: "Starting..."
        case .running: "Running"
        case .thinking: "Thinking..."
        case .error(let message): "Error: \(message)"
        }
    }

    var isActive: Bool {
        switch self {
        case .running, .thinking: true
        default: false
        }
    }
}
