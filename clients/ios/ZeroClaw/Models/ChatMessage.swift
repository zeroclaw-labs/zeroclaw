import Foundation

// ChatMessage is defined by UniFFI in zeroclaw_ios.swift.
// These extensions add SwiftUI convenience on top of the generated type.

extension ChatMessage: Identifiable {}

extension ChatMessage {
    /// Create a message with the current timestamp.
    static func now(content: String, role: String) -> ChatMessage {
        ChatMessage(
            id: UUID().uuidString,
            content: content,
            role: role,
            timestampMs: Int64(Date().timeIntervalSince1970 * 1000)
        )
    }

    var isUser: Bool {
        role == "user"
    }

    var timestamp: Date {
        Date(timeIntervalSince1970: TimeInterval(timestampMs) / 1000.0)
    }
}
