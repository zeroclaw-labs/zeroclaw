import SwiftUI

// AgentStatus is defined by UniFFI in zeroclaw_ios.swift.
// These extensions add display helpers for the UI layer.

extension AgentStatus {
    var displayText: String {
        switch self {
        case .stopped: "Disconnected"
        case .starting: "Connecting..."
        case .running: "Connected"
        case .thinking: "Thinking..."
        case .error(let message): message
        }
    }

    var color: Color {
        switch self {
        case .running: .green
        case .starting, .thinking: .orange
        case .stopped: .gray
        case .error: .red
        }
    }

    var isActive: Bool {
        switch self {
        case .running, .thinking: true
        default: false
        }
    }
}
