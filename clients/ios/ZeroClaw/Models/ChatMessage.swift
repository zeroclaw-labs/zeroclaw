import Foundation

// ChatMessage is defined by UniFFI in zeroclaw_ios.swift.
// These extensions add SwiftUI convenience on top of the generated type.

extension ChatMessage: Identifiable {}

extension ChatMessage {
    var isUser: Bool {
        role == "user"
    }

    var timestamp: Date {
        Date(timeIntervalSince1970: TimeInterval(timestampMs) / 1000.0)
    }
}
